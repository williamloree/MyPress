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

interface VaultStatus {
  mode: "none" | "open" | "sealed";
  unlocked: boolean;
  auto_lock: number;
  auto_lock_choices: number[];
  min_passphrase: number;
}

interface VaultSummary {
  id: string;
  name: string;
  url: string;
  username: string;
  has_password: boolean;
  updated_at: number;
}

interface VaultFields {
  name: string;
  url: string;
  username: string;
  password: string;
  notes: string;
}

interface VaultEntry {
  id: string;
  updated_at: number;
  device_name: string;
  fields: VaultFields;
}

type View = "history" | "settings" | "share" | "vault";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

let items: Item[] = [];
let selected = 0;
let state: AppState;
let view: View = "history";
let share: ShareState | undefined;
let sending = false;
let vault: VaultStatus | undefined;

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
const KIND_ICON = { text: "text", url: "link", code: "code", image: "image" } as const;

/** Raccourci affiché en pastilles, ex. kbd("Ctrl", "1"). */
function kbd(...keys: string[]): HTMLSpanElement {
  const span = document.createElement("span");
  span.className = "keys";
  for (const k of keys) {
    const el = document.createElement("kbd");
    el.textContent = k;
    span.append(el);
  }
  return span;
}

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

      const kind = document.createElement("span");
      kind.className = "kind";
      kind.append(icon(KIND_ICON[it.kind]));
      const body = document.createElement("div");
      body.className = "body";
      body.append(text, meta);

      // Actions au survol, puis raccourci : ↵ pour la ligne choisie, Ctrl+1…9 pour les autres.
      const side = document.createElement("span");
      side.className = "side";
      const actions = document.createElement("span");
      actions.className = "actions";
      actions.append(
        iconButton("copy", "Copier sans coller", () => copyItem(it)),
        iconButton("trash", "Supprimer (Suppr)", () => call("delete_item", { id: it.id })),
      );
      side.append(actions);
      if (i === selected) side.append(kbd("↵"));
      else if (i < 9) side.append(kbd("Ctrl", `${i + 1}`));

      li.append(kind, body, side);
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
    if (view === "vault") vaultBack();
    else if (view !== "history") showView("history");
    else invoke("hide_window");
    return;
  }
  if (view === "vault" && vaultKey(e)) e.preventDefault();
  if (view !== "history") return;
  const it = items[selected];
  if (e.key === "ArrowDown") { selected = Math.min(selected + 1, items.length - 1); render(); e.preventDefault(); }
  else if (e.key === "ArrowUp") { selected = Math.max(selected - 1, 0); render(); e.preventDefault(); }
  else if (e.key === "Enter" && it) { e.preventDefault(); paste(it); }
  else if (e.ctrlKey && /^[1-9]$/.test(e.key) && items[Number(e.key) - 1]) { e.preventDefault(); paste(items[Number(e.key) - 1]); }
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

// ---------- coffre de mots de passe ----------

type Pane = "list" | "detail" | "edit" | "settings";
let pane: Pane = "list";
/** Entrée affichée ou modifiée ; `null` pour une nouvelle entrée. */
let current: string | null = null;
let vaultItems: VaultSummary[] = [];
let vaultSel = 0;

const AVATAR_COLORS = ["#2f6fea", "#2a9d5c", "#d9480f", "#7c3aed", "#c2255c", "#0b7285", "#e67700", "#5c7cfa"];

function avatar(name: string, big = false): HTMLSpanElement {
  const s = document.createElement("span");
  s.className = big ? "avatar big" : "avatar";
  const letter = name.trim().replace(/^https?:\/\/(www\.)?/i, "")[0] ?? "?";
  s.textContent = letter;
  let h = 0;
  for (const c of name.toLowerCase()) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  s.style.background = AVATAR_COLORS[h % AVATAR_COLORS.length];
  return s;
}

