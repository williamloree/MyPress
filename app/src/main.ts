import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";

interface Item {
  id: string;
  created_at: number;
  device_id: string;
  device_name: string;
  kind: "text" | "url" | "code" | "image";
  /** Pour une image : aperçu PNG en base64. */
  text: string;
}

interface DeviceView {
  id: string;
  name: string;
  current: boolean;
  online: boolean;
}

interface AppState {
  status: "solo" | "online" | "offline" | "paused";
  status_label: string;
  notice: string | null;
  device_id: string;
  device_name: string;
  devices: DeviceView[];
  mailbox: string | null;
  mailbox_status: { state: "disabled" | "connecting" | "online" | "error"; detail?: string };
  history_limit: number;
  history_limits: number[];
  capture_paused: boolean;
  sync_paused: boolean;
  autostart: boolean;
  version: string;
  update: {
    state: "" | "idle" | "checking" | "up_to_date" | "installing" | "error";
    detail: string | null;
    checked_at: number | null;
  };
}

interface ShareGroup {
  id: string;
  name: string;
  members: { id: string; name: string }[];
}

interface Transfer {
  created_at: number;
  incoming: boolean;
  group: string;
  peer: string;
  name: string;
  size: number;
  path: string;
}

interface ShareState {
  groups: ShareGroup[];
  transfers: Transfer[];
  dir: string;
  own_devices: number;
}

type View = "history" | "settings" | "share";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let items: Item[] = [];
let selected = 0;
let state: AppState;
let view: View = "history";
let share: ShareState | undefined;
let sending = false;

// ---------- utilitaires ----------

function toast(msg: string) {
  const t = $("toast");
  t.textContent = msg;
  t.hidden = false;
  clearTimeout((toast as any).timer);
  (toast as any).timer = setTimeout(() => (t.hidden = true), 2200);
}

function ago(ms: number): string {
  const s = Math.round((Date.now() - ms) / 1000);
  if (s < 60) return "à l'instant";
  if (s < 3600) return `il y a ${Math.floor(s / 60)} min`;
  const d = new Date(ms);
  const time = d.toLocaleTimeString("fr-FR", { hour: "2-digit", minute: "2-digit" });
  if (s < 86400 && d.getDate() === new Date().getDate()) return time;
  return `${d.toLocaleDateString("fr-FR", { day: "numeric", month: "short" })} ${time}`;
}

const KIND_LABEL = { text: "Texte", url: "URL", code: "Code", image: "Image" } as const;

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T | undefined> {
  try {
    return await invoke<T>(cmd, args);
  } catch (e) {
    toast(String(e));
    return undefined;
  }
}

/** Confirmation sans boîte de dialogue : un second clic valide. */
function confirmTwice(b: HTMLButtonElement): boolean {
  if (b.dataset.armed) return true;
  const label = b.textContent;
  b.dataset.armed = "1";
  b.textContent = "Confirmer ?";
  setTimeout(() => { delete b.dataset.armed; b.textContent = label; }, 3000);
  return false;
}

// ---------- historique ----------

async function refresh() {
  const query = $<HTMLInputElement>("search").value;
  items = (await call<Item[]>("search_history", { query })) ?? [];
  selected = Math.min(selected, Math.max(items.length - 1, 0));
  render();
}

function render() {
  const list = $("list");
  list.replaceChildren(
    ...items.map((it, i) => {
      const li = document.createElement("li");
      li.className = `${it.kind}${i === selected ? " sel" : ""}`;
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === selected));

      let text: HTMLElement;
      if (it.kind === "image") {
        const img = document.createElement("img");
        img.src = `data:image/png;base64,${it.text}`;
        img.alt = "Image copiée";
        text = img;
      } else {
        text = document.createElement("div");
        text.textContent = it.text.length > 600 ? it.text.slice(0, 600) + "…" : it.text;
      }
      text.className = "text";

      const meta = document.createElement("span");
      meta.className = "meta";
      const mine = it.device_id === state?.device_id;
      meta.textContent = `${KIND_LABEL[it.kind]} · ${ago(it.created_at)} · ${mine ? "cet appareil" : it.device_name}`;

      const actions = document.createElement("span");
      actions.className = "actions";
      const copy = document.createElement("button");
      copy.textContent = "Copier";
      copy.onclick = (e) => { e.stopPropagation(); copyItem(it); };
      const del = document.createElement("button");
      del.textContent = "Supprimer";
      del.onclick = (e) => { e.stopPropagation(); call("delete_item", { id: it.id }); };
      actions.append(copy, del);

      li.append(text, meta, actions);
      li.onmousemove = () => { if (selected !== i) { selected = i; render(); } };
      li.onclick = () => paste(it);
      return li;
    }),
  );
  $("empty").hidden = items.length > 0;
  list.querySelector(".sel")?.scrollIntoView({ block: "nearest" });
}

