#!/bin/zsh
# Monta Mish.app: app nativa propia (gui/) que ejecuta mish en su ventana.
#
# Uso:
#   app/build-app.sh                          compila e instala en ~/Applications/Mish.app (uso diario)
#   app/build-app.sh --dist [--arch A]        compila en release y deja en dist/:
#                                               Mish.app, Mish-<ver>-macos-<A>.zip y <zip>.sha256
#     A = arm64 | x86_64 | universal          (por defecto, la arquitectura de esta máquina)
#
# La versión del bundle sale de gui/Cargo.toml (única para todos los crates; ver app/release.sh).
set -euo pipefail
here=${0:A:h}
root=${here:h}
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

die() { print -u2 -- "ERROR: $*"; exit 1; }

# ---------- argumentos ----------
dist=0
arch=""
while (( $# )); do
  case $1 in
    --dist) dist=1 ;;
    --arch)
      (( $# >= 2 )) || die "--arch necesita un valor (arm64|x86_64|universal)"
      arch=$2; shift ;;
    --arch=*) arch=${1#--arch=} ;;
    -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) die "argumento desconocido: $1 (usa --help)" ;;
  esac
  shift
done
[[ -n $arch && $dist == 0 ]] && die "--arch solo tiene sentido con --dist"

host_arch=$(uname -m)            # arm64 | x86_64
[[ -z $arch ]] && arch=$host_arch
case $arch in
  arm64|x86_64|universal) ;;
  *) die "arquitectura no válida: $arch (arm64|x86_64|universal)" ;;
esac
triple_of() { case $1 in arm64) print aarch64-apple-darwin ;; x86_64) print x86_64-apple-darwin ;; esac }