/** Domaine lisible d'une adresse. */
function host(url: string): string {
  try {
    return new URL(/^[a-z]+:\/\//i.test(url) ? url : `https://${url}`).hostname.replace(/^www\./, "");
  } catch {
    return url;
  }
}

// --- générateur et robustesse

const LETTERS = "ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const DIGITS = "23456789";
const SYMBOLS = "-_.!?@#$%&*+=";

/** Mot de passe aléatoire (tirage sans biais), avec au moins un caractère de chaque famille demandée. */
function generatePassword(length: number, digits: boolean, symbols: boolean): string {
  const sets = [LETTERS, ...(digits ? [DIGITS] : []), ...(symbols ? [SYMBOLS] : [])];
  const chars = sets.join("");
  const pick = (alphabet: string) => {
    const max = 256 - (256 % alphabet.length);
    for (;;) {
      const b = crypto.getRandomValues(new Uint8Array(1))[0];
      if (b < max) return alphabet[b % alphabet.length];
    }
  };
  for (;;) {
    const pw = Array.from({ length }, () => pick(chars)).join("");
    if (sets.every((set) => [...pw].some((c) => set.includes(c)))) return pw;
  }
}

function generated(): string {
  return generatePassword(
    Number($<HTMLInputElement>("vf-len").value),
    $<HTMLInputElement>("vf-digits").checked,
    $<HTMLInputElement>("vf-symbols").checked,
  );
}

/** Estimation grossière : longueur × familles de caractères, pénalisée par les répétitions. */
function strength(pw: string): { score: number; label: string } {
  if (!pw) return { score: 0, label: "" };
  let pool = 0;
  if (/[a-z]/.test(pw)) pool += 26;
  if (/[A-Z]/.test(pw)) pool += 26;
  if (/[0-9]/.test(pw)) pool += 10;
  if (/[^a-zA-Z0-9]/.test(pw)) pool += 30;
  const bits = new Set(pw).size / pw.length > 0.5 ? pw.length * Math.log2(pool) : pw.length * 2;
  if (bits < 40) return { score: 1, label: "Faible : facile à deviner" };
  if (bits < 64) return { score: 2, label: "Moyen" };
  if (bits < 90) return { score: 3, label: "Fort" };
  return { score: 4, label: "Excellent" };
}

const STRENGTH_COLORS = ["var(--line)", "var(--danger)", "var(--warn)", "var(--ok)", "var(--ok)"];

function showStrength(pw: string, bar: string, label?: string) {
  const s = strength(pw);
  const el = $(bar);
  el.style.width = `${s.score * 25}%`;
  el.style.background = STRENGTH_COLORS[s.score];
  if (label) $(label).textContent = s.label;
}

// Boutons « œil » : affichent ou masquent le champ indiqué.
document.querySelectorAll<HTMLButtonElement>("button.eye").forEach((b) => {
  b.onclick = () => {
    const input = $<HTMLInputElement>(b.dataset.toggle!);
    input.type = input.type === "password" ? "text" : "password";
    b.classList.toggle("on", input.type === "text");
  };
});

function hideField(id: string) {
  $<HTMLInputElement>(id).type = "password";
  document.querySelector(`button.eye[data-toggle="${id}"]`)?.classList.remove("on");
}

// --- presse-papiers

let clipTimer: ReturnType<typeof setInterval> | undefined;

async function vaultCopy(id: string, field: "username" | "password" | "url") {
  if ((await call("vault_copy", { id, field })) === undefined) return;
  const label = { username: "Identifiant copié", password: "Mot de passe copié", url: "Adresse copiée" }[field];
  const badge = $("vault-clip");
  let left = 30;
  const tick = () => {
    badge.replaceChildren(icon("copy"), `${label} · effacé dans ${left} s`);
    if (left-- <= 0) { clearInterval(clipTimer); badge.hidden = true; }
  };
  clearInterval(clipTimer);
  badge.hidden = false;
  tick();
  clipTimer = setInterval(tick, 1000);
}

// --- écrans

const TITLES: Record<Pane, string> = { list: "Coffre", detail: "", edit: "", settings: "Sécurité du coffre" };

async function loadVault() {
  vault = await call<VaultStatus>("vault_status");
  if (vault && view === "vault") renderVault();
}

function setPane(p: Pane) {
  pane = p;
  renderVault();
}

function renderVault() {
  const v = vault!;
  const locked = !v.unlocked;
  if (locked) pane = "list";
  $("vault-unlock").hidden = !locked;
  $("vault-list-pane").hidden = locked || pane !== "list";
  $("vault-detail").hidden = locked || pane !== "detail";
  $("vault-form").hidden = locked || pane !== "edit";
  $("vault-settings").hidden = locked || pane !== "settings";
  $("vault-lock").hidden = v.mode !== "sealed" || locked;
  $("vault-back").title = pane === "list" || locked ? "Retour à l'historique" : "Retour à la liste";
  $("vault-title").textContent =
    locked ? "Coffre" : pane === "edit" ? (current ? "Modifier l'entrée" : "Nouvelle entrée") : TITLES[pane] || "Coffre";
  if (locked) {
    $<HTMLInputElement>("vault-passphrase").focus();
    return;
  }
  if (pane === "list") renderVaultList();
  if (pane === "settings") renderVaultSettings();
}

async function renderVaultList() {
  const v = vault!;
  const mode = $("vault-mode");
  mode.className = `status ${v.mode === "sealed" ? "online" : "paused"}`;
  mode.querySelector("span")!.textContent = v.mode === "sealed" ? "Protégé par phrase de passe" : "Sans phrase de passe";
  mode.title = v.mode === "sealed" ? "" : "Définissez une phrase de passe dans ⚙";

  const query = $<HTMLInputElement>("vault-search").value;
  vaultItems = (await call<VaultSummary[]>("vault_list", { query })) ?? [];
  vaultSel = Math.min(vaultSel, Math.max(vaultItems.length - 1, 0));
  drawVaultList();
  $("vault-empty").hidden = vaultItems.length > 0;
  $("vault-list").hidden = vaultItems.length === 0;
  $("vault-empty-text").textContent = query
    ? `Aucune entrée ne correspond à « ${query} ».`
    : "Votre coffre est vide. Enregistrez vos mots de passe : ils seront chiffrés et disponibles sur tous vos appareils.";
  $("vault-empty-add").hidden = !!query;
}

function drawVaultList() {
  $("vault-list").replaceChildren(
    ...vaultItems.map((e, i) => {
      const li = document.createElement("li");
      li.className = i === vaultSel ? "sel" : "";
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === vaultSel));
      const info = document.createElement("div");
      info.className = "info";
      const b = document.createElement("b");
      b.textContent = e.name;
      const small = document.createElement("small");
      small.textContent = [e.username, e.url && host(e.url)].filter(Boolean).join(" · ") || "—";
      info.append(b, small);
      const quick = document.createElement("span");
      quick.className = "quick";
      if (e.username) quick.append(iconButton("user", "Copier l'identifiant (Ctrl+B)", () => vaultCopy(e.id, "username")));
      if (e.has_password) quick.append(iconButton("key", "Copier le mot de passe (Ctrl+C)", () => vaultCopy(e.id, "password")));
      li.append(avatar(e.name), info, quick);
      li.onmousemove = () => { if (vaultSel !== i) { vaultSel = i; drawVaultList(); } };
      li.onclick = (ev) => { if (!(ev.target as HTMLElement).closest("button")) openDetail(e.id); };
      return li;
    }),
  );
  $("vault-list").querySelector(".sel")?.scrollIntoView({ block: "nearest" });
}

