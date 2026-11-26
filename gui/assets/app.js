// Mish.app — pestañas con paneles divididos de xterm.js, cada uno conectado a un PTY de Rust.
"use strict";

const THEME = {
  background: "#0c101a",
  foreground: "#fdf4e8",
  cursor: "#fdf4e8",
  cursorAccent: "#0c101a",
  selectionBackground: "#e8854c55",
  black: "#1b2130", brightBlack: "#5d6880",
  red: "#f2675f", brightRed: "#ff8a80",
  green: "#9dd08c", brightGreen: "#b8e6a8",
  yellow: "#ecc46e", brightYellow: "#f6d98f",
  blue: "#8ab8ea", brightBlue: "#a9cdf5",
  magenta: "#e6a2c4", brightMagenta: "#f0bdd8",
  cyan: "#76d3bf", brightCyan: "#9be6d6",
  white: "#d9d0c3", brightWhite: "#fdf4e8",
};

const BASE_BG = THEME.background;
const send = (o) => window.ipc.postMessage(JSON.stringify(o));
const dec = (b64) => Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
const utf8 = new TextDecoder();

const L = window.MishLayout; // layout.js: árbol de paneles (split/remove/leaves/neighbor…)
const tabsEl = document.getElementById("tabs");
const panesEl = document.getElementById("panes"); // #search es su primer hijo: no se toca
// Pestañas: tabId → {id, tabEl, viewEl, root, focused, title}. root es el árbol de layout.js
// con los sids de sus paneles; focused, el sid del panel enfocado.
const tabs = new Map();
// Paneles: sid → {id, tabId, term, fit, search, el, title, cwd, tint, bell, remote, replaying,
// claude, claudeDetail, pillEl}.
// El sid es el id de la sesión PTY en Rust (mensajes new/attach/in/resize/close/paste).
const panes = new Map();
let nextSid = 1;
let nextTabId = 1;
let active = null; // tabId activo
let fontSize = 13;
const MIN_PANE_PX = 80;
const BOOT_WAIT_MS = 1500; // sin mishBoot en este tiempo: arranque como antes (una pestaña nueva)
const REPLAY_MAX_MS = 5000; // red de seguridad si nunca llega mishReplayEnd
const SAVE_DEBOUNCE_MS = 300;
let booted = false; // hasta reconciliar con mishBoot (o el fallback) no se guarda el layout
let quitting = false; // tras el último save_layout de salida no se vuelve a guardar
let saveDisabled = false; // otra ventana tomó las sesiones (taken_over): no pisar su tabs.json
let takenOver = false; // idem; un mishBoot posterior (acción "reattach") vuelve a conectar

const div = (cls) => { const d = document.createElement("div"); d.className = cls; return d; };
const activeTab = () => tabs.get(active) || null;
const focusedPane = (tab = activeTab()) => (tab ? panes.get(tab.focused) || null : null);

// ---------- ajuste de tamaño: un ResizeObserver por .pane, con un fit por frame ----------

const pendingFit = new Set();
let fitFrame = 0;
function fitNow(p) {
  if (!p || !panes.has(p.id) || !p.el.isConnected || !p.el.clientWidth || !p.el.clientHeight) return;
  try { p.fit.fit(); } catch (_) { /* terminal sin renderer todavía */ }
}
function scheduleFit(p) {
  pendingFit.add(p);
  if (fitFrame) return;
  fitFrame = requestAnimationFrame(() => {
    fitFrame = 0;
    const list = [...pendingFit];
    pendingFit.clear();
    for (const q of list) fitNow(q);
  });
}
const resizeObs = new ResizeObserver((entries) => {
  for (const e of entries) { const p = panes.get(e.target._mishSid); if (p) scheduleFit(p); }
});
// Fuerza el fit de todos los paneles de la pestaña activa (vault.js lo usa al abrir/cerrar).
window.mishRefit = () => {
  const tab = activeTab();
  if (tab) for (const sid of L.leaves(tab.root)) fitNow(panes.get(sid));
};

// ---------- paneles ----------

// `id` explícito al restaurar (attach a una sesión viva); si no, el siguiente libre.
function createPane(tabId, id = nextSid++) {
  if (id >= nextSid) nextSid = id + 1;
  const el = div("pane");
  el._mishSid = id;

  const term = new Terminal({
    fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
    fontSize,
    lineHeight: 1.15,
    cursorBlink: true,
    cursorStyle: "bar",
    allowProposedApi: true,
    macOptionIsMeta: false, // ⌥ compone caracteres (| @ # \ ~ [ ] { } €) en teclado español
    scrollback: 20000,
    theme: THEME,
  });
  const fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.loadAddon(new WebLinksAddon.WebLinksAddon((_e, url) => send({ t: "open", url })));

  const p = { id, tabId, term, fit, search: null, el, title: "mish", cwd: null, tint: null, bell: false, remote: false, started: false, replaying: false, claude: "none", claudeDetail: "", pillEl: null };
  // Píldora del estado de Claude Code (claude.css); oculta mientras no haya nada que mostrar.
  p.pillEl = div("claude-pill");
  p.pillEl.hidden = true;
  p.pillEl.append(Object.assign(document.createElement("span"), { className: "cp-dot" }),
    Object.assign(document.createElement("span"), { className: "cp-text" }));
  el.appendChild(p.pillEl);
  // Búsqueda en el scrollback (search.js): una SearchAddon por terminal.
  if (window.SearchAddon) {
    try { p.search = new SearchAddon.SearchAddon(); term.loadAddon(p.search); } catch (_) { p.search = null; }
  }
  panes.set(id, p);

  // Durante el replay xterm contesta a las consultas que trae (DA, DSR…): esas respuestas
  // no deben llegar a la shell como si fueran teclas.
  term.onData((d) => { if (!p.replaying) send({ t: "in", id, d }); });
  // Hasta mandar "new"/"attach" (con cols/rows ya ajustados) no hay sesión que redimensionar;
  // durante el replay tampoco: endReplay manda el tamaño final si cambió.
  term.onResize(({ cols, rows }) => { if (p.started && !p.replaying) send({ t: "resize", id, cols, rows }); });
  term.onTitleChange((title) => setTitle(p, title));

  // OSC 7: cwd ("file://host/ruta%20codificada"). Si viene de otro equipo (ssh) se ignora:
  // un servidor remoto no puede dirigir rutas locales.
  term.parser.registerOscHandler(7, (data) => {
    const cwd = parseOsc7(data);
    if (cwd) setCwd(p, cwd);
    return true;
  });
  // OSC 7701: mish pide teñir el panel ("bg=#rrggbb;title=…" o "reset").
  term.parser.registerOscHandler(7701, (data) => { tint(p, data); return true; });
  // OSC 7702: notificación de fin de comando largo; solo si no estás mirando este panel.
  term.parser.registerOscHandler(7702, (data) => {
    if (p.replaying) return true; // avisos viejos del replay: no se notifican
    const [title, ...rest] = data.split(";");
    const watching = active === p.tabId && tabs.get(p.tabId)?.focused === id;
    if (!document.hasFocus() || !watching) send({ t: "notify", title, msg: rest.join(";") });
    if (!watching) { p.bell = true; updateTab(tabs.get(p.tabId)); }
    return true;
  });

  // OSC 7703: `mish update` pide el flujo de actualización de la app. "update" y
  // "update-now" hacen lo mismo: comprobar y mostrar el aviso; la instalación solo arranca
  // con el clic en «Actualizar» (cualquier salida del terminal puede emitir este OSC, así
  // que nunca dispara una descarga por sí solo). Se ignora en el replay (avisos viejos) y en
  // paneles remotos.
  term.parser.registerOscHandler(7703, (data) => {
    if (p.replaying || p.remote) return true;
    if (data === "update" || data === "update-now") send({ t: "update_check" });
    return true;
  });

  term.attachCustomKeyEventHandler((e) => !(e.metaKey)); // ⌘ lo gestiona la app
  setupWheel(p);

  // Clic en un panel (o foco en su terminal) lo enfoca.
  el.addEventListener("mousedown", () => { if (tabs.get(p.tabId)?.focused !== id) focusPane(id); }, true);
  return p;
}

