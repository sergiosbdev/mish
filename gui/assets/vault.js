// Mish.app — caja fuerte en un panel lateral (⌘⇧P).
//
// Los secretos nunca llegan al webview: Rust solo nos manda nombres y usuarios
// (window.mishVault) y es él quien escribe en el PTY (vault_type) o copia (vault_copy).
// Aquí solo pasan por los <input> de contraseña, que se vacían nada más enviarlos.
//
// Supuestos sobre app.js (script clásico, cargado ANTES que este fichero):
// - window.mishFocused() devuelve el panel enfocado de la pestaña activa ({id: sid, term,
//   fit, …}) o null; vault_type escribe en ese sid. window.mishRefit() reajusta los paneles
//   de la pestaña activa. Se usan con guardas; sin ellos se emite el evento `resize`.
// - Este fichero va dentro de una IIFE para no redeclarar nombres de app.js (send, tabs…).
// - Rust llama a window.mishPaste(id, b64) tras ⌘V; lo envolvemos para que, si el foco está
//   en un campo del panel, el texto vaya ahí y no al terminal.
(() => {
  "use strict";

  const ANIM_MS = 180;
  const FLASH_MS = 2400;
  const MIN_PW = 8;
  const TINTS = ["#e8854c", "#8ab8ea", "#9dd08c", "#ecc46e", "#e6a2c4", "#76d3bf", "#f2675f"];

  // ---------- entorno de app.js ----------

  const post = (o) => {
    try { window.ipc.postMessage(JSON.stringify(o)); } catch (_) { /* sin IPC (pruebas) */ }
  };

  // Panel enfocado de la pestaña activa (con paneles divididos, no la pestaña entera).
  function activeTab() {
    try { return (typeof window.mishFocused === "function" && window.mishFocused()) || null; } catch (_) { return null; }
  }

  function refit() {
    if (typeof window.mishRefit === "function") { try { window.mishRefit(); return; } catch (_) { /* cae al evento */ } }
    window.dispatchEvent(new Event("resize"));
  }

  function focusTerm() {
    const t = activeTab();
    if (t && t.term) t.term.focus();
    else if (document.activeElement && root.contains(document.activeElement)) document.activeElement.blur();
  }

  const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const norm = (s) => String(s || "").normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

  function tintFor(name) {
    let h = 0;
    for (const c of String(name)) h = (h * 31 + c.codePointAt(0)) >>> 0;
    return TINTS[h % TINTS.length];
  }

  // ---------- iconos ----------

  const svg = (d, size = 14) =>
    `<svg width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" ` +
    `stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${d}</svg>`;
  const ICON = {
    lock: svg('<rect x="4.5" y="10.5" width="15" height="10" rx="2.5"/><path d="M8 10.5V7.5a4 4 0 0 1 8 0v3"/>'),
    lockBig: svg('<rect x="4.5" y="10.5" width="15" height="10" rx="2.5"/><path d="M8 10.5V7.5a4 4 0 0 1 8 0v3"/><path d="M12 14.5v2"/>', 22),
    key: svg('<circle cx="8" cy="15" r="4"/><path d="m10.8 12.2 8.2-8.2M16 7l2.5 2.5M14 9l2 2"/>', 22),
    search: svg('<circle cx="11" cy="11" r="6.5"/><path d="m20 20-4.2-4.2"/>', 13),
    trash: svg('<path d="M4.5 7h15M9.5 7V5h5v2M6.5 7l1 12.5h9l1-12.5M10 11v5M14 11v5"/>', 13),
    eye: svg('<path d="M2.5 12s3.5-6.5 9.5-6.5 9.5 6.5 9.5 6.5-3.5 6.5-9.5 6.5S2.5 12 2.5 12Z"/><circle cx="12" cy="12" r="2.8"/>', 14),
    eyeOff: svg('<path d="M3 3l18 18M10.6 5.6A9.6 9.6 0 0 1 12 5.5c6 0 9.5 6.5 9.5 6.5a16 16 0 0 1-3 3.8M6.6 6.7C3.9 8.4 2.5 12 2.5 12s3.5 6.5 9.5 6.5c1.6 0 3-.4 4.2-1"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/>', 14),
  };

  const INPUT = 'autocomplete="off" autocapitalize="off" autocorrect="off" spellcheck="false"';

  // ---------- DOM ----------

  const root = document.createElement("aside");
  root.id = "vault";
  root.className = "vault";
  root.setAttribute("aria-label", "Caja fuerte");
  root.setAttribute("aria-hidden", "true");
  root.inert = true;
  root.dataset.state = "loading";
  root.innerHTML = `
    <header class="v-head">
      <span class="v-dot" aria-hidden="true"></span>
      <h2 class="v-title">Caja fuerte</h2>
      <button class="v-btn v-ghost v-sm v-lock" type="button" title="Bloquear la caja fuerte" hidden>${ICON.lock}<span>Bloquear</span></button>
      <button class="v-icon v-close" type="button" title="Cerrar (Esc)" aria-label="Cerrar panel">×</button>
    </header>

    <section class="v-view v-gate" data-view="loading" aria-live="polite">
      <p class="v-muted">Consultando la caja fuerte…</p>
    </section>

    <form class="v-view v-gate" data-view="none" novalidate>
      <div class="v-emblem">${ICON.key}</div>
      <h3>Crea la contraseña maestra</h3>
      <p class="v-muted">Cifra <code>~/.config/mish/vault.bin</code>. Si la olvidas no hay forma de recuperar las entradas.</p>
      <label class="v-field"><span>Contraseña maestra</span>
        <input class="v-pw" type="password" name="pw1" ${INPUT} minlength="${MIN_PW}" required></label>
      <label class="v-field"><span>Repítela</span>
        <input class="v-pw" type="password" name="pw2" ${INPUT} required></label>
      <p class="v-msg" role="alert"></p>
      <button class="v-btn v-primary v-block" type="submit">Crear caja fuerte</button>
    </form>

    <form class="v-view v-gate" data-view="locked" novalidate>
      <div class="v-emblem">${ICON.lockBig}</div>
      <h3>Caja fuerte bloqueada</h3>
      <p class="v-muted">Introduce la contraseña maestra para ver tus entradas.</p>
      <label class="v-field"><span class="v-sr">Contraseña maestra</span>
        <input class="v-pw" type="password" name="pw" placeholder="Contraseña maestra" ${INPUT} required></label>
      <p class="v-msg" role="alert"></p>
      <button class="v-btn v-primary v-block" type="submit">Desbloquear</button>
    </form>

    <section class="v-view v-browse" data-view="unlocked">
      <div class="v-search">
        ${ICON.search}
        <input type="text" name="q" role="searchbox" placeholder="Buscar por nombre o usuario" aria-label="Buscar entradas" ${INPUT}>
      </div>
      <p class="v-msg v-msg-list" role="alert"></p>
      <ul class="v-list" aria-label="Entradas"></ul>
      <p class="v-empty v-muted" hidden></p>
      <form class="v-add" novalidate hidden>
        <div class="v-add-title">Nueva entrada</div>
        <label class="v-field"><span>Nombre</span>
          <input type="text" name="name" placeholder="p. ej. github" ${INPUT} required></label>
        <label class="v-field"><span>Usuario</span>
          <input type="text" name="user" placeholder="opcional" ${INPUT}></label>
        <label class="v-field"><span>Secreto</span>
          <span class="v-reveal-wrap">
            <input class="v-pw" type="password" name="secret" ${INPUT} required>
            <button class="v-icon v-reveal" type="button" aria-label="Mostrar secreto" aria-pressed="false" title="Mostrar">${ICON.eye}</button>
          </span></label>
        <p class="v-msg" role="alert"></p>
        <div class="v-actions">
          <button class="v-btn v-ghost v-sm v-add-cancel" type="button">Cancelar</button>
          <button class="v-btn v-primary v-sm" type="submit">Guardar</button>
        </div>
      </form>
      <footer class="v-foot">
        <button class="v-btn v-ghost v-sm v-add-open" type="button">+ Añadir</button>
        <span class="v-hint">clic escribe · <kbd>⌥</kbd>clic copia</span>
      </footer>
    </section>`;
  document.body.appendChild(root);

  const $ = (sel, el = root) => el.querySelector(sel);
  const views = [...root.querySelectorAll(".v-view")];
  const lockBtn = $(".v-lock");
  const createForm = $('[data-view="none"]');
  const unlockForm = $('[data-view="locked"]');
  const browse = $('[data-view="unlocked"]');
  const search = $('input[name="q"]', browse);
  const list = $(".v-list", browse);
  const emptyEl = $(".v-empty", browse);
  const listMsg = $(".v-msg-list", browse);
  const addForm = $(".v-add", browse);
  const addOpenBtn = $(".v-add-open", browse);
  const revealBtn = $(".v-reveal", addForm);

  // ---------- estado ----------

  let isOpen = false;
  let closeTimer = 0;
  let state = null;          // null (sin respuesta aún) | "none" | "locked" | "unlocked"
  let entries = [];          // [{name, user}] — nunca secretos
  let busy = null;           // botón esperando respuesta de Rust
  let busyTimer = 0;
  let confirming = null;     // nombre de la entrada pendiente de confirmar borrado
  let flash = null;          // {name, text}
  let flashTimer = 0;

  // ---------- utilidades de UI ----------

  function setMsg(el, text) {
    el.textContent = text || "";
  }

  function clearMsgs() {
    for (const m of root.querySelectorAll(".v-msg")) m.textContent = "";
  }

  function setBusy(btn, label) {
    clearBusy();
    busy = { btn, text: btn.textContent };
    btn.disabled = true;
    btn.textContent = label;
    btn.classList.add("v-busy");
    // Si Rust no contesta, no dejamos el botón bloqueado para siempre.
    busyTimer = setTimeout(clearBusy, 20000);
  }

  function clearBusy() {
    clearTimeout(busyTimer);
    if (!busy) return;
    busy.btn.disabled = false;
    busy.btn.textContent = busy.text;
    busy.btn.classList.remove("v-busy");
    busy = null;
  }

  function wipePasswords() {
    for (const i of root.querySelectorAll("input.v-pw")) i.value = "";
    setReveal(false);
    $('[name="pw2"]', createForm).closest(".v-field").classList.remove("v-bad");
  }

  function setReveal(on) {
    const input = $('input[name="secret"]', addForm);
    input.type = on ? "text" : "password";
    revealBtn.innerHTML = on ? ICON.eyeOff : ICON.eye;
    revealBtn.setAttribute("aria-pressed", String(on));
    revealBtn.setAttribute("aria-label", on ? "Ocultar secreto" : "Mostrar secreto");
    revealBtn.title = on ? "Ocultar" : "Mostrar";
  }

  // ---------- render ----------

  function currentView() {
    return state || "loading";
  }

  function render() {
    const v = currentView();
    root.dataset.state = v;
    for (const el of views) el.hidden = el.dataset.view !== v;
    lockBtn.hidden = v !== "unlocked";
    if (v === "unlocked") renderList();
  }

  function filtered() {
    const q = norm(search.value.trim());
    return entries
      .filter((e) => !q || norm(e.name).includes(q) || norm(e.user).includes(q))
      .sort((a, b) => a.name.localeCompare(b.name, "es", { sensitivity: "base" }));
  }

  function renderList() {
    // Conserva el foco dentro de la lista al re-pintar.
    const fa = document.activeElement;
    const keep = fa && list.contains(fa)
      ? { name: fa.closest(".v-row")?.dataset.name, cls: fa.className.split(" ").find((c) => c.startsWith("v-")) }
      : null;

    const items = filtered();
    list.replaceChildren(...items.map(row));

    if (!entries.length) {
      emptyEl.textContent = "Aún no hay entradas. Añade la primera con «+ Añadir».";
      emptyEl.hidden = false;
    } else if (!items.length) {
      emptyEl.textContent = `Nada coincide con «${search.value.trim()}».`;
      emptyEl.hidden = false;
    } else {
      emptyEl.hidden = true;
    }

    if (keep && keep.name != null) {
      const r = [...list.children].find((li) => li.dataset.name === keep.name);
      const target = r && (r.querySelector("." + keep.cls) || r.querySelector("button"));
      if (target) target.focus({ preventScroll: true });
      else search.focus({ preventScroll: true });
    }
  }

  function row(e) {
    const li = document.createElement("li");
    li.className = "v-row";
    li.dataset.name = e.name;

    if (confirming === e.name) {
      li.classList.add("v-confirming");
      const box = document.createElement("div");
      box.className = "v-confirm";
      const q = document.createElement("span");
      q.className = "v-confirm-q";
      q.append("¿Borrar ");
      const b = document.createElement("b");
      b.textContent = e.name;
      q.append(b, "?");
      const yes = button("v-btn v-danger v-xs v-yes", "Borrar");
      const no = button("v-btn v-ghost v-xs v-no", "Cancelar");
      yes.addEventListener("click", () => {
        confirming = null;
        post({ t: "vault_delete", name: e.name });
        renderList();
        search.focus({ preventScroll: true });
      });
      no.addEventListener("click", () => cancelConfirm(e.name));
      box.append(q, no, yes);
      li.append(box);
      return li;
    }

    const main = button("v-entry", "");
    main.title = "Clic: escribir en la terminal · ⌥clic: copiar";
    const av = document.createElement("span");
    av.className = "v-avatar";
    av.setAttribute("aria-hidden", "true");
    const tint = tintFor(e.name);
    av.style.color = tint;
    av.style.background = tint + "24";
    av.textContent = ([...e.name.trim()][0] || "•").toUpperCase();

    const txt = document.createElement("span");
    txt.className = "v-text";
    const nm = document.createElement("span");
    nm.className = "v-name";
    nm.textContent = e.name;
    const us = document.createElement("span");
    us.className = "v-user";
    if (flash && flash.name === e.name) {
      us.classList.add("v-ok");
      us.textContent = flash.text;
    } else if (e.user) {
      us.textContent = e.user;
    } else {
      us.classList.add("v-nouser");
      us.textContent = "sin usuario";
    }
    txt.append(nm, us);
    main.append(av, txt);
    main.setAttribute("aria-label", `${e.name}${e.user ? ", " + e.user : ""}`);

    main.addEventListener("click", (ev) => pick(e.name, ev.altKey));
    main.addEventListener("keydown", (ev) => {
      if (ev.key === "Enter") { ev.preventDefault(); pick(e.name, ev.altKey); }
      else if (ev.key === "Backspace" || ev.key === "Delete") { ev.preventDefault(); askDelete(e.name); }
    });

    const del = button("v-icon v-del", "");
    del.innerHTML = ICON.trash;
    del.title = "Borrar";
    del.setAttribute("aria-label", `Borrar ${e.name}`);
    del.tabIndex = -1; // se llega con ⌫ desde la entrada; evita duplicar paradas de Tab
    del.addEventListener("click", () => askDelete(e.name));

    li.append(main, del);
    return li;
  }

  function button(cls, text) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = cls;
    if (text) b.textContent = text;
    return b;
  }

  // ---------- acciones ----------

  function pick(name, copy) {
    if (copy) {
      post({ t: "vault_copy", name });
      showFlash(name, "Copiado (se borra en 30 s)");
      return;
    }
    const t = activeTab();
    if (!t) { setMsg(listMsg, "No hay ninguna pestaña activa."); return; }
    post({ t: "vault_type", name, id: t.id });
    showFlash(name, "Escrito en la terminal");
    focusTerm();
  }

  function showFlash(name, text) {
    clearTimeout(flashTimer);
    flash = { name, text };
    renderList();
    flashTimer = setTimeout(() => { flash = null; if (state === "unlocked") renderList(); }, FLASH_MS);
  }

  function askDelete(name) {
    confirming = name;
    renderList();
    const r = [...list.children].find((li) => li.dataset.name === name);
    if (r) r.querySelector(".v-no").focus({ preventScroll: true });
  }

  function cancelConfirm(name) {
    confirming = null;
    renderList();
    const r = [...list.children].find((li) => li.dataset.name === name);
    if (r) r.querySelector(".v-entry").focus({ preventScroll: true });
  }

  function openAdd() {
    addForm.hidden = false;
    addOpenBtn.hidden = true;
    setMsg($(".v-msg", addForm), "");
    $('input[name="name"]', addForm).focus({ preventScroll: true });
  }

  function closeAdd(focusBack = true) {
    for (const i of addForm.querySelectorAll("input")) i.value = "";
    setReveal(false);
    setMsg($(".v-msg", addForm), "");
    addForm.hidden = true;
    addOpenBtn.hidden = false;
    if (focusBack && isOpen) addOpenBtn.focus({ preventScroll: true });
  }

  function resetBrowse() {
    confirming = null;
    flash = null;
    search.value = "";
    if (!addForm.hidden) closeAdd(false);
  }

  function focusPrimary() {
    const v = currentView();
    let el = null;
    if (v === "none") el = $('input[name="pw1"]', createForm);
    else if (v === "locked") el = $('input[name="pw"]', unlockForm);
    else if (v === "unlocked") el = addForm.hidden ? search : $('input[name="name"]', addForm);
    if (el) el.focus({ preventScroll: true });
    else root.querySelector(".v-close").focus({ preventScroll: true });
  }

  // ---------- abrir / cerrar ----------

  function open() {
    if (isOpen) return;
    isOpen = true;
    clearTimeout(closeTimer);
    document.body.classList.add("vault-open");
    window.mishSide?.open("vault"); // side.js: cierra el panel git si estaba abierto
    root.inert = false;
    root.removeAttribute("aria-hidden");
    root.classList.add("open");
    render();
    post({ t: "vault_status" });
    requestAnimationFrame(() => { refit(); focusPrimary(); });
  }

  function close() {
    if (!isOpen) return;
    isOpen = false;
    window.mishSide?.close("vault");
    const hadFocus = root.contains(document.activeElement);
    wipePasswords();
    confirming = null;
    root.classList.remove("open");
    root.setAttribute("aria-hidden", "true");
    root.inert = true;
    if (hadFocus) focusTerm();
    // El terminal recupera el ancho cuando el panel ya ha salido (un solo fit, sin tirones).
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => {
      document.body.classList.remove("vault-open");
      refit();
    }, reducedMotion() ? 0 : ANIM_MS);
  }

  const toggle = () => (isOpen ? close() : open());
  // Otro panel lateral (git) se ha abierto: este se cierra (side.js, exclusión mutua).
  window.addEventListener("mish-side", (e) => { if (isOpen && e.detail?.open !== "vault") close(); });

  // ---------- eventos ----------

  // ⌘⇧P: en captura sobre window, para que xterm no llegue a verlo.
  window.addEventListener("keydown", (e) => {
    if (!e.metaKey || !e.shiftKey || e.altKey || e.ctrlKey) return;
    if (e.code !== "KeyP" && (e.key || "").toLowerCase() !== "p") return;
    if (window.mishHelp?.isOpen?.()) return; // con la ayuda abierta no se abre detrás
    e.preventDefault();
    e.stopPropagation();
    if (!e.repeat) toggle();
  }, true);

  root.addEventListener("keydown", (e) => {
    const tgt = e.target;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      if (confirming) cancelConfirm(confirming);
      else if (!addForm.hidden && addForm.contains(tgt)) closeAdd();
      else if (tgt === search && search.value) { search.value = ""; renderList(); }
      else close();
      return;
    }

    // app.js captura ⌘A/⌘C/⌘V para el terminal (y hace preventDefault); los rehacemos aquí
    // para los campos del panel. ⌘V llega por window.mishPaste (ver más abajo).
    if (e.metaKey) {
      const k = (e.key || "").toLowerCase();
      const inInput = tgt instanceof HTMLInputElement;
      if (k === "a" && inInput && !e.shiftKey) {
        tgt.select();
        const t = activeTab();
        if (t && t.term) t.term.clearSelection();
      } else if (k === "c" && inInput && !e.shiftKey && tgt.type !== "password") {
        const s = tgt.value.slice(tgt.selectionStart ?? 0, tgt.selectionEnd ?? 0);
        if (s) post({ t: "copy", text: s });
      } else if (k === "f" && !e.ctrlKey && state === "unlocked") {
        e.preventDefault();
        search.focus({ preventScroll: true });
        search.select();
      }
      return;
    }

    // Flechas en la lista (y desde el buscador).
    if (state !== "unlocked") return;
    const entriesBtns = [...list.querySelectorAll(".v-entry")];
    const idx = entriesBtns.indexOf(tgt);
    if (e.key === "ArrowDown") {
      if (tgt === search && entriesBtns[0]) { e.preventDefault(); entriesBtns[0].focus({ preventScroll: true }); }
      else if (idx >= 0) { e.preventDefault(); (entriesBtns[idx + 1] || entriesBtns[idx]).focus({ preventScroll: true }); }
    } else if (e.key === "ArrowUp" && idx >= 0) {
      e.preventDefault();
      (idx === 0 ? search : entriesBtns[idx - 1]).focus({ preventScroll: true });
    } else if (e.key === "Home" && idx >= 0 && entriesBtns[0]) {
      e.preventDefault(); entriesBtns[0].focus({ preventScroll: true });
    } else if (e.key === "End" && idx >= 0 && entriesBtns.length) {
      e.preventDefault(); entriesBtns[entriesBtns.length - 1].focus({ preventScroll: true });
    } else if (e.key === "Enter" && tgt === search) {
      e.preventDefault();
      const first = filtered()[0];
      if (first) pick(first.name, e.altKey);
    }
  });

  // Ninguna tecla escrita en el panel debe acabar en otro sitio.
  root.addEventListener("keydown", (e) => { if (!e.metaKey) e.stopPropagation(); });

  root.querySelector(".v-close").addEventListener("click", close);

  lockBtn.addEventListener("click", () => {
    post({ t: "vault_lock" });
    resetBrowse();
  });

  // Crear: dos campos que deben coincidir.
  const pw1 = $('input[name="pw1"]', createForm);
  const pw2 = $('input[name="pw2"]', createForm);
  const createMsg = $(".v-msg", createForm);
  const checkMatch = () => {
    const bad = pw2.value.length > 0 && pw1.value !== pw2.value;
    pw2.closest(".v-field").classList.toggle("v-bad", bad);
    setMsg(createMsg, bad ? "Las contraseñas no coinciden." : "");
    return !bad;
  };
  pw1.addEventListener("input", () => { if (pw2.value) checkMatch(); else setMsg(createMsg, ""); });
  pw2.addEventListener("input", checkMatch);
  createForm.addEventListener("submit", (e) => {
    e.preventDefault();
    if (busy) return;
    if (pw1.value.length < MIN_PW) {
      setMsg(createMsg, `Usa al menos ${MIN_PW} caracteres.`);
      pw1.focus({ preventScroll: true });
      return;
    }
    if (pw1.value !== pw2.value) {
      checkMatch();
      pw2.focus({ preventScroll: true });
      return;
    }
    const pw = pw1.value;
    pw1.value = "";
    pw2.value = "";
    post({ t: "vault_create", pw });
    setBusy($('button[type="submit"]', createForm), "Creando…");
  });

  // Desbloquear: Enter envía.
  const unlockPw = $('input[name="pw"]', unlockForm);
  const unlockMsg = $(".v-msg", unlockForm);
  unlockPw.addEventListener("input", () => setMsg(unlockMsg, ""));
  unlockForm.addEventListener("submit", (e) => {
    e.preventDefault();
    if (busy || !unlockPw.value) return;
    const pw = unlockPw.value;
    unlockPw.value = "";
    post({ t: "vault_unlock", pw });
    setBusy($('button[type="submit"]', unlockForm), "Desbloqueando…");
  });

  // Lista.
  search.addEventListener("input", () => { confirming = null; setMsg(listMsg, ""); renderList(); });
  addOpenBtn.addEventListener("click", openAdd);
  $(".v-add-cancel", addForm).addEventListener("click", () => closeAdd());
  revealBtn.addEventListener("click", () => {
    const input = $('input[name="secret"]', addForm);
    setReveal(input.type === "password");
    input.focus({ preventScroll: true });
  });
  addForm.addEventListener("input", () => setMsg($(".v-msg", addForm), ""));
  addForm.addEventListener("submit", (e) => {
    e.preventDefault();
    const nameEl = $('input[name="name"]', addForm);
    const userEl = $('input[name="user"]', addForm);
    const secEl = $('input[name="secret"]', addForm);
    const msg = $(".v-msg", addForm);
    const name = nameEl.value.trim();
    const user = userEl.value.trim();
    if (!name) { setMsg(msg, "Ponle un nombre."); nameEl.focus({ preventScroll: true }); return; }
    if (entries.some((x) => norm(x.name) === norm(name))) {
      setMsg(msg, "Ya hay una entrada con ese nombre."); nameEl.focus({ preventScroll: true }); nameEl.select(); return;
    }
    if (!secEl.value) { setMsg(msg, "Falta el secreto."); secEl.focus({ preventScroll: true }); return; }
    const secret = secEl.value;
    secEl.value = "";
    post({ t: "vault_add", name, user, secret });
    closeAdd(false);
    search.focus({ preventScroll: true });
  });

  // ⌘V con el foco en el panel: app.js pide el portapapeles a Rust para la pestaña activa
  // y Rust responde con mishPaste; lo desviamos al campo enfocado.
  const origPaste = window.mishPaste;
  const utf8 = new TextDecoder();
  window.mishPaste = (id, b64) => {
    const el = document.activeElement;
    if (isOpen && el instanceof HTMLInputElement && root.contains(el)) {
      let text = "";
      try { text = utf8.decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0))); } catch (_) { return; }
      text = text.replace(/\r?\n|\r/g, el.type === "password" ? "" : " ").replace(/\s+$/, "");
      el.setRangeText(text, el.selectionStart ?? el.value.length, el.selectionEnd ?? el.value.length, "end");
      el.dispatchEvent(new Event("input", { bubbles: true }));
      return;
    }
    if (typeof origPaste === "function") origPaste(id, b64);
  };

  // ---------- Rust → JS ----------

  window.mishVault = (s) => {
    s = s || {};
    const prev = state;
    const next = ["none", "locked", "unlocked"].includes(s.state) ? s.state : "locked";
    entries = Array.isArray(s.entries)
      ? s.entries.map((e) => ({ name: String(e && e.name != null ? e.name : ""), user: String(e && e.user != null ? e.user : "") }))
      : [];
    if (prev === "unlocked" && next !== "unlocked") resetBrowse();
    if (confirming && !entries.some((e) => e.name === confirming)) confirming = null;
    state = next;
    clearBusy();
    clearMsgs();
    render();

    if (s.error) {
      const target = next === "none" ? createMsg : next === "locked" ? unlockMsg : listMsg;
      setMsg(target, String(s.error));
    }
    if (isOpen && prev !== next) focusPrimary();
    else if (isOpen && s.error && next === "locked") unlockPw.focus({ preventScroll: true });
  };

  render();
})();