/** Icône du sprite SVG de index.html. */
function icon(name: string): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("class", "ico");
  const use = document.createElementNS("http://www.w3.org/2000/svg", "use");
  use.setAttribute("href", `#i-${name}`);
  svg.append(use);
  return svg;
}

function iconButton(name: string, title: string, onclick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.append(icon(name));
  b.title = title;
  b.setAttribute("aria-label", title);
  b.onclick = (e) => { e.stopPropagation(); onclick(); };
  return b;
}

async function openDetail(id: string) {
  const e = await call<VaultEntry>("vault_entry", { id });
  if (!e) return;
  current = id;
  const f = e.fields;
  $("vd-avatar").replaceWith(Object.assign(avatar(f.name, true), { id: "vd-avatar" }));
  $("vd-name").textContent = f.name;
  $("vd-meta").textContent = `Modifié ${ago(e.updated_at)} · ${e.device_name}`;

  const card = (label: string, value: HTMLElement | string, ...buttons: HTMLButtonElement[]) => {
    const div = document.createElement("div");
    div.className = "card";
    const body = document.createElement("div");
    const small = document.createElement("small");
    small.textContent = label;
    const span = document.createElement("span");
    if (typeof value === "string") span.textContent = value; else span.append(value);
    body.append(small, span);
    div.append(body, ...buttons);
    return div;
  };
  const cards: HTMLElement[] = [];
  if (f.url) {
    cards.push(card("Adresse", f.url, iconButton("copy", "Copier l'adresse", () => vaultCopy(id, "url"))));
  }
  if (f.username) {
    cards.push(card("Identifiant", f.username, iconButton("copy", "Copier l'identifiant", () => vaultCopy(id, "username"))));
  }
  if (f.password) {
    const secret = document.createElement("span");
    secret.className = "secret";
    const mask = "•".repeat(Math.min(f.password.length, 16));
    secret.textContent = mask;
    const eye = iconButton("eye", "Afficher", () => {
      const shown = secret.textContent !== mask;
      secret.textContent = shown ? mask : f.password;
      eye.title = shown ? "Afficher" : "Masquer";
      eye.classList.toggle("on", !shown);
    });
    const c = card("Mot de passe", secret, eye, iconButton("copy", "Copier le mot de passe", () => vaultCopy(id, "password")));
    const meter = document.createElement("small");
    meter.textContent = strength(f.password).label;
    meter.style.color = STRENGTH_COLORS[strength(f.password).score];
    c.querySelector("div")!.append(meter);
    cards.push(c);
  }
  if (f.notes) {
    const c = card("Notes", f.notes);
    c.classList.add("notes");
    cards.push(c);
  }
  $("vd-fields").replaceChildren(...cards);
  setPane("detail");
}