// Abre el terminal en su .pane (ya en el DOM y visible), ajusta y arranca o conecta la sesión.
// opts: {cwd?, argv?} → "new" (shell nueva; argv solo si se pide explícitamente);
//       {attach: true} → "attach" a una sesión viva del daemon, que responde con su replay.
// Decisión del replay: "attach" lleva el tamaño REAL ya calculado por fit (el .pane está
// visible al llamar aquí), así el daemon redimensiona una sola vez antes del replay. Mientras
// dura el replay no se mandan "resize" ni teclas; en mishReplayEnd, si el tamaño cambió
// (ventana redimensionada a mitad), se manda un único "resize" con el tamaño final.
function startPane(p, opts = {}) {
  p.term.open(p.el);
  try { p.term.loadAddon(new WebglAddon.WebglAddon()); } catch (_) { /* canvas por defecto */ }
  p.term.textarea?.addEventListener("focus", () => {
    const tab = tabs.get(p.tabId);
    if (tab && tab.focused !== p.id) focusPane(p.id, { focus: false });
  });
  fitNow(p);
  resizeObs.observe(p.el);
  if (opts.attach) sendAttach(p);
  else {
    const m = { t: "new", id: p.id, cols: p.term.cols, rows: p.term.rows };
    if (opts.cwd) m.cwd = opts.cwd;
    if (Array.isArray(opts.argv) && opts.argv.length) m.argv = opts.argv.map(String);
    send(m);
    // Hasta que su shell mande OSC 7, el panel "está" en el cwd heredado (otro split lo hereda).
    p.cwd = opts.cwd || null;
  }
  p.started = true;
}

// "attach" con el tamaño actual; hasta endReplay no salen teclas, resize ni avisos 7702.
function sendAttach(p) {
  const { cols, rows } = p.term;
  p.replaying = true;
  p.sentSize = `${cols}x${rows}`;
  clearTimeout(p.replayGuard);
  p.replayGuard = setTimeout(() => endReplay(p), REPLAY_MAX_MS);
  send({ t: "attach", id: p.id, cols, rows });
}

// Fin del replay de un panel: espera a que xterm haya procesado lo ya escrito (las respuestas
// a consultas y los OSC 7702 del replay salen al parsear) y reactiva teclas, resize y avisos.
function endReplay(p) {
  if (!p || !p.replaying || p.replayEnding) return;
  p.replayEnding = true;
  clearTimeout(p.replayGuard);
  p.term.write("", () => {
    p.replaying = false;
    p.replayEnding = false;
    if (!panes.has(p.id)) return;
    const { cols, rows } = p.term;
    if (`${cols}x${rows}` !== p.sentSize) send({ t: "resize", id: p.id, cols, rows });
  });
}

function disposePane(p) {
  clearTimeout(p.replayGuard);
  resizeObs.unobserve(p.el);
  pendingFit.delete(p);
  try { p.term.dispose(); } catch (_) { /* ya cerrado */ }
  p.el.remove();
  panes.delete(p.id);
}

// Reconstruye el DOM de la pestaña a partir del árbol, reutilizando los .pane existentes.
// Panel único: el .pane es hijo directo de .tabview (app.css no le pone indicadores de foco).
function render(tab) {
  const build = (node, path) => {
    if (L.isLeaf(node)) return panes.get(node.sid).el;
    const c = div(`split ${node.split}`);
    const a = build(node.a, path + "a");
    const b = build(node.b, path + "b");
    a.style.flex = `${node.ratio} 1 0`;
    b.style.flex = `${1 - node.ratio} 1 0`;
    const d = div("divider");
    d.dataset.path = path;
    d.addEventListener("mousedown", startDrag);
    d.addEventListener("dblclick", () => setRatio(tab, path, 0.5, d));
    c.append(a, d, b);
    return c;
  };
  const top = build(tab.root, "");
  top.style.flex = ""; // la raíz recibe flex de `.tabview > *`
  tab.viewEl.replaceChildren(top);
  scheduleSave();
}

