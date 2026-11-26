// Mish.app — árbol de paneles divididos (puro, sin DOM).
//
// Nodo hoja:   {sid}
// Nodo split:  {split: "row" | "col", ratio, a, b}
//   "row": a a la izquierda, b a la derecha · "col": a arriba, b abajo.
//   ratio (0..1) es la fracción del espacio que ocupa a.
// Las funciones no mutan: devuelven un árbol nuevo (los subárboles intactos se comparten).
// Un "path" identifica un nodo desde la raíz: cadena de "a"/"b" ("" = raíz).
(function (global, factory) {
  const api = factory();
  if (typeof module === "object" && module.exports) module.exports = api;
  else global.MishLayout = api;
})(typeof window !== "undefined" ? window : globalThis, () => {
  "use strict";

  const isLeaf = (n) => !!n && n.split === undefined;
  const clampRatio = (r) => (Number.isFinite(r) ? Math.min(0.95, Math.max(0.05, r)) : 0.5);

  // Divide la hoja `sid` en dos: la nueva (`newSid`) queda a la derecha ("row") o debajo ("col").
  // Sin raíz, el nuevo panel es la raíz. Si `sid` no está, devuelve el árbol sin cambios.
  function split(root, sid, dir, newSid) {
    if (!root) return { sid: newSid };
    if (dir !== "row" && dir !== "col") throw new Error(`dirección no válida: ${dir}`);
    if (isLeaf(root)) {
      return root.sid === sid ? { split: dir, ratio: 0.5, a: root, b: { sid: newSid } } : root;
    }
    const a = split(root.a, sid, dir, newSid);
    if (a !== root.a) return { ...root, a };
    const b = split(root.b, sid, dir, newSid);
    return b !== root.b ? { ...root, b } : root;
  }

  // Quita la hoja `sid`: su hermano ocupa el lugar del padre. null si el árbol queda vacío.
  function remove(root, sid) {
    if (!root) return null;
    if (isLeaf(root)) return root.sid === sid ? null : root;
    if (isLeaf(root.a) && root.a.sid === sid) return root.b;
    if (isLeaf(root.b) && root.b.sid === sid) return root.a;
    const a = remove(root.a, sid);
    if (a !== root.a) return a ? { ...root, a } : root.b;
    const b = remove(root.b, sid);
    if (b !== root.b) return b ? { ...root, b } : root.a;
    return root;
  }

  // sids de las hojas en orden (izquierda→derecha, arriba→abajo).
  function leaves(root, out = []) {
    if (!root) return out;
    if (isLeaf(root)) out.push(root.sid);
    else { leaves(root.a, out); leaves(root.b, out); }
    return out;
  }

  // Path de la hoja `sid` ("" si es la raíz) o null si no está.
  function pathOf(root, sid, path = "") {
    if (!root) return null;
    if (isLeaf(root)) return root.sid === sid ? path : null;
    return pathOf(root.a, sid, path + "a") ?? pathOf(root.b, sid, path + "b");
  }

  function nodeAt(root, path) {
    let n = root;
    for (const c of path) {
      if (!n || isLeaf(n)) return null;
      n = c === "a" ? n.a : c === "b" ? n.b : null;
    }
    return n || null;
  }

  // Cambia el ratio del split en `path` (acotado a 0,05..0,95).
  function setRatio(root, path, ratio) {
    const n = nodeAt(root, path);
    if (!n || isLeaf(n)) return root;
    const set = (node, i) => {
      if (i === path.length) return { ...node, ratio: clampRatio(ratio) };
      const k = path[i];
      return { ...node, [k]: set(node[k], i + 1) };
    };
    return set(root, 0);
  }

  // Hoja que debe recibir el foco al cerrar `sid`: la más cercana del subárbol hermano
  // (la primera si `sid` estaba a la izquierda/arriba, la última si estaba a la derecha/abajo).
  function adjacent(root, sid) {
    const p = pathOf(root, sid);
    if (!p) return null; // no está, o es la raíz (no hay hermano)
    const parent = nodeAt(root, p.slice(0, -1));
    const wasA = p[p.length - 1] === "a";
    const sib = leaves(wasA ? parent.b : parent.a);
    return wasA ? sib[0] : sib[sib.length - 1];
  }

  function rectList(rects) {
    if (rects instanceof Map) return [...rects].map(([sid, r]) => ({ sid, r }));
    if (Array.isArray(rects)) return rects.map((x) => ({ sid: x.sid, r: x.rect || x }));
    return Object.entries(rects || {}).map(([k, r]) => ({ sid: /^\d+$/.test(k) ? Number(k) : k, r }));
  }

  // Panel vecino de `fromSid` en la dirección dada ("left" | "right" | "up" | "down"), por
  // geometría. `rects`: Map sid → {left, top, right, bottom} (también vale objeto o lista
  // [{sid, left, …}]). Prefiere los que se solapan en el eje perpendicular y, entre ellos,
  // el más cercano; desempata por distancia entre centros. null si no hay ninguno.
  function neighbor(rects, fromSid, dir) {
    const list = rectList(rects);
    const from = list.find((x) => x.sid === fromSid);
    if (!from) return null;
    const f = from.r;
    const EPS = 2;
    const horiz = dir === "left" || dir === "right";
    let best = null;
    for (const { sid, r } of list) {
      if (sid === fromSid) continue;
      let gap;
      if (dir === "right") gap = r.left - f.right;
      else if (dir === "left") gap = f.left - r.right;
      else if (dir === "down") gap = r.top - f.bottom;
      else if (dir === "up") gap = f.top - r.bottom;
      else return null;
      if (gap < -EPS) continue; // no está en esa dirección
      const overlap = horiz
        ? Math.min(f.bottom, r.bottom) - Math.max(f.top, r.top)
        : Math.min(f.right, r.right) - Math.max(f.left, r.left);
      const cross = horiz
        ? Math.abs((f.top + f.bottom) / 2 - (r.top + r.bottom) / 2)
        : Math.abs((f.left + f.right) / 2 - (r.left + r.right) / 2);
      const score = [overlap > 0 ? 0 : 1, Math.max(0, gap), cross];
      if (!best || score[0] < best.score[0]
        || (score[0] === best.score[0] && (score[1] < best.score[1]
          || (score[1] === best.score[1] && score[2] < best.score[2])))) {
        best = { sid, score };
      }
    }
    return best ? best.sid : null;
  }

  return { split, remove, leaves, neighbor, setRatio, pathOf, nodeAt, adjacent, isLeaf };
});