function openForm(e?: VaultEntry) {
  current = e?.id ?? null;
  const f = e?.fields ?? { name: "", url: "", username: "", password: generated(), notes: "" };
  $<HTMLInputElement>("vf-name").value = f.name;
  $<HTMLInputElement>("vf-url").value = f.url;
  $<HTMLInputElement>("vf-username").value = f.username;
  $<HTMLInputElement>("vf-password").value = f.password;
  $<HTMLTextAreaElement>("vf-notes").value = f.notes;
  hideField("vf-password");
  ($("vf-gen") as HTMLDetailsElement).open = !e;
  $("vf-error").hidden = true;
  showStrength(f.password, "vf-strength-bar", "vf-strength");
  setPane("edit");
  $<HTMLInputElement>("vf-name").focus();
}

function renderVaultSettings() {
  const v = vault!;
  const sealed = v.mode === "sealed";
  $("vs-mode").textContent = sealed
    ? "Le coffre est protégé par une phrase de passe, commune à tous vos appareils."
    : "Sans phrase de passe, le coffre s'ouvre avec la session de cet ordinateur. Une phrase de passe le protège même si quelqu'un utilise votre session.";
  $("vs-pass-title").textContent = sealed ? "Changer la phrase de passe" : "Définir une phrase de passe";
  $<HTMLInputElement>("vault-new-pass").placeholder = `Nouvelle phrase de passe (${v.min_passphrase} caractères minimum)`;
  $("vs-sealed-only").hidden = !sealed;
  $<HTMLSelectElement>("vault-auto-lock").replaceChildren(
    ...v.auto_lock_choices.map((n) => new Option(n === 0 ? "jamais" : n === 60 ? "1 heure" : `${n} min`, `${n}`, false, n === v.auto_lock)),
  );
}

