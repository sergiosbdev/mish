#!/bin/zsh
# Regenera mish.icns desde gen-icon-svg.py → mish-icon.svg → PNG 1024 (NSImage, con alfa) → iconset → icns.
# Uso: ./make-icon.sh [dir_trabajo]
set -e
here=${0:A:h}
work=${1:-$(mktemp -d)}
mkdir -p "$work"
python3 "$here/gen-icon-svg.py" "$here/mish-icon.svg" >/dev/null
cat > "$work/render.swift" <<'SWIFT'
import AppKit
let a = CommandLine.arguments
guard let img = NSImage(contentsOf: URL(fileURLWithPath: a[1])) else { fatalError("no se pudo leer el SVG") }
let s = Int(a[3])!
let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: s, pixelsHigh: s, bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
NSGraphicsContext.current!.imageInterpolation = .high
img.draw(in: NSRect(x: 0, y: 0, width: s, height: s))
NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: a[2]))
SWIFT
swift "$work/render.swift" "$here/mish-icon.svg" "$work/mish-1024.png" 1024
set=$work/mish.iconset
rm -rf "$set"; mkdir -p "$set"
for s in 16 32 64 128 256 512; do
  sips -z $s $s "$work/mish-1024.png" --out "$set/icon_${s}x${s}.png" >/dev/null
  d=$((s * 2))
  sips -z $d $d "$work/mish-1024.png" --out "$set/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$set" -o "$here/mish.icns"
echo "OK: $here/mish.icns (PNG 1024: $work/mish-1024.png)"
