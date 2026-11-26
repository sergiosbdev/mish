// Mish.app — panel git en un lateral (⌘⇧G): Cambios (status + diff), PRs, Issues y Buscar.
//
// Marcado en index.html (<aside id="gitpanel" class="side-panel">) y estilos en git.css; aquí
// solo el comportamiento. Rust (gitpanel.rs) hace todo el trabajo con git/gh:
//   JS → Rust: {t:"git", req, op, sid, cwd?, path?, staged?, untracked?, q?, regex?, line?, url?}
//   Rust → JS: window.mishGit({req, op, ok, data?, error?})
// `req` crece en cada petición; por cada op solo vale la respuesta a la última (las viejas se
// descartan: p. ej. un diff lento de un fichero que ya no está seleccionado).
//
// SEGURIDAD: nombres de fichero, diff, títulos de PR/issue, etiquetas y coincidencias vienen
// de fuera (repos ajenos, GitHub). Todo se pinta con textContent/createElement; nunca innerHTML
// con datos. El único estilo derivado de datos es el color de las etiquetas, validado como hex.
//
// Refresco automático del status mientras el panel está abierto (sin tocar app.js):
// - Cambio de panel enfocado o de pestaña: un sondeo de 1 s (solo con el panel abierto) compara
//   el id y el cwd de window.mishFocused(); el propio intervalo hace de debounce.
// - OSC 7 del panel enfocado (la shell lo manda en cada prompt, haya cambiado o no el cwd: así
//   se ve el resultado de un `git add`/`commit`): se registra un segundo manejador de OSC 7 en
//   cada terminal que vemos enfocado; devuelve false para que el de app.js (setCwd) siga
//   recibiéndolo. Debounce de 1 s.
//
// Supuestos sobre app.js/side.js (scripts clásicos cargados antes): window.mishFocused() →
// {id, cwd, term, …} | null; window.mishSide (side.js) para la exclusión con la caja fuerte;
// app.js generaliza window.mishPaste a cualquier <input> enfocado (⌘V en .g-q).
(() => {
  "use strict";

  const root = document.getElementById("gitpanel");
  if (!root) return;

  const ANIM_MS = 180;
  const POLL_MS = 1000;
  const OSC_DEBOUNCE_MS = 1000;
  const MAX_DIFF_LINES = 5000;
  const TABS = ["changes", "prs", "issues", "search"];
  const CI = new Set(["ok", "fail", "pending", "none"]);
  const GROUPS = [
    ["conflict", "Conflictos"],
    ["staged", "Staged"],
    ["unstaged", "Cambios"],
    ["untracked", "Sin seguimiento"],
  ];
  const REVIEW = {
    APPROVED: "aprobada",
    CHANGES_REQUESTED: "cambios pedidos",
    REVIEW_REQUIRED: "revisión pendiente",
  };

  // ---------- DOM ----------

  const $ = (sel, el = root) => el.querySelector(sel);
  const repoEl = $(".g-repo");
  const branchEl = $(".g-branch");
  const syncEl = $(".g-sync");
  const stateEl = $(".g-state");
  const filesEl = $(".g-files");
  const diffEl = $(".g-diff");
  const qEl = $(".g-q");
  const regexEl = $(".g-regex");
  const hitsEl = $(".g-hits");
  const tabBtns = [...root.querySelectorAll(".g-tabs [data-tab]")];
  const views = Object.fromEntries([...root.querySelectorAll(".g-view[data-view]")].map((v) => [v.dataset.view, v]));
  const prList = views.prs && $(".g-list", views.prs);
  const issueList = views.issues && $(".g-list", views.issues);

  const el = (tag, cls, text) => {
    const e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = String(text);
    return e;
  };

  // ---------- entorno ----------

  const post = (o) => {
    try { window.ipc.postMessage(JSON.stringify(o)); } catch (_) { /* sin IPC (pruebas) */ }
  };
  const focused = () => {
    try { return (typeof window.mishFocused === "function" && window.mishFocused()) || null; } catch (_) { return null; }
  };
  const reducedMotion = () => {
    try { return window.matchMedia("(prefers-reduced-motion: reduce)").matches; } catch (_) { return false; }
  };

  function focusTerm() {
    const t = focused();
    if (t && t.term) t.term.focus();
    else if (root.contains(document.activeElement)) document.activeElement.blur();
  }

  // ---------- estado ----------

  let isOpen = false;
  let closeTimer = 0;
  let pollTimer = 0;
  let oscTimer = 0;
  let tab = "changes";
  let reqSeq = 0;
  const latest = {}; // op → último req enviado
  let target = null; // {id, cwd} del panel al que se pidió el último status
  let status = null; // data del último status bueno
  let curRoot = null; // raíz del repo (clave de la caché de PRs/issues)
  let selKey = null; // "kind\0path" del fichero seleccionado
  let lastGrep = null; // {q, regex} de la última búsqueda lanzada
  const cache = { prs: new Map(), issues: new Map() }; // root → lista
  const viewState = Object.fromEntries(TABS.map((t) => [t, { kind: "", text: "" }]));

  function request(op, extra = {}) {
    const p = target || focused();
    const req = ++reqSeq;
    latest[op] = req;
    const msg = { t: "git", req, op, sid: p ? p.id : null, ...extra };
    if (p && p.cwd && msg.cwd === undefined) msg.cwd = p.cwd;
    post(msg);
    return req;
  }

  function setState(view, kind, text) {
    viewState[view] = { kind: text ? kind : "", text: text || "" };
    if (view === tab) renderState();
  }

  function renderState() {
    const s = viewState[tab];
    stateEl.textContent = s.text;
    if (s.text) stateEl.dataset.kind = s.kind;
    else delete stateEl.dataset.kind;
  }

  // ---------- cabecera ----------

  function renderHead() {
    const s = status;
    repoEl.textContent = s ? String(s.repo || "") : "";
    branchEl.textContent = s ? String(s.branch || "") : "";
    const parts = [];
    if (s && s.ahead > 0) parts.push(`↑${s.ahead | 0}`);
    if (s && s.behind > 0) parts.push(`↓${s.behind | 0}`);
    syncEl.textContent = parts.join(" ");
    syncEl.title = s && s.upstream ? `Upstream: ${s.upstream}` : (s ? "Sin upstream" : "");
    root.title = s && s.root ? String(s.root) : "";
  }

  // ---------- Cambios ----------

  const keyOf = (f) => `${f.kind}\0${f.path}`;

  function stLabel(f) {
    if (f.kind === "untracked") return "?";
    if (f.kind === "staged") return f.x || "";
    if (f.kind === "unstaged") return f.y || "";
    return `${f.x || ""}${f.y || ""}`;
  }

  function cleanFile(f) {
    if (!f || typeof f.path !== "string" || !f.path) return null;
    const kind = GROUPS.some(([k]) => k === f.kind) ? f.kind : "unstaged";
    return {
      path: f.path,
      orig: typeof f.orig === "string" && f.orig ? f.orig : null,
      x: typeof f.x === "string" ? f.x.slice(0, 1) : "",
      y: typeof f.y === "string" ? f.y.slice(0, 1) : "",
      kind,
    };
  }

  const fileItems = () => [...filesEl.querySelectorAll("li.g-file")];

  function renderFiles() {
    const files = status && Array.isArray(status.files) ? status.files.map(cleanFile).filter(Boolean) : [];
    filesEl.replaceChildren();
    let selLi = null;
    for (const [kind, label] of GROUPS) {
      const group = files.filter((f) => f.kind === kind);
      if (!group.length) continue;
      const h = el("li", "g-group", label);
      h.setAttribute("role", "presentation");
      h.dataset.count = String(group.length);
      filesEl.append(h);
      for (const f of group) {
        const li = el("li", "g-file");
        li.dataset.st = f.kind;
        li.dataset.x = f.x;
        li.dataset.y = f.y;
        li.tabIndex = -1;
        li.setAttribute("role", "option");
        li._file = f;
        const path = f.orig ? `${f.orig} → ${f.path}` : f.path;
        li.title = path;
        li.append(el("span", "g-st", stLabel(f)), el("span", "g-path", path));
        if (keyOf(f) === selKey) selLi = li;
        filesEl.append(li);
      }
    }
    filesEl.setAttribute("role", "listbox");
    filesEl.setAttribute("aria-label", "Ficheros cambiados");
    if (selLi) markSelected(selLi);
    else {
      selKey = null;
      diffEl.replaceChildren();
      const first = fileItems()[0];
      if (first) first.tabIndex = 0;
    }
    if (status) setState("changes", "empty", files.length ? "" : "Sin cambios: el árbol de trabajo está limpio.");
  }

  function markSelected(li) {
    for (const o of fileItems()) {
      const on = o === li;
      o.classList.toggle("selected", on);
      o.setAttribute("aria-selected", String(on));
      o.tabIndex = on ? 0 : -1;
    }
  }

  function selectFile(li, { focus = true } = {}) {
    if (!li || !li._file) return;
    markSelected(li);
    selKey = keyOf(li._file);
    if (focus) li.focus({ preventScroll: true });
    li.scrollIntoView({ block: "nearest" });
    requestDiff(li._file);
  }

  function requestDiff(f) {
    request("diff", { path: f.path, staged: f.kind === "staged", untracked: f.kind === "untracked" });
  }

  function lineClass(l) {
    if (l.startsWith("+++ ") || l.startsWith("--- ") || l === "+++" || l === "---") return "l-meta";
    if (l.startsWith("@@")) return "l-hunk";
    if (l.startsWith("+")) return "l-add";
    if (l.startsWith("-")) return "l-del";
    if (/^(diff |index |new file|deleted file|similarity |dissimilarity |rename |copy |old mode|new mode|Binary files|\\ No newline)/.test(l)) return "l-meta";
    return "l-ctx";
  }

  function renderDiff(d) {
    const lines = Array.isArray(d.lines) ? d.lines : [];
    const pre = el("pre");
    const shown = Math.min(lines.length, MAX_DIFF_LINES);
    const frag = document.createDocumentFragment();
    for (let i = 0; i < shown; i++) {
      const l = typeof lines[i] === "string" ? lines[i] : String(lines[i] ?? "");
      frag.append(el("div", lineClass(l), l === "" ? " " : l));
    }
    if (!shown) frag.append(el("div", "l-meta", "(sin diferencias de texto)"));
    pre.append(frag);
    diffEl.replaceChildren(pre);
    if (d.truncated || lines.length > MAX_DIFF_LINES) {
      diffEl.append(el("div", "g-trunc",
        lines.length > MAX_DIFF_LINES
          ? `Diff recortado: se muestran ${MAX_DIFF_LINES} de ${lines.length} líneas.`
          : "Diff recortado: el fichero es demasiado grande para mostrarlo entero."));
    }
    diffEl.scrollTop = 0;
  }

  function refreshStatus({ silent = false } = {}) {
    const p = focused();
    target = p ? { id: p.id, cwd: p.cwd || null } : null;
    if (!target) {
      status = null;
      renderHead();
      renderFiles();
      setState("changes", "error", "No hay ningún panel enfocado.");
      return;
    }
    if (!silent || !status) setState("changes", "loading", "Leyendo el estado del repo…");
    request("status");
  }

  function onStatus(r) {
    if (!r.ok || !r.data) {
      status = null;
      curRoot = null;
      renderHead();
      filesEl.replaceChildren();
      diffEl.replaceChildren();
      selKey = null;
      setState("changes", "error", r.error || "No se pudo leer el estado del repo.");
      for (const t of ["prs", "issues", "search"]) clearView(t);
      if (tab === "prs" || tab === "issues") loadList(tab); // muestra el mismo error
      return;
    }
    const prevRoot = curRoot;
    status = r.data;
    curRoot = typeof status.root === "string" ? status.root : null;
    renderHead();
    if (prevRoot !== curRoot) {
      selKey = null;
      for (const t of ["prs", "issues", "search"]) clearView(t);
    }
    const hadSel = selKey;
    renderFiles();
    // El fichero seleccionado sigue ahí: su diff puede haber cambiado.
    if (hadSel && selKey) {
      const li = filesEl.querySelector("li.g-file.selected");
      if (li) requestDiff(li._file);
    }
    if (prevRoot !== curRoot && (tab === "prs" || tab === "issues")) loadList(tab);
  }

  function onDiff(r) {
    if (!r.ok || !r.data) {
      diffEl.replaceChildren(el("div", "g-trunc", r.error || "No se pudo obtener el diff."));
      return;
    }
    renderDiff(r.data);
  }

  // ---------- PRs / Issues ----------

  function clearView(t) {
    if (t === "prs" && prList) prList.replaceChildren();
    if (t === "issues" && issueList) issueList.replaceChildren();
    if (t === "search") { hitsEl.replaceChildren(); lastGrep = null; }
    setState(t, "", "");
  }

  function loadList(t, { force = false } = {}) {
    if (!curRoot) {
      // Sin repo todavía: si el status está en camino, onStatus vuelve a llamar aquí.
      clearView(t);
      const ch = viewState.changes;
      if (ch.kind === "loading") setState(t, "loading", ch.text);
      else setState(t, "error", (ch.kind === "error" && ch.text) || "No es un repositorio git.");
      return;
    }
    const cached = cache[t].get(curRoot);
    if (cached && !force) { renderList(t, cached); return; }
    setState(t, "loading", t === "prs" ? "Cargando pull requests…" : "Cargando issues…");
    const req = request(t);
    pendingRoot[t] = { req, root: curRoot };
  }
  const pendingRoot = {};

  const safeUrl = (u) => (typeof u === "string" && /^https:\/\/github\.com\//.test(u) ? u : null);

  function authorOf(a) {
    if (!a) return "";
    if (typeof a === "string") return a;
    return String(a.login || a.name || "");
  }

  function ago(iso) {
    const t = Date.parse(iso);
    if (!Number.isFinite(t)) return "";
    const s = Math.max(0, (Date.now() - t) / 1000);
    if (s < 90) return "ahora";
    const m = s / 60, h = m / 60, d = h / 24;
    if (m < 60) return `hace ${Math.round(m)} min`;
    if (h < 24) return `hace ${Math.round(h)} h`;
    if (d < 30) return `hace ${Math.round(d)} d`;
    if (d < 365) return `hace ${Math.round(d / 30)} mes${Math.round(d / 30) === 1 ? "" : "es"}`;
    return `hace ${Math.round(d / 365)} a`;
  }

  function labelEl(lb) {
    const name = String((lb && lb.name) || "");
    const span = el("span", "g-label", name);
    const hex = String((lb && lb.color) || "").replace(/^#/, "");
    if (/^[0-9a-fA-F]{6}$/.test(hex)) {
      span.style.backgroundColor = `#${hex}`; // el color del texto lo pone git.css
    }
    return span;
  }

  function renderList(t, items) {
    const list = t === "prs" ? prList : issueList;
    if (!list) return;
    list.replaceChildren();
    list.setAttribute("role", "listbox");
    list.setAttribute("aria-label", t === "prs" ? "Pull requests" : "Issues");
    for (const it of items) {
      if (!it || typeof it !== "object") continue;
      const li = el("li", t === "prs" ? "g-pr" : "g-issue");
      li.tabIndex = -1;
      li.setAttribute("role", "option");
      li._url = safeUrl(it.url);
      li.append(el("span", "g-num", `#${Number.isFinite(+it.number) ? +it.number : "?"}`),
        el("span", "g-ttl", it.title ?? ""));
      const meta = [authorOf(it.author)];
      if (t === "prs") {
        li.dataset.ci = CI.has(it.ci) ? it.ci : "none";
        li.title = `CI: ${li.dataset.ci}`;
        if (it.draft) meta.push("borrador");
        if (it.review) meta.push(REVIEW[it.review] || String(it.review).toLowerCase());
      } else {
        const labels = Array.isArray(it.labels) ? it.labels : [];
        if (labels.length) {
          const box = el("span", "g-labels");
          for (const lb of labels) box.append(labelEl(lb));
          li.append(box);
        }
        const when = ago(it.updated);
        if (when) meta.push(when);
      }
      li.append(el("span", "g-meta", meta.filter(Boolean).join(" · ")));
      list.append(li);
    }
    const first = list.querySelector("li");
    if (first) first.tabIndex = 0;
    setState(t, "empty", list.children.length ? "" : (t === "prs" ? "No hay pull requests abiertas." : "No hay issues abiertas."));
  }

  function onList(t, r) {
    const pend = pendingRoot[t];
    if (!r.ok) {
      if (t === "prs" && prList) prList.replaceChildren();
      if (t === "issues" && issueList) issueList.replaceChildren();
      setState(t, "error", r.error || "No se pudo consultar GitHub (¿está gh instalado y con sesión?).");
      return;
    }
    const items = Array.isArray(r.data) ? r.data : [];
    if (pend && pend.root) cache[t].set(pend.root, items);
    if (!pend || pend.root === curRoot) renderList(t, items);
  }

  function openUrl(li) {
    if (!li || !li._url) return;
    request("open_url", { url: li._url });
  }

  // ---------- Buscar ----------

  function runGrep() {
    const q = qEl.value;
    if (!q.trim()) { clearView("search"); return; }
    if (!target) target = focused() ? { id: focused().id, cwd: focused().cwd || null } : null;
    lastGrep = { q, regex: !!regexEl.checked };
    setState("search", "loading", "Buscando…");
    request("grep", { q, regex: lastGrep.regex });
  }

  // Partes del texto que coinciden, como [inicio, fin) (sin solapes).
  function matches(text, q, regex) {
    const out = [];
    if (!q) return out;
    if (regex) {
      let re;
      try { re = new RegExp(q, "g"); } catch (_) { return out; }
      let m, guard = 0;
      while ((m = re.exec(text)) && guard++ < 200) {
        if (m[0].length === 0) { re.lastIndex++; continue; }
        out.push([m.index, m.index + m[0].length]);
      }
    } else {
      for (let i = text.indexOf(q); i !== -1 && out.length < 200; i = text.indexOf(q, i + q.length)) out.push([i, i + q.length]);
    }
    return out;
  }

  function highlighted(text, q, regex) {
    const span = el("span", "g-text");
    let at = 0;
    for (const [a, b] of matches(text, q, regex)) {
      if (a > at) span.append(document.createTextNode(text.slice(at, a)));
      span.append(el("mark", null, text.slice(a, b)));
      at = b;
    }
    if (at < text.length) span.append(document.createTextNode(text.slice(at)));
    return span;
  }

  function onGrep(r) {
    hitsEl.replaceChildren();
    if (!r.ok || !r.data) { setState("search", "error", r.error || "La búsqueda ha fallado."); return; }
    const hits = Array.isArray(r.data.hits) ? r.data.hits : [];
    const g = lastGrep || { q: "", regex: false };
    hitsEl.setAttribute("role", "listbox");
    hitsEl.setAttribute("aria-label", "Coincidencias");
    for (const h of hits) {
      if (!h || typeof h.path !== "string") continue;
      const li = el("li", "g-hit");
      li.tabIndex = -1;
      li.setAttribute("role", "option");
      const line = Math.max(1, parseInt(h.line, 10) || 1);
      li._hit = { path: h.path, line };
      li.title = `${h.path}:${line}`;
      const text = typeof h.text === "string" ? h.text : String(h.text ?? "");
      li.append(el("span", "g-path", h.path), el("span", "g-line", line), highlighted(text, g.q, g.regex));
      hitsEl.append(li);
    }
    const first = hitsEl.querySelector("li.g-hit");
    if (first) first.tabIndex = 0;
    if (r.data.truncated) hitsEl.append(el("li", "g-trunc", `Se muestran las primeras ${hits.length} coincidencias.`));
    setState("search", "empty", hits.length ? "" : "Sin coincidencias.");
  }

  function openHit(li) {
    if (!li || !li._hit) return;
    request("open", { path: li._hit.path, line: li._hit.line });
  }

  // ---------- pestañas ----------

  function setTab(t, { focusTab = false } = {}) {
    if (!TABS.includes(t)) return;
    tab = t;
    for (const b of tabBtns) {
      const on = b.dataset.tab === t;
      b.setAttribute("aria-selected", String(on));
      b.tabIndex = on ? 0 : -1;
      if (focusTab && on) b.focus({ preventScroll: true });
    }
    for (const [name, v] of Object.entries(views)) v.classList.toggle("active", name === t);
    renderState();
    if (t === "prs" || t === "issues") loadList(t);
    if (t === "search" && !focusTab) qEl.focus({ preventScroll: true });
  }

  // ---------- abrir / cerrar ----------

  function open() {
    if (isOpen) return;
    isOpen = true;
    clearTimeout(closeTimer);
    window.mishSide?.open("git"); // cierra la caja fuerte si estaba abierta
    root.hidden = false;
    root.inert = false;
    root.removeAttribute("aria-hidden");
    setTab("changes");
    // Un frame con el panel ya en el layout para que la transición de entrada se vea; el foco,
    // después de .open (con visibility:hidden no se puede enfocar).
    requestAnimationFrame(() => {
      if (!isOpen) return;
      root.classList.add("open");
      tabBtns.find((b) => b.dataset.tab === tab)?.focus({ preventScroll: true });
    });
    refreshStatus();
    startPoll();
  }

  function close() {
    if (!isOpen) return;
    isOpen = false;
    window.mishSide?.close("git");
    stopPoll();
    clearTimeout(oscTimer);
    const hadFocus = root.contains(document.activeElement);
    root.classList.remove("open");
    root.setAttribute("aria-hidden", "true");
    root.inert = true;
    if (hadFocus) focusTerm();
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => { if (!isOpen) root.hidden = true; }, reducedMotion() ? 0 : ANIM_MS);
  }

  const toggle = () => (isOpen ? close() : open());

  // ---------- refresco automático ----------

  const oscHooked = new WeakSet();

  function hookOsc(p) {
    const parser = p && p.term && p.term.parser;
    if (!parser || oscHooked.has(p.term)) return;
    oscHooked.add(p.term);
    try {
      parser.registerOscHandler(7, () => {
        if (isOpen && focused() === p) {
          clearTimeout(oscTimer);
          oscTimer = setTimeout(() => { if (isOpen) refreshStatus({ silent: true }); }, OSC_DEBOUNCE_MS);
        }
        return false; // que lo procese también app.js (setCwd)
      });
    } catch (_) { /* terminal sin parser (pruebas) */ }
  }

  function poll() {
    const p = focused();
    hookOsc(p);
    const id = p ? p.id : null;
    const cwd = p ? p.cwd || null : null;
    const tid = target ? target.id : null;
    const tcwd = target ? target.cwd : null;
    if (id !== tid || cwd !== tcwd) refreshStatus({ silent: true });
  }

  function startPoll() {
    stopPoll();
    hookOsc(focused());
    pollTimer = setInterval(poll, POLL_MS);
  }
  function stopPoll() { clearInterval(pollTimer); pollTimer = 0; }

  // ---------- eventos ----------

  // ⌘⇧G en captura sobre window, para que xterm no lo vea (e.key varía: "g", "G"…).
  window.addEventListener("keydown", (e) => {
    if (!e.metaKey || !e.shiftKey || e.altKey || e.ctrlKey || e.code !== "KeyG") return;
    if (window.mishHelp?.isOpen?.()) return; // con la ayuda abierta no se abre detrás
    e.preventDefault();
    e.stopPropagation();
    if (!e.repeat) toggle();
  }, true);

  // Otro panel lateral se ha abierto (caja fuerte): este se cierra.
  window.addEventListener("mish-side", (e) => { if (isOpen && e.detail?.open !== "git") close(); });

  function activeList() {
    if (tab === "changes") return { items: fileItems(), kind: "file" };
    if (tab === "prs" && prList) return { items: [...prList.querySelectorAll("li.g-pr")], kind: "url" };
    if (tab === "issues" && issueList) return { items: [...issueList.querySelectorAll("li.g-issue")], kind: "url" };
    if (tab === "search") return { items: [...hitsEl.querySelectorAll("li.g-hit")], kind: "hit" };
    return { items: [], kind: "" };
  }

  function moveTo(li, kind) {
    if (!li) return;
    if (kind === "file") { selectFile(li); return; }
    const { items } = activeList();
    for (const o of items) o.tabIndex = o === li ? 0 : -1;
    li.focus({ preventScroll: true });
    li.scrollIntoView({ block: "nearest" });
  }

  function activate(li, kind) {
    if (kind === "url") openUrl(li);
    else if (kind === "hit") openHit(li);
    else if (kind === "file") {
      // Enter / doble clic en un fichero: abrirlo en el editor (si no está borrado).
      const f = li._file;
      if (f && !(f.kind === "staged" ? f.x === "D" : f.y === "D")) request("open", { path: f.path, line: 1 });
    }
  }

  root.addEventListener("keydown", (e) => {
    const tgt = e.target;

    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      close();
      return;
    }

    // app.js deja pasar ⌘A/⌘C dentro de .side-panel (y en cualquier <input>): se hacen aquí,
    // porque la webview no trae menú Edición. ⌘V llega por window.mishPaste.
    if (e.metaKey) {
      if (e.shiftKey || e.altKey || e.ctrlKey) return;
      const k = (e.key || "").toLowerCase();
      if (k === "a") {
        e.preventDefault();
        if (tgt instanceof HTMLInputElement) tgt.select();
        else {
          const v = tab === "changes" ? (diffEl.querySelector("pre") || diffEl) : views[tab];
          if (v) { const r = document.createRange(); r.selectNodeContents(v); const s = getSelection(); s.removeAllRanges(); s.addRange(r); }
        }
      } else if (k === "c") {
        e.preventDefault();
        let s = "";
        if (tgt instanceof HTMLInputElement) s = tgt.value.slice(tgt.selectionStart ?? 0, tgt.selectionEnd ?? 0);
        else {
          const sel = getSelection();
          if (sel && sel.rangeCount && root.contains(sel.anchorNode)) s = sel.toString();
        }
        if (s) post({ t: "copy", text: s });
      } else if (k === "f") {
        e.preventDefault();
        setTab("search");
        qEl.select();
      }
      return;
    }

    // Pestañas: ← → entre ellas, ↓ baja a la lista.
    if (tabBtns.includes(tgt)) {
      const i = tabBtns.indexOf(tgt);
      if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
        e.preventDefault();
        const n = tabBtns[(i + (e.key === "ArrowRight" ? 1 : tabBtns.length - 1)) % tabBtns.length];
        setTab(n.dataset.tab, { focusTab: true });
        return;
      }
      if (e.key === "ArrowDown") {
        e.preventDefault();
        if (tab === "search") { qEl.focus({ preventScroll: true }); return; }
        const { items, kind } = activeList();
        const sel = items.find((li) => li.classList.contains("selected")) || items[0];
        moveTo(sel, kind);
        return;
      }
    }

    // Buscador: Enter lanza la búsqueda; ↓ baja a los resultados.
    if (tgt === qEl) {
      if (e.key === "Enter") { e.preventDefault(); runGrep(); }
      else if (e.key === "ArrowDown") {
        const first = hitsEl.querySelector("li.g-hit");
        if (first) { e.preventDefault(); moveTo(first, "hit"); }
      }
      return;
    }

    // Listas: ↑ ↓ Inicio Fin, Enter abre.
    const { items, kind } = activeList();
    const idx = items.indexOf(tgt);
    if (idx < 0) return;
    if (e.key === "ArrowDown") { e.preventDefault(); moveTo(items[Math.min(idx + 1, items.length - 1)], kind); }
    else if (e.key === "ArrowUp") {
      e.preventDefault();
      if (idx === 0 && tab === "search") qEl.focus({ preventScroll: true });
      else if (idx === 0) tabBtns.find((b) => b.dataset.tab === tab)?.focus({ preventScroll: true });
      else moveTo(items[idx - 1], kind);
    } else if (e.key === "Home") { e.preventDefault(); moveTo(items[0], kind); }
    else if (e.key === "End") { e.preventDefault(); moveTo(items[items.length - 1], kind); }
    else if (e.key === "Enter" || (e.key === " " && kind !== "file")) { e.preventDefault(); activate(tgt, kind); }
  });

  // Ninguna tecla escrita en el panel debe acabar en el terminal ni en otro sitio.
  root.addEventListener("keydown", (e) => { if (!e.metaKey) e.stopPropagation(); });

  for (const b of tabBtns) b.addEventListener("click", () => setTab(b.dataset.tab, { focusTab: true }));
  $(".g-close")?.addEventListener("click", close);
  $(".g-refresh")?.addEventListener("click", () => {
    if (curRoot) { cache.prs.delete(curRoot); cache.issues.delete(curRoot); }
    refreshStatus();
    if (tab === "prs" || tab === "issues") loadList(tab, { force: true });
    if (tab === "search" && qEl.value.trim()) runGrep();
  });

  filesEl.addEventListener("click", (e) => {
    const li = e.target instanceof Element && e.target.closest("li.g-file");
    if (li) selectFile(li);
  });
  filesEl.addEventListener("dblclick", (e) => {
    const li = e.target instanceof Element && e.target.closest("li.g-file");
    if (li) activate(li, "file");
  });
  for (const list of [prList, issueList]) {
    list?.addEventListener("click", (e) => {
      const li = e.target instanceof Element && e.target.closest("li.g-pr, li.g-issue");
      if (li) { moveTo(li, "url"); openUrl(li); }
    });
  }
  hitsEl.addEventListener("click", (e) => {
    const li = e.target instanceof Element && e.target.closest("li.g-hit");
    if (li) { moveTo(li, "hit"); openHit(li); }
  });
  regexEl?.addEventListener("change", () => { if (lastGrep && qEl.value.trim()) runGrep(); });

  // ⌘V con el foco en el panel: en .g-q lo pega app.js (mishPaste generalizado a inputs); en
  // cualquier otro sitio del panel (lista, diff) se descarta para que no acabe en el terminal.
  const origPaste = window.mishPaste;
  window.mishPaste = (id, b64) => {
    const a = document.activeElement;
    if (isOpen && a && root.contains(a) && !(a instanceof HTMLInputElement)) return;
    if (typeof origPaste === "function") origPaste(id, b64);
  };

  // ---------- Rust → JS ----------

  window.mishGit = (r) => {
    if (!r || typeof r !== "object") return;
    if (r.req !== latest[r.op]) return; // respuesta vieja: hay otra más reciente en camino
    switch (r.op) {
      case "status": onStatus(r); break;
      case "diff": onDiff(r); break;
      case "prs": onList("prs", r); break;
      case "issues": onList("issues", r); break;
      case "grep": onGrep(r); break;
      case "open":
      case "open_url":
        if (!r.ok) setState(tab, "error", r.error || "No se pudo abrir.");
        break;
      default: break;
    }
  };

  window.mishGitPanel = { open, close, toggle, isOpen: () => isOpen };

  // Estado inicial coherente con el marcado (cerrado). tabIndex -1: un clic en el diff deja el
  // foco dentro del panel (si no, iría a <body> y ⌘C copiaría la selección del terminal).
  root.tabIndex = -1;
  diffEl.tabIndex = -1;
  root.inert = true;
  root.setAttribute("aria-hidden", "true");
  for (const [name, v] of Object.entries(views)) v.classList.toggle("active", name === tab);
  for (const b of tabBtns) {
    b.setAttribute("aria-selected", String(b.dataset.tab === tab));
    b.tabIndex = b.dataset.tab === tab ? 0 : -1;
  }
  renderState();
})();