/** Retour : de la fiche, du formulaire ou des réglages vers la liste ; de la liste vers l'historique. */
function vaultBack() {
  if (!vault?.unlocked || pane === "list") return showView("history");
  if (pane === "edit" && current) return openDetail(current);
  setPane("list");
  $<HTMLInputElement>("vault-search").focus();
}

/** Raccourcis de l'onglet ; `true` si la touche a été traitée. */
function vaultKey(e: KeyboardEvent): boolean {
  if (!vault?.unlocked) return false;
  const ctrl = e.ctrlKey || e.metaKey;
  if (pane === "edit" && ctrl && e.key.toLowerCase() === "s") {
    $<HTMLFormElement>("vault-form").requestSubmit();
    return true;
  }
  if (pane !== "list") return false;
  if (ctrl && e.key.toLowerCase() === "n") { openForm(); return true; }
  const it = vaultItems[vaultSel];
  if (e.key === "ArrowDown") { vaultSel = Math.min(vaultSel + 1, vaultItems.length - 1); drawVaultList(); return true; }
  if (e.key === "ArrowUp") { vaultSel = Math.max(vaultSel - 1, 0); drawVaultList(); return true; }
  if (!it) return false;
  if (e.key === "Enter") { openDetail(it.id); return true; }
  // Ctrl+C sans texte sélectionné dans la recherche : copie le mot de passe.
  const search = $<HTMLInputElement>("vault-search");
  const selecting = document.activeElement === search && search.selectionStart !== search.selectionEnd;
  if (ctrl && e.key.toLowerCase() === "c" && !selecting && it.has_password) { vaultCopy(it.id, "password"); return true; }
  if (ctrl && e.key.toLowerCase() === "b" && it.username) { vaultCopy(it.id, "username"); return true; }
  return false;
}

$<HTMLFormElement>("vault-unlock").addEventListener("submit", async (e) => {
  e.preventDefault();
  const input = $<HTMLInputElement>("vault-passphrase");
  const btn = $<HTMLButtonElement>("vault-unlock-btn");
  const err = $("vault-unlock-error");
  if (!input.value) return input.focus();
  err.hidden = true;
  btn.disabled = true;
  btn.textContent = "Déverrouillage…";
  try {
    await invoke("vault_unlock", { passphrase: input.value });
    input.value = "";
    hideField("vault-passphrase");
    await loadVault();
    $<HTMLInputElement>("vault-search").focus();
  } catch (e) {
    err.textContent = String(e).replace(/^./, (c) => c.toUpperCase());
    err.hidden = false;
    input.select();
  } finally {
    btn.disabled = false;
    btn.textContent = "Déverrouiller";
  }
});

$<HTMLFormElement>("vault-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const fields: VaultFields = {
    name: $<HTMLInputElement>("vf-name").value,
    url: $<HTMLInputElement>("vf-url").value,
    username: $<HTMLInputElement>("vf-username").value,
    password: $<HTMLInputElement>("vf-password").value,
    notes: $<HTMLTextAreaElement>("vf-notes").value,
  };
  const err = $("vf-error");
  try {
    const id = await invoke<string>("vault_save", { id: current, fields });
    toast(current ? "Modifications enregistrées" : "Entrée ajoutée");
    openDetail(id);
  } catch (e) {
    err.textContent = String(e).replace(/^./, (c) => c.toUpperCase());
    err.hidden = false;
    if (!fields.name.trim()) $<HTMLInputElement>("vf-name").focus();
  }
});

$("vf-password").addEventListener("input", (e) => showStrength((e.target as HTMLInputElement).value, "vf-strength-bar", "vf-strength"));
$("vf-len").addEventListener("input", (e) => {
  $("vf-len-value").textContent = (e.target as HTMLInputElement).value;
  $("vf-generate").click();
});
for (const id of ["vf-digits", "vf-symbols"]) $(id).addEventListener("change", () => $("vf-generate").click());
$("vf-generate").onclick = () => {
  const input = $<HTMLInputElement>("vf-password");
  input.value = generated();
  input.type = "text";
  document.querySelector('button.eye[data-toggle="vf-password"]')?.classList.add("on");
  showStrength(input.value, "vf-strength-bar", "vf-strength");
};
$("vf-cancel").onclick = vaultBack;

