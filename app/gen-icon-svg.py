#!/usr/bin/env python3
"""Genera mish-icon.svg (1024x1024): squircle oscuro + prompt "❯ █" (chevron cobre + cursor de bloque).

Solo usa la stdlib. Sin filtros SVG (el renderizador de NSImage los ignora): las sombras
se construyen apilando formas desplazadas con baja opacidad.
"""
import os, sys

def continuous_rect(x, y, w, h, r):
    """Rectángulo con esquinas continuas (curvatura tipo iOS/macOS)."""
    X, Y = x + w, y + h
    k = (1.52866483, 1.08849323, 0.86840689, 0.63149399, 0.07491100, 0.37282401, 0.16906001)
    a, b, c, d, e, f, g = (v * r for v in k)
    P = lambda px, py: f"{px:.2f},{py:.2f}"
    return " ".join([
        f"M{P(x + a, y)}", f"L{P(X - a, y)}",
        f"C{P(X - b, y)} {P(X - c, y)} {P(X - d, y + e)}",
        f"C{P(X - f, y + g)} {P(X - g, y + f)} {P(X - e, y + d)}",
        f"C{P(X, y + c)} {P(X, y + b)} {P(X, y + a)}",
        f"L{P(X, Y - a)}",
        f"C{P(X, Y - b)} {P(X, Y - c)} {P(X - e, Y - d)}",
        f"C{P(X - g, Y - f)} {P(X - f, Y - g)} {P(X - d, Y - e)}",
        f"C{P(X - c, Y)} {P(X - b, Y)} {P(X - a, Y)}",
        f"L{P(x + a, Y)}",
        f"C{P(x + b, Y)} {P(x + c, Y)} {P(x + d, Y - e)}",
        f"C{P(x + f, Y - g)} {P(x + g, Y - f)} {P(x + e, Y - d)}",
        f"C{P(x, Y - c)} {P(x, Y - b)} {P(x, Y - a)}",
        f"L{P(x, y + a)}",
        f"C{P(x, y + b)} {P(x, y + c)} {P(x + e, y + d)}",
        f"C{P(x + g, y + f)} {P(x + f, y + g)} {P(x + d, y + e)}",
        f"C{P(x + c, y)} {P(x + b, y)} {P(x + a, y)}", "Z"])

# Cuerpo: 824px centrado (rejilla estándar de iconos macOS Big Sur+)
body = continuous_rect(100, 100, 824, 824, 168)

# Chevron ❯ (polígono con remates horizontales, como el glifo)
chev = "M290,318 L408,318 L560,512 L408,706 L290,706 L442,512 Z"
# Cursor de bloque
cur = dict(x=612, y=410, w=136, h=204, rx=12)

def shadow_layers(shape, n, dy, op, grow=0):
    out = []
    for i in range(1, n + 1):
        t = i / n
        out.append(f'<g transform="translate(0 {dy * t:.1f})" opacity="{op / n * 1.6:.3f}">{shape}</g>')
    return "\n    ".join(out)

body_shape = f'<path d="{body}" fill="#000"/>'
fg_shape = (f'<path d="{chev}" fill="#000" stroke="#000" stroke-width="18" stroke-linejoin="round"/>'
            f'<rect x="{cur["x"]}" y="{cur["y"]}" width="{cur["w"]}" height="{cur["h"]}" rx="{cur["rx"]}" fill="#000"/>')

scan = "".join(f'<rect x="100" y="{y}" width="824" height="2"/>' for y in range(124, 924, 16))

svg = f'''<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="bg" x1="0" y1="0" x2="0.25" y2="1">
      <stop offset="0" stop-color="#23324d"/>
      <stop offset="0.5" stop-color="#151f33"/>
      <stop offset="1" stop-color="#0a101c"/>
    </linearGradient>
    <radialGradient id="glow" cx="0.5" cy="0.54" r="0.42">
      <stop offset="0" stop-color="#e2733a" stop-opacity="0.20"/>
      <stop offset="0.6" stop-color="#e2733a" stop-opacity="0.05"/>
      <stop offset="1" stop-color="#e2733a" stop-opacity="0"/>
    </radialGradient>
    <linearGradient id="copper" x1="0" y1="0" x2="0.5" y2="1">
      <stop offset="0" stop-color="#f6ad74"/>
      <stop offset="0.45" stop-color="#e47a3e"/>
      <stop offset="1" stop-color="#ad4a1f"/>
    </linearGradient>
    <linearGradient id="cursor" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#fdf4e8"/>
      <stop offset="1" stop-color="#ead6bf"/>
    </linearGradient>
    <linearGradient id="rim" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.28"/>
      <stop offset="0.2" stop-color="#ffffff" stop-opacity="0.06"/>
      <stop offset="0.8" stop-color="#ffffff" stop-opacity="0.02"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0.08"/>
    </linearGradient>
    <linearGradient id="sheen" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#ffffff" stop-opacity="0.07"/>
      <stop offset="1" stop-color="#ffffff" stop-opacity="0"/>
    </linearGradient>
    <clipPath id="clip"><path d="{body}"/></clipPath>
  </defs>

  <!-- sombra del cuerpo -->
  {shadow_layers(body_shape, 8, 14, 0.28)}

  <!-- cuerpo -->
  <path d="{body}" fill="url(#bg)"/>
  <g clip-path="url(#clip)">
    <rect width="1024" height="1024" fill="url(#glow)"/>
    <g fill="#ffffff" opacity="0.022">{scan}</g>
    <ellipse cx="512" cy="40" rx="620" ry="400" fill="url(#sheen)"/>
  </g>
  <path d="{body}" fill="none" stroke="url(#rim)" stroke-width="3"/>

  <!-- sombra del prompt -->
  {shadow_layers(fg_shape, 6, 12, 0.40)}

  <!-- prompt: ❯ cobre + cursor de bloque -->
  <path d="{chev}" fill="url(#copper)" stroke="url(#copper)" stroke-width="18" stroke-linejoin="round"/>
  <path d="M290,318 L408,318 L560,512" fill="none" stroke="#ffd2ad" stroke-opacity="0.35" stroke-width="4" stroke-linejoin="round"/>
  <rect x="{cur["x"]}" y="{cur["y"]}" width="{cur["w"]}" height="{cur["h"]}" rx="{cur["rx"]}" fill="url(#cursor)"/>
</svg>
'''
out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(os.path.dirname(os.path.abspath(__file__)), "mish-icon.svg")
with open(out, "w") as fh:
    fh.write(svg)
print(out)
