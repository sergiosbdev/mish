// Mish.app — búsqueda en el scrollback (⌘F) con @xterm/addon-search.
// Cada panel tiene su SearchAddon (p.search, la crea app.js en createPane). La barra #search
// (index.html + search.css) es única y actúa sobre el panel enfocado de la pestaña activa
// (window.mishFocused()); app.js llama a onActivate al cambiar de panel o de pestaña.
// Enter: siguiente · ⇧Enter: anterior · Esc: cerrar · ⌘F con la barra abierta: seleccionar.
(() => {
  "use strict";

  const bar = document.getElementById("search");
  if (!bar) return;
  const q = bar.querySelector(".s-q");
  const count = bar.querySelector(".s-count");
  const btnPrev = bar.querySelector(".s-prev");
  const btnNext = bar.querySelector(".s-next");
  const btnCase = bar.querySelector(".s-case");
  const btnClose = bar.querySelector(".s-close");

  // Colores de las coincidencias (los fondos deben ser #RRGGBB).
  const DECORATIONS = {
    matchBackground: "#4a3c22",
    matchOverviewRuler: "#ecc46e",
    activeMatchBackground: "#b8622f",
    activeMatchColorOverviewRuler: "#e8854c",
  };

  let caseSensitive = false;
  let current = null; // panel sobre el que hay decoraciones
  const hooked = new WeakSet(); // addons con onDidChangeResults ya suscrito

  const focused = () => (typeof window.mishFocused === "function" ? window.mishFocused() : null);
  const isOpen = () => bar.classList.contains("open");
  const opts = (incremental) => ({ incremental, caseSensitive, decorations: DECORATIONS });

  function setCount(text, none) {
    count.textContent = text;
    count.classList.toggle("s-none", none);
    bar.classList.toggle("s-none", none);
  }

  function hook(t) {
    if (!t || !t.search || hooked.has(t.search)) return;
    hooked.add(t.search);
    t.search.onDidChangeResults(({ resultIndex, resultCount }) => {
      if (current !== t || !isOpen() || !q.value) return;
      if (resultCount === 0) setCount("0/0", true);
      // resultIndex -1: se superó el límite de resaltados o no hay coincidencia activa.
      else setCount(resultIndex < 0 ? `?/${resultCount}` : `${resultIndex + 1}/${resultCount}`, false);
    });
  }

  function clear(t) {
    if (!t || !t.search) return;
    try { t.search.clearDecorations(); } catch (_) { /* terminal ya cerrado */ }
  }

  // dir: "next" | "prev" | "inc" (búsqueda mientras se escribe).
  function run(dir) {
    const t = focused();
    if (current && current !== t) clear(current);
    current = t;
    if (!t || !t.search) { setCount("", false); return; }
    hook(t);
    const text = q.value;
    if (!text) { clear(t); setCount("", false); return; }
    let found;
    try {
      found = dir === "prev" ? t.search.findPrevious(text, opts(false)) : t.search.findNext(text, opts(dir === "inc"));
    } catch (_) { found = false; }
    if (!found) setCount("0/0", true);
  }

  function open() {
    if (!isOpen()) {
      bar.classList.add("open");
      if (q.value) run("inc");
    }
    q.focus();
    q.select();
  }

  function close() {
    if (!isOpen()) return;
    bar.classList.remove("open");
    clear(current);
    current = null;
    setCount("", false);
    const t = focused();
    if (t && t.term) t.term.focus();
  }

  // Al cambiar de panel o de pestaña con la barra abierta: misma búsqueda en el nuevo panel.
  function onActivate(t) {
    if (!isOpen()) return;
    if (current && current !== t) clear(current);
    current = null;
    if (q.value) run("inc");
    else setCount("", false);
  }

  q.addEventListener("input", () => run("inc"));
  q.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.metaKey && !e.ctrlKey && !e.altKey) {
      e.preventDefault();
      run(e.shiftKey ? "prev" : "next");
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    } else if (e.metaKey && !e.shiftKey && !e.altKey && !e.ctrlKey) {
      // La webview no trae menú Edición: ⌘A/⌘C se hacen a mano (⌘V va por mishPaste).
      const k = (e.key || "").toLowerCase();
      if (k === "a") {
        e.preventDefault();
        q.select();
      } else if (k === "c") {
        e.preventDefault();
        const s = q.value.slice(q.selectionStart ?? 0, q.selectionEnd ?? 0);
        if (s) { try { window.ipc.postMessage(JSON.stringify({ t: "copy", text: s })); } catch (_) { /* sin IPC */ } }
      }
    }
  });
  // Al perder el foco, se quita el resaltado de la coincidencia activa (queda la selección).
  q.addEventListener("blur", () => {
    if (current && current.search) { try { current.search.clearActiveDecoration(); } catch (_) { /* */ } }
  });

  // Los botones no roban el foco del campo.
  for (const b of [btnPrev, btnNext, btnCase, btnClose]) {
    if (b) b.addEventListener("mousedown", (e) => e.preventDefault());
  }
  if (btnPrev) btnPrev.addEventListener("click", () => run("prev"));
  if (btnNext) btnNext.addEventListener("click", () => run("next"));
  if (btnClose) btnClose.addEventListener("click", close);
  if (btnCase) {
    btnCase.addEventListener("click", () => {
      caseSensitive = !caseSensitive;
      btnCase.setAttribute("aria-pressed", String(caseSensitive));
      clear(current);
      run("inc");
    });
  }

  window.mishSearch = {
    open,
    close,
    onActivate,
    isOpen,
    isFocused: () => document.activeElement === q,
  };
})();