$("vd-edit").onclick = async () => {
  const e = current ? await call<VaultEntry>("vault_entry", { id: current }) : undefined;
  if (e) openForm(e);
};
$("vd-delete").onclick = async (e) => {
  if (!current || !confirmTwice(e.currentTarget as HTMLButtonElement)) return;
  if ((await call("vault_delete", { id: current })) !== undefined) {
    toast("Entrée supprimée sur tous vos appareils");
    current = null;
    setPane("list");
  }
};

$("vault-new").onclick = () => openForm();
$("vault-empty-add").onclick = () => openForm();
$("vault-search").addEventListener("input", () => { vaultSel = 0; renderVaultList(); });
$("vault-lock").onclick = () => call("vault_lock");
$("vault-open-settings").onclick = () => setPane("settings");
$("vault-back").onclick = vaultBack;
$("vault-close").onclick = () => invoke("hide_window");

$("vault-new-pass").addEventListener("input", (e) => showStrength((e.target as HTMLInputElement).value, "vs-strength-bar"));
$<HTMLFormElement>("vault-pass-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const a = $<HTMLInputElement>("vault-new-pass");
  const b = $<HTMLInputElement>("vault-new-pass2");
  if (a.value.length < vault!.min_passphrase) return toast(`${vault!.min_passphrase} caractères minimum`);
  if (a.value !== b.value) return toast("Les deux saisies diffèrent");
  const btn = $<HTMLButtonElement>("vault-pass-submit");
  btn.disabled = true;
  btn.textContent = "Chiffrement…";
  const ok = (await call("vault_set_passphrase", { passphrase: a.value })) !== undefined;
  btn.disabled = false;
  btn.textContent = "Enregistrer";
  if (ok) {
    a.value = b.value = "";
    showStrength("", "vs-strength-bar");
    toast("Phrase de passe enregistrée pour tous vos appareils");
    loadVault();
  }
});

$("vault-remove-pass").onclick = async (e) => {
  if (!confirmTwice(e.currentTarget as HTMLButtonElement)) return;
  if ((await call("vault_set_passphrase", { passphrase: null })) !== undefined) {
    toast("Phrase de passe retirée");
    loadVault();
  }
};

$<HTMLSelectElement>("vault-auto-lock").onchange = async (e) => {
  await call("vault_set_auto_lock", { minutes: Number((e.target as HTMLSelectElement).value) });
};
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
  $("vault").hidden = v !== "vault";
  if (v === "vault") {
    pane = "list";
    current = null;
    vaultSel = 0;
    $<HTMLInputElement>("vault-search").value = "";
    $("vault-unlock-error").hidden = true;
    loadVault().then(() => { if (vault?.unlocked) $<HTMLInputElement>("vault-search").focus(); });
  } else if (v === "history") {
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
$("open-vault").onclick = () => showView("vault");
$("share-back").onclick = () => showView("history");
$("share-close").onclick = () => invoke("hide_window");
$("back").onclick = () => showView("history");
$("close").onclick = () => invoke("hide_window");

listen<string>("show-view", (e) => { loadState(); showView(e.payload as View); });
listen("shares-changed", () => loadShare());
// Fiche ou formulaire ouverts : on ne les quitte que si le coffre vient d'être verrouillé.
listen("vault-changed", async () => {
  if (view !== "vault") return;
  const s = await call<VaultStatus>("vault_status");
  if (!s) return;
  vault = s;
  if (pane === "list" || pane === "settings" || !s.unlocked) renderVault();
});
listen("history-changed", () => { if (view === "history") refresh(); });
listen("status-changed", () => loadState());

loadState().then(() => showView("history"));
