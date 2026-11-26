// Mish.app — ventana modal con los atajos de teclado (⌘/ o menú «Ayuda → Atajos de teclado»).
// Marcado en index.html (#help) y estilos en help.css; aquí solo la lista y el comportamiento.
// Rust (menú nativo) y app.js (⌘/) llaman a window.mishHelp({open: true}); abrir es idempotente.
(() => {
  "use strict";

  // ---------- lista de atajos (dirigida por datos) ----------
  //
  // Cada entrada: {grupo, teclas, desc}.
  //   teclas: ["⌘", "T"]                  → una combinación, una <kbd> por tecla.
  //           [["⌘", "Enter"], ["⌃", "⌘", "F"]] → alternativas, separadas por «o».
  // Los grupos salen en el orden en que aparecen por primera vez.
  //
  // Cada fase que añada atajos los suma aquí (regla del plan). Desde otro script también se
  // puede hacer en tiempo de ejecución, antes o después de cargar help.js:
  //   window.mishShortcuts.push({ grupo: "Sesiones", teclas: ["⌘", "⌥", "Q"], desc: "…" });
  // La lista se vuelve a pintar cada vez que se abre la ventana, así que basta con el push.
  //
  // Todo lo que hay aquí está comprobado contra app.js, search.js, vault.js y el menú nativo.
  const SHORTCUTS = [
    // app.js: keydown en captura sobre window y eventos de .tab.
    { grupo: "Pestañas", teclas: [["⌘", "T"], ["⌘", "N"]], desc: "Nueva pestaña (en el directorio del panel enfocado)" },
    { grupo: "Pestañas", teclas: ["⌘", "W"], desc: "Cerrar el panel enfocado (y la pestaña si es el último)" },
    { grupo: "Pestañas", teclas: ["Clic central"], desc: "Cerrar la pestaña" },
    { grupo: "Pestañas", teclas: ["⌘", "1 … 9"], desc: "Ir a la pestaña 1 … 9 (o a la última si hay menos)" },
    { grupo: "Pestañas", teclas: [["⌘", "⇧", "]"], ["⌘", "⇧", "["]], desc: "Pestaña siguiente / anterior" },

    // app.js: splitFocused, moveFocus, closePane, divisores (startDrag / dblclick).
    { grupo: "Paneles", teclas: ["⌘", "D"], desc: "Dividir: panel nuevo a la derecha" },
    { grupo: "Paneles", teclas: ["⌘", "⇧", "D"], desc: "Dividir: panel nuevo debajo" },
    { grupo: "Paneles", teclas: ["⌘", "⌥", "← ↑ → ↓"], desc: "Enfocar el panel vecino" },
    { grupo: "Paneles", teclas: ["⌘", "W"], desc: "Cerrar el panel enfocado" },
    { grupo: "Paneles", teclas: ["Clic"], desc: "Enfocar un panel" },
    { grupo: "Paneles", teclas: ["Arrastrar divisor"], desc: "Cambiar el tamaño de los paneles" },
    { grupo: "Paneles", teclas: ["Doble clic en divisor"], desc: "Repartir el espacio al 50\u00a0%" },

    // app.js: keydown (copy/paste/selectAll/clear/setFont) y opciones de xterm.
    { grupo: "Terminal", teclas: ["⌘", "C"], desc: "Copiar la selección" },
    { grupo: "Terminal", teclas: ["⌘", "V"], desc: "Pegar" },
    { grupo: "Terminal", teclas: ["⌘", "A"], desc: "Seleccionar todo" },
    { grupo: "Terminal", teclas: ["⌘", "K"], desc: "Limpiar el terminal y el historial" },
    { grupo: "Terminal", teclas: [["⌘", "+"], ["⌘", "="]], desc: "Aumentar el tamaño de letra" },
    { grupo: "Terminal", teclas: ["⌘", "−"], desc: "Reducir el tamaño de letra" },
    { grupo: "Terminal", teclas: ["⌘", "0"], desc: "Tamaño de letra por defecto" },
    { grupo: "Terminal", teclas: ["⌥"], desc: "Escribe símbolos del teclado: | @ # \\ ~ [ ] { } €" },
    { grupo: "Terminal", teclas: ["Clic en enlace"], desc: "Abrir la URL en el navegador" },

    // search.js (⌘F lo abre app.js).
    { grupo: "Búsqueda", teclas: ["⌘", "F"], desc: "Buscar en el historial del panel (otra vez: seleccionar el texto)" },
    { grupo: "Búsqueda", teclas: ["Enter"], desc: "Coincidencia siguiente" },
    { grupo: "Búsqueda", teclas: ["⇧", "Enter"], desc: "Coincidencia anterior" },
    { grupo: "Búsqueda", teclas: ["Esc"], desc: "Cerrar la búsqueda" },

    // La shell mish (src/histsearch.rs y build_editor en src/main.rs); xterm.js deja pasar ⌃.
    { grupo: "Historial de comandos", teclas: ["⌃", "R"], desc: "Buscar un comando por lo que recuerdes: varias palabras en cualquier orden, sin mayúsculas, o sus letras en orden («dcup» → docker compose up)" },
    { grupo: "Historial de comandos", teclas: [["↑ ↓"], ["⌃", "R"], ["⌃", "S"]], desc: "Con la búsqueda abierta: elegir (más recientes primero)" },
    { grupo: "Historial de comandos", teclas: ["Enter"], desc: "Poner el comando elegido en la línea (no lo ejecuta)" },
    { grupo: "Historial de comandos", teclas: [["Esc"], ["⌃", "G"]], desc: "Cerrar la búsqueda" },
    { grupo: "Historial de comandos", teclas: ["↑"], desc: "Con texto escrito: comandos anteriores que empiezan así" },
    { grupo: "Historial de comandos", teclas: ["→"], desc: "Aceptar la sugerencia gris del historial" },

    // vault.js.
    { grupo: "Caja fuerte", teclas: ["⌘", "⇧", "P"], desc: "Abrir o cerrar la caja fuerte" },
    { grupo: "Caja fuerte", teclas: [["Clic"], ["Enter"]], desc: "Escribir el secreto en el terminal" },
    { grupo: "Caja fuerte", teclas: [["⌥", "Clic"], ["⌥", "Enter"]], desc: "Copiar el secreto (el portapapeles se borra en 30 s)" },
    { grupo: "Caja fuerte", teclas: ["↑ ↓"], desc: "Moverse por la lista" },
    { grupo: "Caja fuerte", teclas: [["⌫"], ["Supr"]], desc: "Borrar la entrada (pide confirmación)" },
    { grupo: "Caja fuerte", teclas: ["⌘", "F"], desc: "Buscar dentro de la caja fuerte" },
    { grupo: "Caja fuerte", teclas: ["Esc"], desc: "Cancelar / vaciar la búsqueda / cerrar" },

    // git.js.
    { grupo: "Git", teclas: ["⌘", "⇧", "G"], desc: "Panel git: cambios, PRs, issues y búsqueda en el repo" },
    { grupo: "Git", teclas: ["← →"], desc: "Cambiar de pestaña (con el foco en las pestañas)" },
    { grupo: "Git", teclas: ["↑ ↓"], desc: "Moverse por la lista (en Cambios, muestra el diff)" },
    { grupo: "Git", teclas: ["Enter"], desc: "Abrir: fichero o coincidencia en el editor, PR o issue en el navegador" },
    { grupo: "Git", teclas: ["Doble clic"], desc: "Abrir el fichero cambiado en el editor" },
    { grupo: "Git", teclas: ["⌘", "F"], desc: "Buscar en el repo (git grep)" },
    { grupo: "Git", teclas: ["Esc"], desc: "Cerrar el panel git" },

    // app.js (fullscreen, quit, barra superior) y menú nativo «Mish» (menu.rs).
    { grupo: "Ventana", teclas: [["⌘", "Enter"], ["⌃", "⌘", "F"]], desc: "Pantalla completa" },
    { grupo: "Ventana", teclas: ["Doble clic en la barra"], desc: "Ampliar la ventana" },
    { grupo: "Ventana", teclas: ["⌘", "M"], desc: "Minimizar" },
    { grupo: "Ventana", teclas: ["⌘", "H"], desc: "Ocultar Mish" },
    { grupo: "Ventana", teclas: ["⌘", "⌥", "H"], desc: "Ocultar las demás apps" },
    { grupo: "Ventana", teclas: ["⌘", "Q"], desc: "Salir (las sesiones siguen vivas)" },

    // app.js: ⌘Q → mishBeforeQuit (desconecta); ⌘⌥Q → quit_all (el daemon las mata todas).
    { grupo: "Sesiones", teclas: ["⌘", "Q"], desc: "Salir (las sesiones siguen vivas)" },
    { grupo: "Sesiones", teclas: ["⌘", "⌥", "Q"], desc: "Salir y cerrar todas las sesiones" },

    // Menú nativo «Mish» (menu.rs), sin atajo; `mish update` hace lo mismo desde el terminal.
    { grupo: "Mish", teclas: ["Menú Mish"], desc: "Buscar actualizaciones… (o «mish update» en el terminal)" },
    { grupo: "Mish", teclas: ["Menú Mish"], desc: "Instalar comando mish en el terminal… (enlace en ~/.local/bin)" },

    // app.js (⌘/) y menú nativo «Ayuda» (menu.rs).
    { grupo: "Ayuda", teclas: ["⌘", "/"], desc: "Mostrar esta ventana de atajos" },
  ];
  // Si otro script ya creó window.mishShortcuts (para añadir los suyos), se conservan.
  if (Array.isArray(window.mishShortcuts)) SHORTCUTS.push(...window.mishShortcuts);
  window.mishShortcuts = SHORTCUTS;

  // ---------- DOM ----------

  const root = document.getElementById("help");
  if (!root) { window.mishHelp = () => {}; return; }
  const card = root.querySelector(".h-card");
  const filter = root.querySelector(".h-filter");
  const btnClose = root.querySelector(".h-close");
  const body = root.querySelector(".h-body");
  // Un clic en la tarjeta (filas, títulos) deja el foco dentro del diálogo y no en <body>.
  card.tabIndex = -1;

  let returnFocus = null; // elemento que tenía el foco al abrir

  const isOpen = () => root.classList.contains("open");
  const post = (o) => { try { window.ipc.postMessage(JSON.stringify(o)); } catch (_) { /* sin IPC */ } };

  // Normaliza para el filtro: minúsculas y sin acentos («pestaña» = «PESTANA»).
  const norm = (s) => String(s).normalize("NFD").replace(/[̀-ͯ]/g, "").toLowerCase();

  // Nombres de los símbolos, para poder filtrar por «cmd», «shift», «opcion»…
  const KEY_WORDS = {
    "⌘": "cmd comando command", "⇧": "shift mayus mayusculas", "⌥": "alt opcion option",
    "⌃": "ctrl control", "⌫": "borrar backspace", "−": "menos -", "Esc": "escape",
    "← ↑ → ↓": "flechas", "↑ ↓": "flechas", "← →": "flechas", "↑": "flecha arriba", "→": "flecha derecha",
  };

  // teclas → lista de alternativas, cada una una lista de teclas.
  const combos = (teclas) => (Array.isArray(teclas[0]) ? teclas : [teclas]);

  function haystack(s) {
    const keys = combos(s.teclas).flat();
    return norm([s.grupo, s.desc, keys.join(" "), keys.join(""), ...keys.map((k) => KEY_WORDS[k] || "")].join(" "));
  }

  // Texto con <mark> en las apariciones de las palabras del filtro (comparando normalizado).
  function highlighted(text, words) {
    const frag = document.createDocumentFragment();
    const chars = [...text];
    // Se normaliza carácter a carácter para conservar posiciones (á → a, ñ → n).
    const flat = chars.map((c) => norm(c));
    if (!words.length || flat.some((c) => c.length !== 1)) { frag.append(text); return frag; }
    const str = flat.join("");
    const hit = new Array(chars.length).fill(false);
    for (const w of words) {
      for (let j = str.indexOf(w); j !== -1; j = str.indexOf(w, j + w.length)) hit.fill(true, j, j + w.length);
    }
    for (let i = 0; i < chars.length;) {
      let j = i;
      while (j < chars.length && hit[j] === hit[i]) j++;
      const piece = chars.slice(i, j).join("");
      if (hit[i]) { const m = document.createElement("mark"); m.textContent = piece; frag.append(m); }
      else frag.append(piece);
      i = j;
    }
    return frag;
  }

  function row(s, words) {
    const r = document.createElement("div");
    r.className = "h-row";
    const keys = document.createElement("span");
    keys.className = "h-keys";
    combos(s.teclas).forEach((combo, i) => {
      if (i) keys.append("o");
      for (const k of combo) {
        const kbd = document.createElement("kbd");
        kbd.textContent = k;
        keys.append(kbd);
      }
    });
    const desc = document.createElement("span");
    desc.className = "h-desc";
    desc.append(highlighted(s.desc, words));
    r.append(keys, desc);
    return r;
  }

  function render() {
    // Varias palabras: tienen que aparecer todas («cmd shift d», «panel cerrar»).
    const words = norm(filter.value).split(/\s+/).filter(Boolean);
    const groups = new Map();
    for (const s of SHORTCUTS) {
      if (!s || !s.grupo || !Array.isArray(s.teclas) || !s.teclas.length) continue;
      if (!groups.has(s.grupo)) groups.set(s.grupo, []);
      if (!words.length || words.every((w) => haystack(s).includes(w))) groups.get(s.grupo).push(s);
    }
    const out = [];
    for (const [name, items] of groups) {
      if (!items.length) continue;
      const sec = document.createElement("section");
      sec.className = "h-group";
      const h = document.createElement("h3");
      h.textContent = name;
      sec.append(h, ...items.map((s) => row(s, words)));
      out.push(sec);
    }
    if (!out.length) {
      const p = document.createElement("p");
      p.className = "h-empty";
      p.textContent = "Ningún atajo coincide";
      out.push(p);
    }
    body.replaceChildren(...out);
  }

  // ---------- abrir / cerrar ----------

  function open() {
    if (!isOpen()) {
      const a = document.activeElement;
      returnFocus = a instanceof HTMLElement && a !== document.body && !root.contains(a) ? a : null;
      filter.value = "";
      render();
      body.scrollTop = 0;
      root.hidden = false;
      root.classList.add("open");
    }
    filter.focus({ preventScroll: true });
    filter.select();
  }

  function close() {
    if (!isOpen()) return;
    root.classList.remove("open");
    root.hidden = true;
    const prev = returnFocus;
    returnFocus = null;
    // Vuelve a donde estaba el foco (p. ej. la búsqueda o la caja fuerte) si sigue visible;
    // si no, al terminal enfocado.
    const xtermTextarea = prev && prev.classList.contains("xterm-helper-textarea");
    if (prev && !xtermTextarea && prev.isConnected && prev.getClientRects().length && !prev.closest("[inert]")) {
      prev.focus({ preventScroll: true });
      return;
    }
    const t = typeof window.mishFocused === "function" ? window.mishFocused() : null;
    if (t && t.term) t.term.focus();
  }

  // ---------- eventos ----------

  filter.addEventListener("input", render);

  btnClose.addEventListener("click", close);
  // Clic en el fondo (fuera de la tarjeta) cierra. mousedown + click en el fondo, para que
  // seleccionar texto del filtro y soltar fuera no cierre la ventana.
  let downOnBackdrop = false;
  root.addEventListener("mousedown", (e) => { downOnBackdrop = !card.contains(e.target); });
  root.addEventListener("click", (e) => {
    if (downOnBackdrop && !card.contains(e.target)) close();
    downOnBackdrop = false;
  });

  root.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      e.preventDefault();
      // Primero vacía el filtro; con el filtro vacío, cierra.
      if (e.target === filter && filter.value) { filter.value = ""; render(); }
      else close();
    } else if (e.key === "Tab") {
      // Foco atrapado: solo el filtro y el botón de cerrar son enfocables.
      const items = [filter, btnClose].filter((el) => el && !el.disabled);
      const i = items.indexOf(document.activeElement);
      e.preventDefault();
      const next = e.shiftKey ? (i <= 0 ? items.length - 1 : i - 1) : (i + 1) % items.length;
      items[next].focus({ preventScroll: true });
    } else if (e.metaKey && !e.altKey && !e.ctrlKey) {
      // La webview no trae menú Edición: ⌘A/⌘C se hacen a mano (⌘V llega por mishPaste).
      const k = (e.key || "").toLowerCase();
      if (k === "a" && e.target === filter && !e.shiftKey) {
        e.preventDefault();
        filter.select();
      } else if (k === "c" && e.target === filter && !e.shiftKey) {
        e.preventDefault();
        const s = filter.value.slice(filter.selectionStart ?? 0, filter.selectionEnd ?? 0);
        if (s) post({ t: "copy", text: s });
      } else if (k === "f" && !e.shiftKey) {
        e.preventDefault();
        filter.focus({ preventScroll: true });
        filter.select();
      }
    }
    // Ninguna tecla del diálogo sigue hacia el terminal u otros paneles.
    e.stopPropagation();
  });
  root.addEventListener("keyup", (e) => e.stopPropagation());
  root.addEventListener("keypress", (e) => e.stopPropagation());

  // Si el foco se escapa (p. ej. un clic programático), vuelve al filtro.
  document.addEventListener("focusin", (e) => {
    if (isOpen() && !root.contains(e.target)) filter.focus({ preventScroll: true });
  });

  // mishHelp() o mishHelp({open: true}) abre (idempotente: si ya está abierta, enfoca el
  // filtro); mishHelp({open: false}) cierra; mishHelp({toggle: true}) alterna.
  window.mishHelp = (opts = {}) => {
    const o = opts && typeof opts === "object" ? opts : {};
    if (o.toggle) { if (isOpen()) close(); else open(); return; }
    if (o.open === false) close();
    else open();
  };
  window.mishHelp.isOpen = isOpen;
})();