function setRatio(tab, path, ratio, divider) {
  tab.root = L.setRatio(tab.root, path, ratio);
  const r = L.nodeAt(tab.root, path).ratio;
  divider.previousElementSibling.style.flex = `${r} 1 0`;
  divider.nextElementSibling.style.flex = `${1 - r} 1 0`;
  // Los .pane afectados se reajustan solos por su ResizeObserver.
  scheduleSave();
}

// Arrastre de un divisor: ajusta el ratio del split con un mínimo de 80 px por lado.
function startDrag(e) {
  if (e.button !== 0) return;
  e.preventDefault();
  const d = e.currentTarget;
  const tab = activeTab();
  if (!tab) return;
  const path = d.dataset.path;
  const row = d.parentElement.classList.contains("row");
  const box = d.parentElement.getBoundingClientRect();
  const dr = d.getBoundingClientRect();
  const size = (row ? box.width - dr.width : box.height - dr.height);
  d.classList.add("dragging");
  document.body.classList.add("resizing");
  const move = (ev) => {
    if (size < 2 * MIN_PANE_PX) return;
    const pos = (row ? ev.clientX - box.left - dr.width / 2 : ev.clientY - box.top - dr.height / 2);
    const lo = MIN_PANE_PX / size;
    setRatio(tab, path, Math.min(1 - lo, Math.max(lo, pos / size)), d);
  };
  const up = () => {
    window.removeEventListener("mousemove", move, true);
    window.removeEventListener("mouseup", up, true);
    d.classList.remove("dragging");
    document.body.classList.remove("resizing");
    if (!window.mishSearch?.isFocused()) focusedPane(tab)?.term.focus();
  };
  window.addEventListener("mousemove", move, true);
  window.addEventListener("mouseup", up, true);
}

// La pestaña refleja el panel enfocado (título, tinte, .remote) y avisa (.bell) si algún
// otro panel suyo terminó un comando largo.
function updateTab(tab) {
  if (!tab) return;
  const p = panes.get(tab.focused);
  if (!p) return;
  tab.title = p.title;
  tab.tabEl.querySelector(".title").textContent = p.title;
  tab.tabEl.style.setProperty("--tint", p.tint ? p.tint + "88" : "transparent");
  tab.tabEl.classList.toggle("remote", p.remote);
  tab.tabEl.classList.toggle("bell", L.leaves(tab.root).some((sid) => panes.get(sid)?.bell));
  // Estado de Claude agregado de todos sus paneles: waiting > error > working > done.
  let claude = null;
  for (const sid of L.leaves(tab.root)) {
    const s = panes.get(sid)?.claude;
    if (CLAUDE_RANK[s] && (!claude || CLAUDE_RANK[s] > CLAUDE_RANK[claude])) claude = s;
  }
  if (claude) tab.tabEl.dataset.claude = claude;
  else delete tab.tabEl.dataset.claude;
  if (tab.id === active) document.title = p.title;
}

// Enfoca un panel de su pestaña. opts.focus=false: no mueve el foco del teclado.
function focusPane(sid, opts = {}) {
  const p = panes.get(sid);
  const tab = p && tabs.get(p.tabId);
  if (!tab) return;
  const changed = tab.focused !== sid;
  tab.focused = sid;
  if (changed) scheduleSave();
  for (const id of L.leaves(tab.root)) panes.get(id)?.el.classList.toggle("focused", id === sid);
  p.bell = false;
  // «Claude · listo» se da por visto al enfocar el panel (no durante el arranque/montaje).
  if (p.claude === "done" && tab.id === active && !claudeHold) setClaude(p, "none", "", { silent: true, noTab: true });
  updateTab(tab);
  if (tab.id !== active) return;
  if (changed || opts.force) window.mishSearch?.onActivate(p);
  if (opts.focus !== false && !window.mishSearch?.isFocused()) p.term.focus();
}

function moveFocus(dir) {
  const tab = activeTab();
  if (!tab) return;
  const rects = new Map(L.leaves(tab.root).map((sid) => [sid, panes.get(sid).el.getBoundingClientRect()]));
  const to = L.neighbor(rects, tab.focused, dir);
  if (to != null) focusPane(to);
}

// Divide el panel enfocado: "row" deja el nuevo a la derecha, "col" debajo. Hereda su cwd.
function splitFocused(dir) {
  const tab = activeTab();
  const cur = focusedPane(tab);
  if (!cur) return;
  const p = createPane(tab.id);
  tab.root = L.split(tab.root, cur.id, dir, p.id);
  render(tab);
  startPane(p, { cwd: cur.cwd });
  focusPane(p.id);
}

function closePane(sid) {
  if (!panes.has(sid)) return;
  send({ t: "close", id: sid });
  removePane(sid);
}

// Quita el panel del árbol (sin avisar a Rust). Si la pestaña queda vacía, se cierra.
function removePane(sid) {
  const p = panes.get(sid);
  if (!p) return;
  const tab = tabs.get(p.tabId);
  const next = tab ? L.adjacent(tab.root, sid) : null;
  disposePane(p);
  if (!tab) return;
  tab.root = L.remove(tab.root, sid);
  if (!tab.root) { removeTab(tab.id); return; }
  render(tab);
  if (tab.focused === sid) focusPane(next ?? L.leaves(tab.root)[0], { force: true });
  else focusPane(tab.focused, { focus: tab.id === active });
}

// ---------- estado de Claude Code (hooks → daemon → mishClaude) ----------
// Estados: none|idle (sin indicador) · working · waiting (pide permiso/entrada) · done · error.
// Marcado: .pane > .claude-pill[data-state] y .tab[data-claude] (estilo en claude.css).

const CLAUDE_STATES = new Set(["none", "idle", "working", "waiting", "done", "error"]);
const CLAUDE_RANK = Object.assign(Object.create(null), { done: 1, working: 2, error: 3, waiting: 4 });
const CLAUDE_TEXT = { working: "Claude · trabajando", waiting: "Claude · esperando permiso", done: "Claude · listo", error: "Claude · error" };
const CLAUDE_DETAIL_MAX = 48; // un detail más largo no cabe en la píldora: va solo en el tooltip
let claudeHold = false; // durante el arranque no se limpia «listo» al montar/activar pestañas

