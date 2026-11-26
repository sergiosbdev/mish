# mish-updater

Actualizador de Mish.app desde GitHub Releases. Crate independiente (sin
workspace); se usa como `mish-updater = { path = "../updater" }`.
La comprobación es solo manual (menú o `mish update`).

## API

```rust
pub struct UpdConfig { owner, repo, api_base, current: Version, bundle: PathBuf, pubkey: [u8; 32], cache_dir: PathBuf }
pub struct Release   { version, tag, notes, published, zip: Asset, sig: Asset, sha256: Asset }
pub struct Asset     { name, url, size: u64 }
pub struct Downloaded { pub release: Release, pub zip: PathBuf, /* firma, privada */ }

pub fn default_config(current_bundle: PathBuf) -> UpdConfig;
pub fn check(cfg: &UpdConfig) -> Result<Option<Release>, UpdError>;
pub fn download(cfg: &UpdConfig, rel: &Release, progress: &mut dyn FnMut(u64, Option<u64>)) -> Result<Downloaded, UpdError>;
pub fn install(cfg: &UpdConfig, dl: &Downloaded) -> Result<(), UpdError>;

// Extras
pub fn current_version() -> Version;           // CARGO_PKG_VERSION (versión única del proyecto)
pub fn current_bundle() -> Option<PathBuf>;     // .app que contiene current_exe()
pub fn bundle_of(exe: &Path) -> Option<PathBuf>;
pub fn default_cache_dir() -> PathBuf;          // ~/Library/Caches/mish/update
pub fn arch_candidates() -> &'static [&'static str];
pub const DEFAULT_REPO: &str;                   // MISH_UPDATE_REPO al compilar, o "sergiosbdev/mish"
pub const EMBEDDED_PUBKEY: [u8; 32];            // update-key.pub, decodificada al compilar
pub const MAX_ZIP: u64;                         // 200 MiB
pub mod format;                                 // formatos .sig / .sha256 / update-key.pub
```

Las funciones son bloqueantes: llámalas desde un hilo. Los mensajes de
`UpdError` (`Display`) están en español y se pueden mostrar tal cual.

- `check`: `GET {api_base}/repos/{owner}/{repo}/releases/latest`. Devuelve
  `None` si la versión no es más nueva, si es draft/prerelease o si el tag
  lleva sufijo semver (`-rc.1`). Elige `Mish-X.Y.Z-macos-<arch>.zip`, con la
  arquitectura nativa (`arm64` o `x86_64`) y, si no está, `universal`. Nunca
  elige la de otra arquitectura. Exige `<zip>.sig` y `<zip>.sha256`.
- `download`: baja el .sha256 y el .sig (máx. 4 KiB) y luego el zip a
  `cache_dir` (0700) como `.part`. Falla si supera `Asset.size` o
  `MAX_ZIP`, o si llega incompleto. Verifica el sha256 y la firma
  (`verify_strict`). Si algo falla, borra la descarga.
- `install`: vuelve a verificar la firma del zip y comprueba que se puede
  escribir en la carpeta del bundle. Después:
  1. `ditto -x -k` en `.<Mish.app>.update-<ts>` (oculto, en el mismo volumen).
  2. Comprueba que el zip trae exactamente `Mish.app`.
  3. Comprueba que `CFBundleShortVersionString` es la de la release.
  4. Pasa `codesign --verify --deep --strict` y quita `com.apple.quarantine`.
  5. Cambia las apps: `bundle → <bundle>.old-<ts>` y luego `nueva → bundle`.
     Si lo segundo falla, deshace lo primero.
  6. Borra `.old`.

  Nada se instala si la app está translocada (`/AppTranslocation/`) o si falta
  el permiso de escritura (`UpdError::SinPermiso(dir)`).

HTTP: ureq 3 con rustls/ring y raíces webpki, sin OpenSSL.
- User-Agent: `mish-updater/<versión>`.
- Timeouts: 15 s para conectar, 30 s para la respuesta y 60 s por lectura.
- Hasta 5 redirecciones.
- Solo HTTPS si `api_base` es https.

## Formatos (lo que publica CI)

Por cada zip `Mish-X.Y.Z-macos-<arch>.zip` la release lleva:

| Fichero | Contenido |
|---|---|
| `<zip>.sig` | Firma **Ed25519 pura** (RFC 8032, sin prehash) de los bytes del zip, en **base64 estándar** (88 caracteres + `\n`). También se aceptan 64 bytes en crudo. |
| `<zip>.sha256` | `<64 hex>  <nombre-del-zip>\n`, igual que `shasum -a 256 <nombre>` ejecutado en el directorio del zip. |

`update-key.pub` contiene la clave pública: 32 bytes en hex, y las líneas que
empiezan por `#` se ignoran. Se embebe con `include_str!` y se decodifica en
tiempo de compilación. Si es todo ceros, la app no tiene clave
(`UpdError::SinClave`).

## Claves

```sh
app/gen-update-key.sh
```

- La privada se genera en `~/.config/mish-export/update-key.pem` (PKCS#8 PEM,
  dir 0700 y fichero 0600). Si ya existe, se reutiliza.
- La pública se escribe en `updater/update-key.pub`.
- El script nunca imprime la privada.

Para que CI firme, carga la privada como secret (a mano):

```sh
gh secret set MISH_UPDATE_KEY --repo sergiosbdev/mish < ~/.config/mish-export/update-key.pem
```

La clave privada no debe estar nunca en el repo (`updater/.gitignore` ignora
`*.pem`).

## Firma en CI

Desde la raíz del repo, con la clave en un fichero (0600) o en una variable
de entorno:

```sh
cargo run --release --manifest-path updater/Cargo.toml --example sign -- --key "$key" "$zip"
# o bien, sin fichero:
cargo run --release --manifest-path updater/Cargo.toml --example sign -- --key-env MISH_UPDATE_KEY "$zip"
```

- Escribe `"$zip.sig"` y `"$zip.sha256"` junto al zip.
- Falla (exit 1) si la clave no corresponde a `update-key.pub` o si la firma
  no verifica. `--allow-other-key` quita esa comprobación, pero solo sirve
  para pruebas.
- Acepta varios zips.

Verificación independiente con el mismo código que usa la app:

```sh
cargo run --release --manifest-path updater/Cargo.toml --example verify -- "$zip"
```

La firma también se puede comprobar con OpenSSL 3:

```sh
openssl pkeyutl -verify -pubin -inkey pub.pem -rawin -in "$zip" -sigfile <(base64 -d < "$zip.sig")
```

## Tests

```sh
cargo test     # servidor HTTP local (std::net), sin red; usa ditto/codesign/plutil
cargo clippy --all-targets -- -D warnings
```