async function paste(it: Item) {
  const pasted = await call<boolean>("paste_item", { id: it.id });
  if (pasted === false) toast("Copié : collez avec Ctrl+V");
}

async function copyItem(it: Item) {
  await call("copy_item", { id: it.id });
  toast("Copié");
}

$("search").addEventListener("input", () => { selected = 0; refresh(); });

document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    e.preventDefault();
    if (view !== "history") showView("history");
    else invoke("hide_window");
    return;
  }
  if (view !== "history") return;
  const it = items[selected];
  if (e.key === "ArrowDown") { selected = Math.min(selected + 1, items.length - 1); render(); e.preventDefault(); }
  else if (e.key === "ArrowUp") { selected = Math.max(selected - 1, 0); render(); e.preventDefault(); }
  else if (e.key === "Enter" && it) { e.preventDefault(); paste(it); }
  else if (e.key === "Delete" && it && !$<HTMLInputElement>("search").value) { e.preventDefault(); call("delete_item", { id: it.id }); }
});

// ---------- fichiers et groupes de partage ----------

function size(n: number): string {
  if (n < 1024) return `${n} o`;
  if (n < 1024 ** 2) return `${(n / 1024).toFixed(0)} Ko`;
  if (n < 1024 ** 3) return `${(n / 1024 ** 2).toFixed(1)} Mo`;
  return `${(n / 1024 ** 3).toFixed(2)} Go`;
}

function entry(title: string, detail: string, ...buttons: HTMLButtonElement[]): HTMLLIElement {
  const li = document.createElement("li");
  const div = document.createElement("div");
  const b = document.createElement("b");
  b.textContent = title;
  b.title = title;
  const small = document.createElement("small");
  small.textContent = detail;
  div.append(b, small);
  li.append(div, ...buttons);
  return li;
}

function button(label: string, onclick: (b: HTMLButtonElement) => void, cls = ""): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  b.className = cls;
  b.onclick = () => onclick(b);
  return b;
}

async function loadShare() {
  share = await call<ShareState>("share_state");
  if (share && view === "share") renderShare();
}

function renderShare() {
  const s = share!;
  const target = $<HTMLSelectElement>("target");
  const current = target.value || "me";
  target.replaceChildren(
    new Option(`Mes appareils (${s.own_devices})`, "me"),
    ...s.groups.map((g) => new Option(`${g.name} (${g.members.length - 1})`, g.id)),
  );
  target.value = [...target.options].some((o) => o.value === current) ? current : "me";
  $("received-dir").textContent = s.dir;

  $("transfers").replaceChildren(
    ...s.transfers.map((t) =>
      entry(
        t.name,
        `${t.incoming ? "Reçu de" : "Envoyé à"} ${t.peer} · ${t.group} · ${size(t.size)} · ${ago(t.created_at)}`,
        button("Afficher", () => call("reveal_file", { path: t.path })),
      ),
    ),
  );
  $("no-transfer").hidden = s.transfers.length > 0;

  $("share-groups").replaceChildren(
    ...s.groups.map((g) =>
      entry(
        g.name,
        g.members.map((m) => (m.id === state?.device_id ? "vous" : m.name)).join(", "),
        button("Inviter", async () => {
          const code = await call<string>("share_invite", { group: g.id });
          if (!code) return;
          $<HTMLTextAreaElement>("share-invite-code").value = code;
          $("share-invite-box").hidden = false;
        }),
        button("Quitter", (b) => { if (confirmTwice(b)) call("leave_share_group", { group: g.id }); }, "danger"),
      ),
    ),
  );
}