function renderPill(p) {
  const el = p.pillEl;
  if (!el) return;
  const s = p.claude;
  if (!CLAUDE_RANK[s]) {
    el.hidden = true;
    delete el.dataset.state;
    el.removeAttribute("title");
    return;
  }
  const d = p.claudeDetail;
  let text = CLAUDE_TEXT[s];
  if (s === "waiting" && d && d.length <= CLAUDE_DETAIL_MAX) text = /^claude\b/i.test(d) ? d : `Claude · ${d}`;
  el.dataset.state = s;
  el.querySelector(".cp-text").textContent = text;
  el.title = d && d !== text ? d : text;
  el.hidden = false;
}

// Cambia el estado de Claude de un panel. opts.silent: sin avisos ni campana (arranque, mishMeta);
// opts.noTab: quien llama ya actualiza la pestaña.
function setClaude(p, state, detail, opts = {}) {
  if (!CLAUDE_STATES.has(state)) return;
  const prev = p.claude;
  p.claude = state;
  p.claudeDetail = typeof detail === "string" ? detail.replace(/\s+/g, " ").trim().slice(0, 200) : "";
  renderPill(p);
  const tab = tabs.get(p.tabId);
  // Aviso al pasar a waiting/done si no estás mirando el panel (misma regla que OSC 7702).
  if (!opts.silent && !p.replaying && state !== prev && (state === "waiting" || state === "done")) {
    const watching = active === p.tabId && tab?.focused === p.id;
    const what = state === "waiting" ? "Esperando permiso" : "Terminó";
    if (!document.hasFocus() || !watching) send({ t: "notify", title: "Claude", msg: `${what} — ${p.title}` });
    if (p.tabId !== active) p.bell = true;
  }
  if (!opts.noTab) updateTab(tab);
}

// Estado inicial (mishBoot) o resincronizado (mishMeta): "working" o {state, detail}. Sin avisos.
function applyClaudeMeta(p, c) {
  if (typeof c === "string") setClaude(p, c, "", { silent: true });
  else if (c && typeof c === "object" && typeof c.state === "string") setClaude(p, c.state, c.detail, { silent: true });
  else if (c == null) setClaude(p, "none", "", { silent: true });
}

// Rust → mishClaude({id, state, detail?}); un id que no es un panel de esta ventana se ignora.
window.mishClaude = (m) => {
  const p = m && typeof m === "object" && panes.get(m.id);
  if (p && typeof m.state === "string") setClaude(p, m.state, m.detail);
};

// ---------- pestañas ----------

// Crea la pestaña (DOM y entrada en `tabs`) sin paneles: root/focused los pone quien llama.
function makeTab() {
  const id = nextTabId++;
  const viewEl = div("tabview");
  panesEl.appendChild(viewEl);

  const tabEl = div("tab");
  tabEl.innerHTML = '<span class="title">mish</span><button class="close" title="Cerrar (⌘W)">×</button>';
  tabEl.addEventListener("mousedown", (e) => {
    if (e.button === 1) { e.preventDefault(); return; } // clic central: se cierra en mouseup
    if (e.button === 0 && !e.target.classList.contains("close")) activate(id);
  });
  tabEl.addEventListener("mouseup", (e) => {
    if (e.button === 1) { e.preventDefault(); closeTab(id); }
  });
  tabEl.querySelector(".close").addEventListener("click", () => closeTab(id));
  tabsEl.appendChild(tabEl);

  const tab = { id, tabEl, viewEl, root: null, focused: null, title: "mish" };
  tabs.set(id, tab);
  return tab;
}

// opts: {cwd?, argv?, title?}. Sin cwd, hereda el del panel enfocado (OSC 7); Rust lo valida.
function newTab(opts = {}) {
  const cwd = opts.cwd || focusedPane()?.cwd || undefined;
  const tab = makeTab();
  const p = createPane(tab.id);
  tab.root = { sid: p.id };
  tab.focused = p.id;
  render(tab);
  activate(tab.id, { focus: false });
  startPane(p, { cwd, argv: opts.argv });
  if (opts.title) setTitle(p, opts.title);
  focusPane(p.id, { force: true });
  return tab.id;
}

const normHost = (h) => String(h || "").toLowerCase().replace(/\.$/, "").replace(/\.local$/, "");

function parseOsc7(data) {
  let u;
  try { u = new URL(data); } catch (_) { return null; }
  if (u.protocol !== "file:") return null;
  const h = normHost(u.hostname);
  if (h && h !== "localhost" && h !== normHost(window.__mishHost)) return null;
  let path;
  try { path = decodeURIComponent(u.pathname); } catch (_) { return null; }
  return path.startsWith("/") ? path : null;
}

// Rueda con mouse tracking activo (Claude fullscreen, vim, htop…): xterm.js 6 amortigua
// los deltas del trackpad (×0,3) y envía un único evento por wheel aunque acumule varias
// líneas, así que el scroll se queda corto. Aquí acumulamos píxeles y enviamos un evento
// de rueda por cada línea, como un terminal nativo.
const WHEEL_MAX_PER_EVENT = 12;

function setupWheel(t) {
  const { term } = t;
  let sgr = false;
  let acc = 0;
  const trackMode = (on) => (params) => {
    for (const p of params) if (p === 1006 || (Array.isArray(p) && p.includes(1006))) sgr = on;
    return false; // que xterm siga procesando la secuencia
  };
  term.parser.registerCsiHandler({ prefix: "?", final: "h" }, trackMode(true));
  term.parser.registerCsiHandler({ prefix: "?", final: "l" }, trackMode(false));

  term.attachCustomWheelEventHandler((e) => {
    if (term.modes.mouseTrackingMode === "none" || e.shiftKey || e.deltaY === 0) return true;
    const screen = term.element.querySelector(".xterm-screen");
    const r = screen.getBoundingClientRect();
    const cellH = r.height / term.rows;
    const cellW = r.width / term.cols;
    if (!cellH || !cellW) return true;

    if (e.deltaMode === WheelEvent.DOM_DELTA_LINE) acc += e.deltaY * cellH;
    else if (e.deltaMode === WheelEvent.DOM_DELTA_PAGE) acc += e.deltaY * cellH * term.rows;
    else acc += e.deltaY;
    // Cambio de sentido: descarta el resto acumulado para responder al instante.
    if (Math.sign(acc) !== Math.sign(e.deltaY)) acc = e.deltaY;

    let lines = Math.trunc(acc / cellH);
    acc -= lines * cellH;
    if (!lines) return false;
    const up = lines < 0;
    lines = Math.min(Math.abs(lines), WHEEL_MAX_PER_EVENT);

    const col = Math.min(term.cols, Math.max(1, Math.floor((e.clientX - r.left) / cellW) + 1));
    const row = Math.min(term.rows, Math.max(1, Math.floor((e.clientY - r.top) / cellH) + 1));
    const btn = (up ? 64 : 65) + (e.altKey ? 8 : 0) + (e.ctrlKey ? 16 : 0);
    const one = sgr
      ? `\x1b[<${btn};${col};${row}M`
      : (col > 223 || row > 223) ? "" : `\x1b[M${String.fromCharCode(32 + btn, 32 + col, 32 + row)}`;
    if (one) send({ t: "in", id: t.id, d: one.repeat(lines) });
    return false;
  });
}

