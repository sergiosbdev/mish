#!/bin/zsh
# Prepara una release de Mish: sube la versión de todos los crates, pasa los tests,
# hace commit "Release vX.Y.Z" y crea el tag anotado vX.Y.Z. NO hace push.
#
# Uso:
#   app/release.sh X.Y.Z [--notes FICHERO] [--no-test] [--no-tag]
#
#   --notes FICHERO  texto del tag anotado (= notas de la release en GitHub: el workflow usa
#                    --notes-from-tag y el actualizador las enseña). Por defecto: lista de
#                    commits desde el tag anterior.
#   --no-test        no ejecuta `cargo test` (solo si ya los has pasado).
#   --no-tag         solo sube versiones y hace commit, sin tag (ver flujo abajo).
#
# Flujo previsto con la rama pública (fase P):
#   La rama `master` pública es huérfana y la genera app/export-public.sh desde nebula-features.
#   El tag que dispara .github/workflows/release.yml debe estar en `master`, así que:
#     1. En nebula-features:  app/release.sh X.Y.Z --no-tag   (las versiones quedan en la rama privada
#                              y la siguiente exportación no las retrocede).
#     2. app/export-public.sh  (genera el commit en master).
#     3. En master:            app/release.sh X.Y.Z           (si la versión ya está puesta solo
#                              crea el tag; si no, también hace el commit).
#   Este script no depende de nada que export-public.sh quite (tests, .claude/, informes/…):
#   funciona igual en master exportada que en la rama privada.
set -euo pipefail
here=${0:A:h}
root=${here:h}
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

die() { print -u2 -- "ERROR: $*"; exit 1; }