# ---------- versión (de gui/Cargo.toml) ----------
cargo_version() {
  local manifest=$1 id v
  # cargo pkgid: "path+file:///…#mish-app@0.2.0" (cargo reciente) o "…#0.2.0" (antiguo).
  if id=$(cargo pkgid --offline --manifest-path "$manifest" 2>/dev/null) && [[ -n $id ]]; then
    v=${id##*#}; v=${v##*@}
  else
    # Respaldo sin cargo: primer `version = "…"` dentro de [package].
    v=$(awk '
      /^\[/ { inpkg = ($0 ~ /^\[package\][[:space:]]*$/) ; next }
      inpkg && /^[[:space:]]*version[[:space:]]*=/ {
        if (match($0, /"[^"]*"/)) { print substr($0, RSTART+1, RLENGTH-2); exit }
      }' "$manifest")
  fi
  print -- "$v"
}
version=$(cargo_version "$root/gui/Cargo.toml")
# SemVer: X.Y.Z[-pre][+build]; además impide inyectar nada raro en el Info.plist.
[[ $version =~ '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$' ]] \
  || die "versión de gui/Cargo.toml no válida: '$version'"
# CFBundleVersion debe ser numérico (X.Y.Z): se quita pre-release/metadatos.
bundle_version=${version%%[-+]*}
echo "→ versión $version"

echo "→ comprobando JS de la app…"
# Todos los .js propios de gui/assets/ (se excluyen los vendorizados de xterm: xterm.js y addon-*.js).
for f in "$root"/gui/assets/*.js; do
  case ${f:t} in xterm.js|addon-*.js) continue ;; esac
  node --check "$f" || die "sintaxis en $f"
  echo "   ok ${f:t}"
done
# Identificador de esta compilación: la GUI y el daemon lo comparan en el saludo
# (distinto → aviso de reiniciar sesiones).
export MISH_BUILD="$(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo nogit)-$(date +%s)"
echo "→ build $MISH_BUILD"
# Mismo mínimo que LSMinimumSystemVersion: el código C/ObjC de las dependencias (cc) se
# compilaría si no para la versión del SDK de esta máquina y no arrancaría en macOS anteriores.
export MACOSX_DEPLOYMENT_TARGET=${MACOSX_DEPLOYMENT_TARGET:-13.0}

# ---------- compilación ----------
# build_crate <dir> <triple|""> : cargo build --release del crate (con --target si se da).
build_crate() {
  local dir=$1 triple=$2
  local -a args=(build --release -q)
  (( dist )) && args+=(--locked)
  [[ -n $triple ]] && args+=(--target "$triple")
  (cd "$dir" && cargo "${args[@]}")
}
# out_dir <dir> <triple|""> : carpeta donde cargo deja los binarios.
out_dir() { [[ -n $2 ]] && print -- "$1/target/$2/release" || print -- "$1/target/release" }
# arch_dir <dir> <arm64|x86_64> : como out_dir, pero la arquitectura nativa se compila sin --target.
arch_dir() { [[ $2 == $host_arch ]] && out_dir "$1" "" || out_dir "$1" "$(triple_of $2)" }

typeset -A bin_src   # nombre del ejecutable → crate (directorio)
bin_src=(mish "$root" mish-daemon "$root/daemon" mish-app "$root/gui")

if (( dist )); then
  if [[ $arch == universal ]]; then
    archs=(arm64 x86_64)
  else
    archs=($arch)
  fi
  # Comprueba que la librería estándar de cada target está instalada (rustup o no).
  sysroot=$(rustc --print sysroot)
  for a in $archs; do
    t=$(triple_of $a)
    if [[ $a != $host_arch && ! -d $sysroot/lib/rustlib/$t ]]; then
      die "falta el target Rust $t (necesario para --arch $arch). Instálalo con: rustup target add $t"
    fi
  done
  for a in $archs; do
    t=$(triple_of $a)
    # Arquitectura nativa sin --target: reutiliza target/release de las compilaciones normales.
    [[ $a == $host_arch ]] && t=""
    echo "→ compilando para $a…"
    echo "   mish (shell)";            build_crate "$root" "$t"
    echo "   mish-daemon (sesiones)";  build_crate "$root/daemon" "$t"
    echo "   mish-app (ventana)";      build_crate "$root/gui" "$t"
  done
  app="$root/dist/Mish.app"
  mkdir -p "$root/dist"
else
  echo "→ compilando mish (shell)…"
  (cd "$root" && cargo install --path . -q)
  # Copia para el bundle: el daemon lanza Contents/MacOS/mish sin depender de cargo.
  build_crate "$root" ""
  echo "→ compilando mish-daemon (sesiones)…"
  build_crate "$root/daemon" ""
  echo "→ compilando mish-app (ventana)…"
  build_crate "$root/gui" ""
  app="$HOME/Applications/Mish.app"
fi

# ---------- bundle ----------
# rm + cp crea inodos nuevos: un mish-daemon antiguo que siga vivo conserva su binario
# (no se mata aquí; la app avisa de la compilación distinta al conectar).
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
for bin in mish-app mish-daemon mish; do
  dir=${bin_src[$bin]}
  dst="$app/Contents/MacOS/$bin"
  if (( dist )) && [[ $arch == universal ]]; then
    lipo -create -output "$dst" "$(arch_dir "$dir" arm64)/$bin" "$(arch_dir "$dir" x86_64)/$bin"
  elif (( dist )); then
    cp "$(arch_dir "$dir" "$arch")/$bin" "$dst"
  else
    cp "$(out_dir "$dir" "")/$bin" "$dst"
  fi
done
cp "$here/mish.icns" "$app/Contents/Resources/mish.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Mish</string>
  <key>CFBundleDisplayName</key><string>Mish</string>
  <key>CFBundleIdentifier</key><string>dev.sergiosbdev.mish</string>
  <key>CFBundleExecutable</key><string>mish-app</string>
  <key>CFBundleIconFile</key><string>mish</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$bundle_version</string>
  <key>LSMinimumSystemVersion</key><string>$MACOSX_DEPLOYMENT_TARGET</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
</dict>
</plist>
PLIST
plutil -lint -s "$app/Contents/Info.plist" || die "Info.plist inválido"

# ---------- firma ad-hoc ----------
# Ejecutables auxiliares primero y la app al final, que sella el bundle entero.
if (( dist )); then
  # Atributos extendidos (FinderInfo, cuarentena…) hacen fallar codesign y viajarían en el zip.
  xattr -cr "$app" 2>/dev/null || true
  for bin in mish-daemon mish; do
    codesign --force --sign - --timestamp=none "$app/Contents/MacOS/$bin" || die "codesign de $bin"
  done
  codesign --force --sign - --timestamp=none "$app" || die "codesign de Mish.app"
  codesign --verify --deep --strict "$app" || die "la firma de Mish.app no verifica"

  zip="Mish-$version-macos-$arch.zip"
  rm -f "$root/dist/$zip" "$root/dist/$zip.sha256" "$root/dist/$zip.sig"
  # ditto conserva permisos, symlinks y la firma; --keepParent → el zip contiene Mish.app/.
  ditto -c -k --keepParent "$app" "$root/dist/$zip"
  (cd "$root/dist" && shasum -a 256 "$zip" > "$zip.sha256")
  echo "OK: $app"
  echo "OK: $root/dist/$zip"
  echo "OK: $root/dist/$zip.sha256  ($(cut -d' ' -f1 < "$root/dist/$zip.sha256"))"
else
  for bin in mish-daemon mish; do
    codesign --force --sign - "$app/Contents/MacOS/$bin" >/dev/null 2>&1 || true
  done
  codesign --force --sign - "$app" >/dev/null 2>&1 || true
  touch "$app"
  echo "OK: $app"
fi