function setTitle(p, title) {
  const t = title || "mish";
  if (t === p.title) return;
  p.title = t;
  const tab = tabs.get(p.tabId);
  if (tab && tab.focused === p.id) updateTab(tab);
  scheduleSave();
}

function setCwd(p, cwd) {
  if (!cwd || cwd === p.cwd) return;
  p.cwd = cwd;
  scheduleSave();
}

// Fondo del panel: bg "#rrggbb" lo tiñe (sesión remota); null lo devuelve al tema base.
function setBg(p, bg) {
  p.remote = !!bg;
  p.tint = bg || null;
  p.el.style.backgroundColor = bg || "";
  p.term.options.theme = bg ? { ...THEME, background: bg, cursorAccent: bg } : { ...THEME };
}

function tint(p, data) {
  if (data === "reset") {
    setBg(p, null);
    setTitle(p, p.prevTitle || "mish");
    p.prevTitle = null;
    updateTab(tabs.get(p.tabId));
    return;
  }
  const kv = Object.fromEntries(data.split(";").map((s) => s.split("=")));
  if (kv.bg) setBg(p, kv.bg);
  if (kv.title) {
    p.prevTitle = p.prevTitle || p.title;
    setTitle(p, kv.title);
  }
  updateTab(tabs.get(p.tabId));
}

function activate(id, opts = {}) {
  const tab = tabs.get(id);
  if (!tab) return;
  for (const o of tabs.values()) {
    o.viewEl.classList.toggle("active", o.id === id);
    o.tabEl.classList.toggle("active", o.id === id);
  }
  if (active !== id) scheduleSave();
  active = id;
  if (opts.focus === false) return;
  focusPane(tab.focused, { force: true });
  requestAnimationFrame(() => window.mishRefit());
}

function closeTab(id) {
  const tab = tabs.get(id);
  if (!tab) return;
  for (const sid of L.leaves(tab.root)) send({ t: "close", id: sid });
  removeTab(id);
}

function removeTab(id) {
  const tab = tabs.get(id);
  if (!tab) return;
  const order = [...tabs.keys()];
  const idx = order.indexOf(id);
  for (const sid of L.leaves(tab.root)) { const p = panes.get(sid); if (p) disposePane(p); }
  tab.viewEl.remove();
  tab.tabEl.remove();
  tabs.delete(id);
  if (tabs.size === 0) {
    // Última pestaña cerrada (sus sesiones ya recibieron close o salieron): se guarda el
    // layout vacío, para no resucitarlas como shells nuevas al volver a abrir, y se sale.
    if (quitting) return;
    flushSave();
    quitting = true;
    send({ t: "quit" });
    return;
  }
  scheduleSave();
  if (active === id) {
    const rest = [...tabs.keys()];
    activate(rest[Math.min(idx, rest.length - 1)]);
  }
}

// Llamadas desde Rust (id = sid del panel).
window.mishOut = (id, b64) => { const p = panes.get(id); if (p) p.term.write(dec(b64)); };
window.mishExit = (id) => removePane(id);
window.mishReplayEnd = (id) => endReplay(panes.get(id));
// Estado de una sesión según el daemon (OSC 0/2/7/7701 que vio mientras no había GUI).
window.mishMeta = (m) => { const p = m && panes.get(m.id); if (p) applyMeta(p, m); };
window.mishHost = (h) => { window.__mishHost = typeof h === "string" ? h : ""; };
// Panel enfocado de la pestaña activa: {id (sid), term, fit, cwd, search, …} o null
// (lo usan search.js y vault.js; vault_type escribe en este sid).
window.mishFocused = () => focusedPane();

// Campo de texto de la página (no el textarea interno de xterm) que puede recibir un pegado.
function textField(el) {
  if (el instanceof HTMLTextAreaElement) return !el.classList.contains("xterm-helper-textarea") && !el.readOnly;
  return el instanceof HTMLInputElement && !el.readOnly && !el.disabled
    && ["text", "search", "password", "url", "email", "tel", ""].includes(el.type);
}

// ⌘V: si el foco está en un <input>/<textarea> de la página, el texto va ahí; si no, al terminal.
window.mishPaste = (id, b64) => {
  let text;
  try { text = utf8.decode(dec(b64)); } catch (_) { return; }
  const el = document.activeElement;
  if (textField(el)) {
    if (el instanceof HTMLInputElement) {
      text = text.replace(/\r?\n|\r/g, el.type === "password" ? "" : " ").replace(/\s+$/, "");
    }
    try {
      el.setRangeText(text, el.selectionStart ?? el.value.length, el.selectionEnd ?? el.value.length, "end");
    } catch (_) { el.value += text; } // email/tel no admiten setRangeText
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return;
  }
  const p = panes.get(id);
  if (p) p.term.paste(text);
};

function setFont(size) {
  fontSize = Math.max(9, Math.min(28, size));
  for (const p of panes.values()) { p.term.options.fontSize = fontSize; fitNow(p); }
}

const ARROWS = { arrowleft: "left", arrowright: "right", arrowup: "up", arrowdown: "down" };

