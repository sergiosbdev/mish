// Mish.app — gestor de paneles laterales (caja fuerte ⌘⇧P, git ⌘⇧G).
//
// Solo puede haber uno abierto: abrir uno cierra el anterior. Cada panel (vault.js, git.js)
// hace su propia animación y llama aquí al abrir/cerrar; este fichero solo lleva el estado
// común y lo publica:
// - body[data-side="vault"|"git"] mientras haya un panel abierto (el CSS deja sitio a la
//   derecha de #panes); al cerrar se quita cuando el panel ya ha salido (ANIM_MS), para que el
//   terminal recupere el ancho con un solo fit.
// - body.vault-open se mantiene en sincronía con data-side="vault" (lo usan vault.css y
//   notice.css; vault.js también lo pone y lo quita por su cuenta).
// - CustomEvent("mish-side", {detail: {open: nombre|null, prev}}) en window: los demás paneles
//   lo escuchan para cerrarse cuando se abre otro.
// - window.mishRefit() tras cada cambio de ancho.
//
// API: window.mishSide = {open(name), close(name), toggle(name), current()}.
// Se carga ANTES que vault.js y git.js (index.html).
(() => {
  "use strict";

  const ANIM_MS = 180;
  let current = null;
  let closeTimer = 0;

  const reducedMotion = () => {
    try { return window.matchMedia("(prefers-reduced-motion: reduce)").matches; } catch (_) { return false; }
  };

  function refit() {
    if (typeof window.mishRefit === "function") { try { window.mishRefit(); return; } catch (_) { /* cae al evento */ } }
    window.dispatchEvent(new Event("resize"));
  }

  function setBody(name) {
    const b = document.body;
    if (!b) return;
    if (name) b.dataset.side = name;
    else delete b.dataset.side;
    b.classList.toggle("vault-open", name === "vault");
  }

  function emit(open, prev) {
    window.dispatchEvent(new CustomEvent("mish-side", { detail: { open, prev } }));
  }

  function open(name) {
    name = String(name || "");
    if (!name || current === name) return;
    const prev = current;
    current = name;
    clearTimeout(closeTimer);
    setBody(name);
    emit(name, prev); // el panel anterior se cierra al oírlo
    requestAnimationFrame(refit);
  }

  function close(name) {
    if (!current || (name && name !== current)) return;
    const prev = current;
    current = null;
    emit(null, prev);
    clearTimeout(closeTimer);
    closeTimer = setTimeout(() => {
      if (current) return; // se abrió otro mientras salía este
      setBody(null);
      refit();
    }, reducedMotion() ? 0 : ANIM_MS);
  }

  const toggle = (name) => (current === name ? close(name) : open(name));

  window.mishSide = { open, close, toggle, current: () => current };
})();
