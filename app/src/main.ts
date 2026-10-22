import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Item {
  id: string;
  created_at: number;
  device_id: string;
  device_name: string;
  kind: "text" | "url" | "code";
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
}

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let items: Item[] = [];
let selected = 0;
let state: AppState;
let view: "history" | "settings" = "history";

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

const KIND_LABEL = { text: "Texte", url: "URL", code: "Code" } as const;

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

      const text = document.createElement("div");
      text.className = "text";
      text.textContent = it.text.length > 600 ? it.text.slice(0, 600) + "…" : it.text;

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
    if (view === "settings") showView("history");
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
}

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

function showView(v: "history" | "settings") {
  view = v;
  invoke("set_view", { name: v });
  $("history").hidden = v !== "history";
  $("settings").hidden = v !== "settings";
  if (v === "history") {
    const search = $<HTMLInputElement>("search");
    search.value = "";
    selected = 0;
    refresh();
    search.focus();
  } else {
    $("invite-box").hidden = true;
    $("join-error").hidden = true;
    renderSettings();
  }
}

$("open-settings").onclick = () => showView("settings");
$("back").onclick = () => showView("history");
$("close").onclick = () => invoke("hide_window");

listen<string>("show-view", (e) => { loadState(); showView(e.payload as "history" | "settings"); });
listen("history-changed", () => { if (view === "history") refresh(); });
listen("status-changed", () => loadState());

loadState().then(() => showView("history"));