window.addEventListener("keydown", (e) => {
  if (!e.metaKey) return;
  const k = e.key.toLowerCase();
  // ⌘/: ventana de atajos (help.js). En teclado español "/" es ⇧7: no se exige !shiftKey.
  // Abrir es idempotente (el menú nativo «Ayuda» puede dispararlo a la vez).
  if (e.key === "/") { e.preventDefault(); e.stopPropagation(); window.mishHelp?.({ open: true }); return; }
  // Con la ventana de atajos abierta solo siguen ⌘V (pega en su filtro vía mishPaste), ⌘Q y
  // pantalla completa; ⌘A/⌘C/⌘F los gestiona help.js y el resto no debe tocar el terminal.
  if (window.mishHelp?.isOpen?.() && !(k === "v" || k === "q" || (e.altKey && e.code === "KeyQ") || k === "enter" || (k === "f" && e.ctrlKey))) return;
  // ⌘A/⌘C en un <input> (caja fuerte) o dentro de un panel lateral (diff del panel git): que
  // los gestione su script, no el terminal.
  if ((k === "a" || k === "c") && (e.target instanceof HTMLInputElement || (e.target instanceof Element && e.target.closest(".side-panel")))) return;
  // ⌘F dentro de la caja fuerte (u otro panel lateral): usa su propio buscador.
  if (k === "f" && !e.ctrlKey && e.target instanceof Element && e.target.closest(".vault,.side-panel")) return;
  const order = [...tabs.keys()];
  const cur = focusedPane();
  let handled = true;
  if (e.altKey && ARROWS[k]) moveFocus(ARROWS[k]);
  else if ((k === "f" && e.ctrlKey) || k === "enter") send({ t: "fullscreen" });
  else if (k === "f") { if (window.mishSearch) window.mishSearch.open(); }
  else if (k === "d" && !e.altKey && !e.ctrlKey) splitFocused(e.shiftKey ? "col" : "row");
  else if (k === "t" || k === "n") newTab();
  else if (k === "w") { if (cur) closePane(cur.id); }
  else if (k === "q" || (e.altKey && e.code === "KeyQ")) { if (e.altKey) quitAll(); else window.mishBeforeQuit(); } // ⌥Q puede dar "œ"
  else if (k === "c") { const s = cur && cur.term.getSelection(); if (s) send({ t: "copy", text: s }); }
  else if (k === "v") { if (cur) send({ t: "paste", id: cur.id }); }
  else if (k === "k") cur && cur.term.clear();
  else if (k === "a") cur && cur.term.selectAll();
  else if (k === "=" || k === "+") setFont(fontSize + 1);
  else if (k === "-") setFont(fontSize - 1);
  else if (k === "0") setFont(13);
  else if (/^[1-9]$/.test(k)) { const i = Math.min(+k, order.length) - 1; activate(order[i]); }
  else if (e.shiftKey && (k === "]" || k === "}")) activate(order[(order.indexOf(active) + 1) % order.length]);
  else if (e.shiftKey && (k === "[" || k === "{")) activate(order[(order.indexOf(active) - 1 + order.length) % order.length]);
  else handled = false;
  if (handled) { e.preventDefault(); e.stopPropagation(); }
}, true);

document.getElementById("add").addEventListener("click", () => newTab());
// Barra superior: arrastrar mueve la ventana; doble clic la hace ocupar toda la pantalla.
const bar = document.getElementById("bar");
bar.addEventListener("mousedown", (e) => {
  if (e.button !== 0 || !["bar", "drag", "tabs"].includes(e.target.id)) return;
  if (e.detail === 2) send({ t: "zoom" });
  else send({ t: "drag" });
});
// El redimensionado de la ventana lo recoge el ResizeObserver de cada .pane.

// ---------- layout persistente (tabs.json v1) ----------
// {"v":1,"active":idx,"tabs":[{"title","focused":sid,"root":hoja|split}]}
//   hoja: {sid, cwd, title} · split: {split:"row"|"col", ratio, a, b}
// Solo estructura y metadatos: nunca scrollback ni contenido. Rust valida y escribe atómico.

function serializeLayout() {
  const node = (n) => {
    if (L.isLeaf(n)) {
      const p = panes.get(n.sid);
      // El título guardado es el local: una shell nueva no está en el ssh de antes.
      const title = p ? (p.remote ? p.prevTitle || p.title : p.title) : "mish";
      return { sid: n.sid, cwd: p?.cwd || null, title: title || "mish" };
    }
    return { split: n.split, ratio: Math.round(n.ratio * 1e4) / 1e4, a: node(n.a), b: node(n.b) };
  };
  const list = [...tabs.values()].filter((t) => t.root);
  return {
    v: 1,
    active: Math.max(0, list.findIndex((t) => t.id === active)),
    tabs: list.map((t) => ({ title: t.title || "mish", focused: t.focused, root: node(t.root) })),
  };
}

let saveTimer = 0;
function flushSave() {
  clearTimeout(saveTimer);
  saveTimer = 0;
  if (!booted || quitting || saveDisabled) return;
  send({ t: "save_layout", json: JSON.stringify(serializeLayout()) });
}
// Cualquier cambio de árbol, foco, pestaña activa, título, cwd o ratio: guardado en 300 ms.
function scheduleSave() {
  if (!booted || quitting || saveDisabled) return;
  clearTimeout(saveTimer);
  saveTimer = setTimeout(flushSave, SAVE_DEBOUNCE_MS);
}

// ⌘Q y CloseRequested (Rust): guarda ya y sale. Con daemon las sesiones siguen vivas.
window.mishBeforeQuit = () => {
  if (!quitting) { flushSave(); quitting = true; }
  send({ t: "quit" });
};
// ⌘⌥Q: guarda el layout (las pestañas se reabren con shells nuevas en sus cwd), mata todas
// las sesiones y sale. Rust/daemon cierran; los mishExit que lleguen ya no tocan tabs.json.
function quitAll() {
  if (!quitting) { flushSave(); quitting = true; }
  send({ t: "quit_all" });
}

// ---------- arranque: reconciliar layout guardado y sesiones vivas ----------

const isSid = (x) => Number.isInteger(x) && x > 0 && x < 2 ** 32;
const absPath = (c) => (typeof c === "string" && c.startsWith("/") ? c : null);

