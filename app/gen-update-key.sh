#!/usr/bin/env bash
# Genera el par Ed25519 con el que se firman las actualizaciones de Mish.
#
#   app/gen-update-key.sh
#
# - Privada: ~/.config/mish-export/update-key.pem (dir 0700, fichero 0600),
#   PKCS#8 PEM. Si ya existe, NO se regenera: solo se recalcula la pública.
#   Se puede cambiar el directorio con MISH_UPDATE_KEY_DIR.
# - Pública: updater/update-key.pub (32 bytes en hex), va en el repo y se
#   embebe en la app al compilar.
# - Este script nunca imprime la clave privada ni la sube a ningún sitio.
set -euo pipefail
umask 077

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
KEY_DIR="${MISH_UPDATE_KEY_DIR:-$HOME/.config/mish-export}"
KEY="$KEY_DIR/update-key.pem"
PUB="$ROOT/updater/update-key.pub"

mkdir -p "$KEY_DIR"
KEY_DIR="$(cd "$KEY_DIR" && pwd -P)"
KEY="$KEY_DIR/update-key.pem"
case "$KEY_DIR/" in
  "$(cd "$ROOT" && pwd -P)"/*)
    echo "error: la clave privada no puede guardarse dentro del repo ($KEY_DIR)" >&2
    exit 1 ;;
esac
chmod 700 "$KEY_DIR"

# El crate necesita update-key.pub para compilar: si falta, un placeholder de
# ceros (= «sin clave») que keygen sobrescribe enseguida.
if [ ! -f "$PUB" ]; then
  { echo "# placeholder: ejecuta app/gen-update-key.sh"; printf '%064d\n' 0; } > "$PUB"
fi

existed=0
[ -f "$KEY" ] && existed=1

cargo run --quiet --manifest-path "$ROOT/updater/Cargo.toml" --example keygen -- "$KEY" "$PUB" >/dev/null
chmod 600 "$KEY"
chmod 644 "$PUB"

echo
if [ "$existed" = 1 ]; then
  echo "Clave privada existente reutilizada: $KEY"
else
  echo "Clave privada nueva: $KEY"
  echo "  Haz una copia de seguridad (gestor de contraseñas). Si se pierde, las"
  echo "  apps ya instaladas no aceptarán actualizaciones firmadas con otra clave."
fi
echo "Clave pública: $PUB (súbela al repo)"
echo
echo "Para que el workflow de GitHub firme las releases, carga la privada como"
echo "secret (no lo ejecuta este script):"
echo
echo "  gh secret set MISH_UPDATE_KEY --repo ${MISH_UPDATE_REPO:-sergiosbdev/mish} < \"$KEY\""
echo
echo "Comprobación: git status no debe mostrar ningún .pem."