# ---------- argumentos ----------
version="" notes_file="" run_tests=1 make_tag=1
while (( $# )); do
  case $1 in
    --notes) (( $# >= 2 )) || die "--notes necesita un fichero"; notes_file=$2; shift ;;
    --no-test) run_tests=0 ;;
    --no-tag) make_tag=0 ;;
    -h|--help) sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) die "opción desconocida: $1" ;;
    *) [[ -z $version ]] || die "sobra el argumento: $1"; version=${1#v} ;;
  esac
  shift
done
[[ -n $version ]] || die "falta la versión. Uso: app/release.sh X.Y.Z"
# SemVer sin metadatos de build (+…): X.Y.Z o X.Y.Z-pre. Sin ceros a la izquierda.
semver='^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
[[ $version =~ $semver ]] || die "versión no válida (SemVer X.Y.Z o X.Y.Z-pre): $version"
tag="v$version"
[[ -z $notes_file || -r $notes_file ]] || die "no puedo leer $notes_file"
[[ -n $notes_file ]] && notes_file=${notes_file:A}

cd "$root"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "$root no es un repositorio git"
branch=$(git symbolic-ref --quiet --short HEAD) || die "HEAD separado: cámbiate a una rama"
[[ -z $(git status --porcelain --untracked-files=normal) ]] \
  || die "el árbol no está limpio (git status): haz commit o stash antes"
git rev-parse -q --verify "refs/tags/$tag" >/dev/null && die "el tag $tag ya existe"

# ---------- crates ----------
# Todos los Cargo.toml versionados con [package] (raíz, gui, daemon, proto, updater…).
manifests=()
# Por ruta (no solo git ls-files): incluye crates nuevos aún sin commit; target/ no tiene Cargo.toml.
for m in Cargo.toml */Cargo.toml(N); do
  [[ -f $m ]] && grep -q '^\[package\]' "$m" && manifests+=("$m")
done
(( ${#manifests} )) || die "no encuentro ningún Cargo.toml"

# pkg_field <manifest> <campo> : valor literal de name/version dentro de [package].
pkg_field() {
  awk -v key="$2" '
    /^\[/ { inpkg = ($0 ~ /^\[package\][[:space:]]*$/); next }
    inpkg && $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
      if (match($0, /"[^"]*"/)) { print substr($0, RSTART+1, RLENGTH-2); exit }
    }' "$1"
}

current=$(pkg_field gui/Cargo.toml version)
print "→ rama $branch · versión actual $current → $version"
for m in $manifests; do
  v=$(pkg_field "$m" version)
  [[ -n $v ]] || die "$m no tiene version = \"…\" literal en [package] (¿version.workspace?)"
  [[ $v == $current ]] || print "   aviso: $m está en $v (se iguala a $version)"
done

# La nueva versión no puede ser menor que la actual (se compara X.Y.Z numéricamente).
core_new=${version%%-*} core_cur=${current%%-*}
if [[ $core_new != $core_cur ]]; then
  lowest=$(printf '%s\n%s\n' "$core_new" "$core_cur" | sort -t. -k1,1n -k2,2n -k3,3n | head -1)
  [[ $lowest == $core_cur ]] || die "$version es menor que la versión actual $current"
fi

# ---------- subir versiones ----------
changed=()
restore() {
  # Si algo falla antes del commit, deja el árbol como estaba.
  (( ${#changed} )) && git checkout -q -- "${changed[@]}" 2>/dev/null || true
}
trap 'restore' EXIT

for m in $manifests; do
  [[ $(pkg_field "$m" version) == $version ]] && continue
  tmp=$(mktemp "${m}.XXXXXX")
  awk -v new="$version" '
    /^\[/ { inpkg = ($0 ~ /^\[package\][[:space:]]*$/) }
    inpkg && !done && /^[[:space:]]*version[[:space:]]*=[[:space:]]*"/ {
      sub(/"[^"]*"/, "\"" new "\""); done = 1
    }
    { print }' "$m" > "$tmp"
  mv "$tmp" "$m"
  changed+=("$m")
  [[ $(pkg_field "$m" version) == $version ]] || die "no he podido cambiar la versión en $m"
  print "   $m → $version"
done

# Cargo.lock: cada crate con lock propio refresca su entrada (y las de sus dependencias por
# ruta, p. ej. mish-proto en gui/ y daemon/). --offline: no toca el resto de dependencias.
for m in $manifests; do
  dir=${m:h}
  [[ -f $dir/Cargo.lock ]] || continue
  name=$(pkg_field "$m" name)
  (cd "$dir" && { cargo update -p "$name" --offline -q 2>/dev/null || cargo update -p "$name" -q; })
  git ls-files --error-unmatch "$dir/Cargo.lock" >/dev/null 2>&1 && changed+=("$dir/Cargo.lock")
done

# Solo deben haber cambiado Cargo.toml y Cargo.lock.
unexpected=$(git status --porcelain | awk '{print $NF}' | grep -Ev '(^|/)Cargo\.(toml|lock)$' || true)
[[ -z $unexpected ]] || die "han cambiado ficheros inesperados:\n$unexpected"

# ---------- tests rápidos ----------
if (( run_tests )); then
  for m in $manifests; do
    dir=${m:h}
    print "→ cargo test en ${dir/#./raíz}…"
    had_lock=0; [[ -f $dir/Cargo.lock ]] && had_lock=1
    if (( had_lock )); then
      (cd "$dir" && cargo test --locked -q) || die "fallan los tests en $dir (versiones restauradas)"
    else
      (cd "$dir" && cargo test -q) || die "fallan los tests en $dir (versiones restauradas)"
      # Crate sin lock propio (librería): no dejar un Cargo.lock nuevo sin seguimiento.
      rm -f "$dir/Cargo.lock"
    fi
  done
fi

# ---------- commit + tag ----------
email=$(git config user.email || true)
[[ $email == *@users.noreply.github.com ]] \
  || print -u2 "aviso: user.email = '$email' (en la rama pública usa la dirección noreply de GitHub)"

if [[ -n $(git status --porcelain) ]]; then
  git add -- "${changed[@]}"
  git commit -q -m "Release $tag"
  print "→ commit $(git rev-parse --short HEAD) «Release $tag»"
else
  print "→ las versiones ya estaban en $version: sin commit nuevo"
fi
changed=()   # ya está en el commit: nada que restaurar
trap - EXIT

if (( make_tag )); then
  msgfile=$(mktemp)
  trap 'rm -f "$msgfile"' EXIT
  if [[ -n $notes_file ]]; then
    { print "Mish $tag"; print; cat "$notes_file"; } > "$msgfile"
  else
    prev=$(git describe --tags --abbrev=0 --match 'v[0-9]*' HEAD^ 2>/dev/null || true)
    {
      print "Mish $tag"; print
      if [[ -n $prev ]]; then
        git log --no-merges --format='- %s' "$prev..HEAD" | grep -v '^- Release v' || true
      else
        print "- First public release"
      fi
    } > "$msgfile"
    # En master exportada los commits suelen ser solo «Release vX.Y.Z»: sin lista útil.
    (( $(wc -l < "$msgfile") > 2 )) \
      || print -u2 "aviso: el tag no lleva lista de cambios; usa --notes FICHERO para las notas de la release"
  fi
  git tag -a "$tag" -F "$msgfile"
  print "→ tag anotado $tag:"
  sed 's/^/     /' "$msgfile"
fi

remote=$(git config "branch.$branch.remote" || true)
[[ -n $remote ]] || remote=origin
print
print "Listo. Revisa con:  git show --stat HEAD"
if (( make_tag )); then
  print "Para publicar (lanza el workflow de release):"
  print "  git push --atomic $remote $branch refs/tags/$tag"
else
  print "Sin tag (--no-tag). Para subir la rama:  git push $remote $branch"
fi
