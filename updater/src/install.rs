//! Instalación: `ditto -x -k` en un directorio oculto hermano del bundle,
//! validación (`Info.plist`, `codesign`) y cambio atómico con rollback.
//! Todos los procesos se lanzan con rutas absolutas y `Command::arg` (sin shell).

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Downloaded, UpdConfig, UpdError, ensure_key, verify_file};

const DITTO: &str = "/usr/bin/ditto";
const PLUTIL: &str = "/usr/bin/plutil";
const CODESIGN: &str = "/usr/bin/codesign";
const XATTR: &str = "/usr/bin/xattr";
/// Nombre que debe tener el único elemento del zip.
const APP_NAME: &str = "Mish.app";

/// Instala el zip verificado en `cfg.bundle`. Si algo falla antes del cambio,
/// la app instalada no se toca; si falla el cambio, se restaura la anterior.
pub fn install(cfg: &UpdConfig, dl: &Downloaded) -> Result<(), UpdError> {
    install_with(cfg, dl, &mut |a, b| fs::rename(a, b))
}

type RenameFn<'a> = dyn FnMut(&Path, &Path) -> io::Result<()> + 'a;

pub(crate) fn install_with(
    cfg: &UpdConfig,
    dl: &Downloaded,
    rename: &mut RenameFn<'_>,
) -> Result<(), UpdError> {
    ensure_key(cfg)?;
    // Defensa en profundidad: el zip de la caché se vuelve a verificar.
    verify_file(&dl.zip, &dl.sig, &cfg.pubkey)?;

    let bundle = fs::canonicalize(&cfg.bundle).map_err(|_| UpdError::SinBundle)?;
    if bundle.to_string_lossy().contains("/AppTranslocation/") {
        return Err(UpdError::Translocada);
    }
    if !bundle.join("Contents/Info.plist").is_file() {
        return Err(UpdError::SinBundle);
    }
    let parent = bundle.parent().ok_or(UpdError::SinBundle)?.to_path_buf();
    let name = bundle
        .file_name()
        .ok_or(UpdError::SinBundle)?
        .to_string_lossy()
        .into_owned();
    check_writable(&parent)?;

    let stamp = stamp();
    let staging = parent.join(format!(".{name}.update-{stamp}"));
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&staging)
        .map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                UpdError::SinPermiso(parent.clone())
            }
            _ => UpdError::io("crear el directorio temporal", e),
        })?;
    let _cleanup = RmDirOnDrop(staging.clone());

    if !run(Command::new(DITTO)
        .arg("-x")
        .arg("-k")
        .arg(&dl.zip)
        .arg(&staging))
    {
        return Err(UpdError::Paquete("no se pudo descomprimir".into()));
    }
    let new_app = single_app(&staging)?;
    let got = plist_version(&new_app)?;
    let want = dl.release.version.to_string();
    if got != want {
        return Err(UpdError::Paquete(format!(
            "la app contiene la versión {got} y se esperaba {want}"
        )));
    }
    if !run(Command::new(CODESIGN)
        .arg("--verify")
        .arg("--deep")
        .arg("--strict")
        .arg(&new_app))
    {
        return Err(UpdError::Codesign);
    }
    // ureq no pone quarantine, pero si alguien lo hubiera añadido, fuera.
    let _ = run(Command::new(XATTR)
        .arg("-dr")
        .arg("com.apple.quarantine")
        .arg(&new_app));

    let old = parent.join(format!("{name}.old-{stamp}"));
    swap(&bundle, &new_app, &old, rename)?;
    // Mejor esfuerzo: si la vieja no se puede borrar (p. ej. ficheros de root),
    // la instalación ya está hecha.
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// bundle → old, new → bundle; si lo segundo falla, old → bundle.
pub(crate) fn swap(
    bundle: &Path,
    new_app: &Path,
    old: &Path,
    rename: &mut RenameFn<'_>,
) -> Result<(), UpdError> {
    rename(bundle, old).map_err(|e| match e.kind() {
        io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
            UpdError::SinPermiso(bundle.parent().unwrap_or(bundle).to_path_buf())
        }
        k => UpdError::Instalacion(format!("no se pudo apartar la app actual: {k}")),
    })?;
    if let Err(e) = rename(new_app, bundle) {
        return match rename(old, bundle) {
            Ok(()) => Err(UpdError::Instalacion(format!(
                "no se pudo colocar la app nueva: {}",
                e.kind()
            ))),
            Err(_) => Err(UpdError::RollbackFallido {
                old: old.to_path_buf(),
            }),
        };
    }
    Ok(())
}

struct RmDirOnDrop(PathBuf);
impl Drop for RmDirOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn stamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{secs}-{}", std::process::id())
}

fn check_writable(dir: &Path) -> Result<(), UpdError> {
    let c = CString::new(dir.as_os_str().as_bytes()).map_err(|_| UpdError::SinBundle)?;
    // SAFETY: `c` es una cadena C válida durante la llamada.
    let ok = unsafe { libc::access(c.as_ptr(), libc::W_OK | libc::X_OK) } == 0;
    if ok {
        Ok(())
    } else {
        Err(UpdError::SinPermiso(dir.to_path_buf()))
    }
}

fn run(cmd: &mut Command) -> bool {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// El directorio debe contener exactamente `Mish.app` (directorio real, no symlink).
fn single_app(staging: &Path) -> Result<PathBuf, UpdError> {
    let entries: Vec<_> = fs::read_dir(staging)
        .map_err(|e| UpdError::io("leer el paquete", e))?
        .filter_map(|e| e.ok())
        .collect();
    if entries.len() != 1 || entries[0].file_name() != APP_NAME {
        return Err(UpdError::Paquete(format!("debe contener solo {APP_NAME}")));
    }
    let app = entries[0].path();
    let meta = fs::symlink_metadata(&app).map_err(|e| UpdError::io("leer el paquete", e))?;
    if !meta.is_dir() {
        return Err(UpdError::Paquete(format!("{APP_NAME} no es una app")));
    }
    Ok(app)
}

pub(crate) fn plist_version(app: &Path) -> Result<String, UpdError> {
    let plist = app.join("Contents/Info.plist");
    let out = Command::new(PLUTIL)
        .arg("-extract")
        .arg("CFBundleShortVersionString")
        .arg("raw")
        .arg("-o")
        .arg("-")
        .arg(&plist)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| UpdError::io("ejecutar plutil", e))?;
    if !out.status.success() {
        return Err(UpdError::Paquete("Info.plist sin versión".into()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