// Valida un nodo de tabs.json; null si está mal formado.
function cleanNode(n, depth = 0) {
  if (!n || typeof n !== "object" || depth > 32) return null;
  if (n.split === undefined) {
    if (!isSid(n.sid)) return null;
    return { sid: n.sid, cwd: absPath(n.cwd), title: typeof n.title === "string" ? n.title : null };
  }
  if (n.split !== "row" && n.split !== "col") return null;
  const a = cleanNode(n.a, depth + 1);
  const b = cleanNode(n.b, depth + 1);
  if (!a || !b) return a || b; // una rama rota: se queda la otra
  const r = Number(n.ratio);
  return { split: n.split, ratio: Number.isFinite(r) ? Math.min(0.95, Math.max(0.05, r)) : 0.5, a, b };
}

function cleanLayout(l) {
  if (typeof l === "string") { try { l = JSON.parse(l); } catch (_) { return null; } }
  if (!l || typeof l !== "object" || l.v !== 1 || !Array.isArray(l.tabs)) return null;
  const tabsOut = l.tabs.map((t) => (t && typeof t === "object"
    ? { title: typeof t.title === "string" ? t.title : null, focused: t.focused, root: cleanNode(t.root) }
    : { root: null }));
  return { active: Number.isInteger(l.active) ? l.active : 0, tabs: tabsOut };
}

const savedLeaves = (n, out = []) => {
  if (!n) return out;
  if (n.split === undefined) out.push(n);
  else { savedLeaves(n.a, out); savedLeaves(n.b, out); }
  return out;
};