async function sendFiles(paths: string[]) {
  if (sending || !paths.length) return;
  const drop = $("drop");
  sending = true;
  drop.classList.add("busy");
  drop.textContent = `Envoi de ${paths.length} fichier${paths.length > 1 ? "s" : ""}…`;
  const res = await call<[number, number]>("send_files", { target: $<HTMLSelectElement>("target").value, paths });
  sending = false;
  drop.classList.remove("busy");
  drop.textContent = "Glissez des fichiers ici";
  if (res) toast(res[0] === res[1] ? "Envoyé" : `Envoyé à ${res[0]} sur ${res[1]} : les autres sont injoignables`);
}

// Un fichier glissé sur la fenêtre ouvre la vue d'envoi ; le déposer l'envoie.
getCurrentWebview().onDragDropEvent((e) => {
  const drop = $("drop");
  if (e.payload.type === "enter") {
    if (view !== "share") showView("share");
    drop.classList.add("over");
  } else if (e.payload.type === "leave") {
    drop.classList.remove("over");
  } else if (e.payload.type === "drop") {
    drop.classList.remove("over");
    sendFiles(e.payload.paths);
  }
});

$("copy-share-invite").onclick = async () => {
  await call("copy_text", { text: $<HTMLTextAreaElement>("share-invite-code").value });
  toast("Code copié");
};

$<HTMLFormElement>("create-group").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $<HTMLInputElement>("group-name");
  if (await call("create_share_group", { name: input.value })) {
    input.value = "";
    toast("Groupe créé : invitez des membres");
  }
});

$<HTMLFormElement>("join-group").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $<HTMLInputElement>("join-group-code");
  const g = await call<ShareGroup>("join_share_group", { code: input.value });
  if (g) {
    input.value = "";
    ($("join-group-box") as HTMLDetailsElement).open = false;
    toast(`Vous avez rejoint « ${g.name} »`);
  }
});

// ---------- état et paramètres ----------

async function loadState() {
  state = (await call<AppState>("get_state"))!;
  const st = $("status");
  st.className = `status ${state.capture_paused ? "private" : state.status}`;
  st.querySelector("span")!.textContent = state.capture_paused ? "Mode privé" : state.status_label;
  st.title = state.status_label;
  if (view === "settings") renderSettings();
}

const MAILBOX_LABEL = {
  disabled: "",
  connecting: "Connexion…",
  online: "Connectée",
  error: "Injoignable",
} as const;

function renderSettings() {
  const notice = $("notice");
  notice.textContent = state.notice ?? "";
  notice.hidden = !state.notice;
  $("sync-status").textContent = state.status_label;

  $("devices").replaceChildren(
    ...state.devices.map((d) => {
      const li = document.createElement("li");
      const dot = document.createElement("i");
      dot.className = d.current || d.online ? "on" : "";
      dot.title = d.current ? "" : d.online ? "Connecté" : "Non joignable";
      const label = document.createElement("span");
      label.textContent = d.current ? `${d.name} (cet appareil)` : d.name;
      li.append(dot, label);
      if (!d.current) {
        const b = document.createElement("button");
        b.textContent = "Retirer";
        b.className = "danger";
        b.title = "Retire l'appareil et change la clé du groupe";
        b.onclick = async () => {
          if (!confirmTwice(b)) return;
          await call("revoke_device", { id: d.id });
          toast(`${d.name} a été retiré`);
        };
        li.append(b);
      }
      return li;
    }),
  );
  $("leave").hidden = state.devices.length < 2;

  const url = $<HTMLInputElement>("mailbox-url");
  if (document.activeElement !== url) url.value = state.mailbox ?? "";
  const ms = state.mailbox_status;
  $("mailbox-status").textContent = state.mailbox
    ? `${MAILBOX_LABEL[ms.state]}${ms.detail ? ` : ${ms.detail}` : ""}`
    : "";

  $<HTMLSelectElement>("limit").replaceChildren(
    ...state.history_limits.map((n) => new Option(`${n}`, `${n}`, false, n === state.history_limit)),
  );
  $<HTMLInputElement>("autostart").checked = state.autostart;

  const u = state.update;
  const label = {
    "": "première vérification dans quelques secondes",
    idle: "première vérification dans quelques secondes",
    checking: "recherche en cours…",
    up_to_date: `à jour${u.checked_at ? ` (vérifié ${ago(u.checked_at)})` : ""}`,
    installing: `installation de la version ${u.detail}, redémarrage imminent…`,
    error: `vérification impossible : ${u.detail}`,
  }[u.state];
  $("update-status").textContent = `Version ${state.version} · ${label}`;
  $<HTMLButtonElement>("check-updates").disabled = u.state === "checking" || u.state === "installing";
}