// Título, cwd y tinte de una sesión (mishMeta o entrada de `sessions`). Reutiliza la lógica
// de OSC 0/7/7701: tint es "#rrggbb", "bg=…;title=…", o null/""/"reset" para quitarlo.
function applyMeta(p, m) {
  if ("tint" in m) {
    const t = m.tint;
    if (t == null || t === "" || t === "reset") { if (p.tint) setBg(p, null); }
    else if (typeof t === "string" && /^#[0-9a-f]{3,8}$/i.test(t)) setBg(p, t);
    else if (typeof t === "string" && t.includes("=")) tint(p, t);
  }
  if (typeof m.title === "string" && m.title) setTitle(p, m.title);
  if ("claude" in m) applyClaudeMeta(p, m.claude);
  if (typeof m.cwd === "string") {
    const c = m.cwd.startsWith("file:") ? parseOsc7(m.cwd) : absPath(m.cwd);
    if (c) setCwd(p, c);
  }
  updateTab(tabs.get(p.tabId));
}

// Monta una pestaña: crea sus paneles, la hace visible (para que fit mida de verdad) y
// lanza "attach" o "new" por cada hoja. spec: sid → {attach, meta?} | {cwd?, title?}.
function mountTab(root, focused, specs) {
  const tab = makeTab();
  const sids = L.leaves(root);
  for (const sid of sids) {
    const p = createPane(tab.id, sid);
    const sp = specs.get(sid);
    if (sp.title) setTitle(p, sp.title);
    if (sp.meta) applyMeta(p, sp.meta);
  }
  tab.root = root;
  tab.focused = sids.includes(focused) ? focused : sids[0];
  render(tab);
  activate(tab.id, { focus: false });
  for (const sid of sids) {
    const sp = specs.get(sid);
    const p = panes.get(sid);
    if (sp.attach) startPane(p, { attach: true });
    else startPane(p, { cwd: sp.cwd || undefined }); // solo la shell: nunca se reejecuta nada
  }
  focusPane(tab.focused, { focus: false });
  return tab;
}

function reconcile(info) {
  const sessions = (Array.isArray(info.sessions) ? info.sessions : [])
    .filter((x) => x && typeof x === "object" && isSid(x.id));
  const alive = new Map(sessions.map((x) => [x.id, x]));
  const layout = cleanLayout(info.layout);

  // nextId = max(ids vivos y del layout) + 1: un "new" nunca choca con nada conocido.
  let max = nextSid - 1;
  for (const id of alive.keys()) max = Math.max(max, id);
  if (layout) for (const t of layout.tabs) for (const lf of savedLeaves(t.root)) max = Math.max(max, lf.sid);
  nextSid = max + 1;

  const used = new Set(); // sids vivos ya asignados (un sid duplicado en el layout = muerto)
  let activeTab = null;
  layout?.tabs.forEach((t, i) => {
    if (!t.root) return;
    const specs = new Map();
    const remap = new Map(); // sid guardado → sid del panel
    const build = (n) => {
      if (n.split !== undefined) return { split: n.split, ratio: n.ratio, a: build(n.a), b: build(n.b) };
      const s = alive.get(n.sid);
      if (s && !used.has(n.sid)) {
        used.add(n.sid);
        remap.set(n.sid, n.sid);
        specs.set(n.sid, { attach: true, title: n.title, meta: s });
        return { sid: n.sid };
      }
      // Sesión muerta: shell nueva con id nuevo, en su cwd y con su título guardados.
      const sid = nextSid++;
      if (!remap.has(n.sid)) remap.set(n.sid, sid);
      specs.set(sid, { cwd: n.cwd, title: n.title });
      return { sid };
    };
    const root = build(t.root);
    const tab = mountTab(root, remap.get(t.focused), specs);
    if (i === layout.active) activeTab = tab.id;
  });

  // Sesiones vivas que el layout no conocía (layout perdido u otra ventana): una pestaña cada una.
  for (const s of sessions.sort((a, b) => a.id - b.id)) {
    if (used.has(s.id) || panes.has(s.id)) continue;
    used.add(s.id);
    mountTab({ sid: s.id }, s.id, new Map([[s.id, { attach: true, meta: s }]]));
  }
  return activeTab;
}

let bootTimer = 0;
function boot(info) {
  if (booted) {
    // mishBoot tardío (después del fallback): ya hay pestañas; solo se conectan las sesiones
    // vivas que falten, sin tocar las actuales.
    // Tras taken_over, los paneles que siguen vivos se vuelven a conectar con su replay
    // (se limpia el terminal para no duplicar la salida) y se reanuda el guardado.
    const prev = active;
    claudeHold = true;
    try {
      if (takenOver) {
        const alive = new Set((Array.isArray(info.sessions) ? info.sessions : []).map((x) => x && x.id));
        for (const p of panes.values()) {
          if (!alive.has(p.id) || !p.started) continue;
          p.term.reset();
          fitNow(p);
          sendAttach(p);
        }
        takenOver = false;
        saveDisabled = false;
        hideNotice();
      }
      reconcile({ sessions: info.sessions });
      for (const m of Array.isArray(info.sessions) ? info.sessions : []) {
        const p = m && panes.get(m.id);
        if (p) applyMeta(p, m);
      }
    } catch (e) { console.error("mishBoot", e); }
    if (prev != null && tabs.has(prev)) activate(prev);
    claudeHold = false;
    scheduleSave();
    return;
  }
  booted = true;
  clearTimeout(bootTimer);
  if (typeof info.hostname === "string") window.__mishHost = info.hostname;
  window.__mishMode = info.mode === "local" ? "local" : info.mode === "daemon" ? "daemon" : null;
  let activeTab = null;
  claudeHold = true;
  try { activeTab = reconcile(info); } catch (e) { console.error("mishBoot", e); }
  if (tabs.size === 0) { claudeHold = false; newTab(); return; }
  activate(activeTab ?? [...tabs.keys()][0]);
  claudeHold = false;
  scheduleSave();
}
// Rust → {layout: tabs.json v1 | null, sessions: [{id,pid,cwd,title,tint,cols,rows,started,claude?}],
//         hostname, mode: "daemon" | "local"}
window.mishBoot = (info) => boot(info && typeof info === "object" ? info : {});
bootTimer = setTimeout(() => boot({}), BOOT_WAIT_MS);

// ---------- aviso (#notice): versión del daemon, otra ventana, modo local, actualizaciones… ----------
// Rust → mishNotice(n); mishNotice(null) lo oculta. n = {
//   kind: "build"|"local"|"proto"|"taken_over"|"update"|"updating"|"uptodate"|"updated"|"error",
//   title?, msg?, list?: [texto…] (p. ej. commits), progress?: 0-100 (barra con porcentaje),
//   busy?: bool (barra indeterminada fuera de "updating"),
//   actions?: [{id, label, style?: "primary"|"secondary"|"danger"}] }
// Cada llamada pinta el estado completo (se limpia lo anterior). Al pulsar una acción se manda
// {t:"notice_action", id} y se oculta. Marcado en index.html y estilo en notice.css (el
// desplazamiento con la caja fuerte abierta lo da body.vault-open, que pone vault.js).
const NOTICE_STYLE = { restart: "danger", local: "secondary" };
function noticeEl() {
  let el = document.getElementById("notice");
  if (!el) { // index.html sin marcado (no debería pasar): uno mínimo con la misma estructura
    el = div("");
    el.id = "notice";
    el.hidden = true;
    const text = div("n-text");
    const title = document.createElement("strong"); title.className = "n-title";
    const msg = document.createElement("span"); msg.className = "n-msg";
    const list = document.createElement("ul"); list.className = "n-list";
    text.append(title, msg, list, div("n-progress"));
    const close = document.createElement("button");
    close.type = "button"; close.className = "n-close"; close.setAttribute("aria-label", "Cerrar"); close.textContent = "×";
    el.append(div("n-icon"), text, div("n-actions"), close);
    document.body.appendChild(el);
  }
  return el;
}
function hideNotice() { document.getElementById("notice")?.classList.remove("open"); }
function showNotice(n) {
  const el = noticeEl();
  const kind = String(n.kind || "");
  el.dataset.kind = kind;
  el.setAttribute("role", kind === "proto" || kind === "error" ? "alert" : "status");
  const setText = (sel, v) => {
    const x = el.querySelector(sel);
    if (!x) return;
    x.textContent = v == null ? "" : String(v);
    x.hidden = !x.textContent; // .n-title/.n-msg vacíos no ocupan sitio
  };
  setText(".n-title", n.title);
  setText(".n-msg", n.msg);
  const list = el.querySelector(".n-list");
  if (list) {
    const items = Array.isArray(n.list) ? n.list : [];
    list.replaceChildren(...items.map((t) => { const li = document.createElement("li"); li.textContent = String(t); return li; }));
    list.hidden = !items.length;
  }
  // Progreso: se limpia en cada cambio de estado; solo queda si viene en este aviso.
  const pct = Number(n.progress);
  if (n.progress != null && Number.isFinite(pct)) {
    const v = String(Math.round(Math.min(100, Math.max(0, pct))));
    el.dataset.p = v;
    el.style.setProperty("--p", v);
  } else {
    delete el.dataset.p;
    el.style.removeProperty("--p");
  }
  el.classList.toggle("n-busy", !!n.busy);
  const acts = el.querySelector(".n-actions");
  if (acts) {
    const list = (Array.isArray(n.actions) ? n.actions : []).filter((x) => x && x.id != null);
    acts.replaceChildren(...list.map((x, i) => {
      const b = document.createElement("button");
      b.type = "button";
      b.dataset.action = String(x.id);
      const st = ["primary", "secondary", "danger"].includes(x.style) ? x.style
        : NOTICE_STYLE[x.id] || (i === 0 ? "primary" : "secondary");
      b.className = st;
      b.textContent = String(x.label ?? x.id);
      return b;
    }));
  }
  el.hidden = false;
  el.classList.add("open");
}
window.mishNotice = (n) => {
  if (!n || typeof n !== "object") { hideNotice(); return; }
  // Otra ventana tomó las sesiones: esta deja de escribir tabs.json hasta un mishBoot nuevo.
  if (n.kind === "taken_over") { takenOver = true; saveDisabled = true; clearTimeout(saveTimer); }
  showNotice(n);
};
document.addEventListener("click", (e) => {
  if (!(e.target instanceof Element)) return;
  const b = e.target.closest("#notice .n-actions button[data-action]");
  if (b) { send({ t: "notice_action", id: b.dataset.action }); hideNotice(); return; }
  if (e.target.closest("#notice .n-close")) hideNotice();
});