$("check-updates").onclick = () => call("check_updates");

$("invite").onclick = async () => {
  const code = await call<string>("create_invite");
  if (!code) return;
  $<HTMLTextAreaElement>("invite-code").value = code;
  $("invite-box").hidden = false;
};

// Copié sans passer par l'historique synchronisé.
$("copy-invite").onclick = async () => {
  await call("copy_text", { text: $<HTMLTextAreaElement>("invite-code").value });
  toast("Code copié");
};

$("join").onclick = async (e) => {
  const b = e.currentTarget as HTMLButtonElement;
  const err = $("join-error");
  err.hidden = true;
  b.disabled = true;
  b.textContent = "Connexion…";
  try {
    await invoke("join_group", { code: $<HTMLInputElement>("join-code").value });
    $<HTMLInputElement>("join-code").value = "";
    ($("join-box") as HTMLDetailsElement).open = false;
    toast("Appareil associé");
    loadState();
  } catch (e) {
    err.textContent = String(e);
    err.hidden = false;
  } finally {
    b.disabled = false;
    b.textContent = "Rejoindre";
  }
};

$("leave").onclick = async (e) => {
  if (!confirmTwice(e.currentTarget as HTMLButtonElement)) return;
  await call("leave_group");
  loadState();
};

$<HTMLFormElement>("mailbox-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const ok = await call("set_mailbox", { url: $<HTMLInputElement>("mailbox-url").value });
  if (ok !== undefined || !$<HTMLInputElement>("mailbox-url").value) toast("Enregistré pour tous les appareils");
  ($<HTMLInputElement>("mailbox-url")).blur();
  loadState();
});

$<HTMLSelectElement>("limit").onchange = async (e) => {
  const limit = Number((e.target as HTMLSelectElement).value);
  if (limit < state.history_limit) toast(`Seuls les ${limit} éléments les plus récents seront conservés.`);
  await call("set_history_limit", { limit });
  loadState();
};

$("clear").onclick = async (e) => {
  if (!confirmTwice(e.currentTarget as HTMLButtonElement)) return;
  await call("clear_history");
  toast(state.devices.length > 1 ? "Historique vidé sur tous les appareils" : "Historique vidé");
};

$<HTMLInputElement>("autostart").onchange = async (e) => {
  await call("set_autostart", { enabled: (e.target as HTMLInputElement).checked });
  loadState();
};

function showView(v: View) {
  view = v;
  invoke("set_view", { name: v });
  $("history").hidden = v !== "history";
  $("settings").hidden = v !== "settings";
  $("share").hidden = v !== "share";
  if (v === "history") {
    const search = $<HTMLInputElement>("search");
    search.value = "";
    selected = 0;
    refresh();
    search.focus();
  } else if (v === "share") {
    $("share-invite-box").hidden = true;
    loadShare();
  } else {
    $("invite-box").hidden = true;
    $("join-error").hidden = true;
    renderSettings();
  }
}

$("open-settings").onclick = () => showView("settings");
$("open-share").onclick = () => showView("share");
$("share-back").onclick = () => showView("history");
$("share-close").onclick = () => invoke("hide_window");
$("back").onclick = () => showView("history");
$("close").onclick = () => invoke("hide_window");

listen<string>("show-view", (e) => { loadState(); showView(e.payload as View); });
listen("shares-changed", () => loadShare());
listen("history-changed", () => { if (view === "history") refresh(); });
listen("status-changed", () => loadState());

loadState().then(() => showView("history"));
