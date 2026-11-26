//! Rutas del estado del daemon: `~/.local/state/mish/{daemon.sock,daemon.lock,daemon.log}`.
//!
//! El directorio se sobrescribe con la variable de entorno `MISH_STATE_DIR`
//! (tests). Ojo en macOS: `sun_path` admite ~104 bytes, así que en tests usa
//! directorios temporales cortos.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};

/// Variable de entorno que sustituye a [`state_dir`] (si no está vacía).
pub const STATE_DIR_ENV: &str = "MISH_STATE_DIR";

/// Directorio de estado: `$MISH_STATE_DIR` o `~/.local/state/mish`.
///
/// El home sale de `$HOME` o, si falta, de `getpwuid`. No crea nada: usa
/// [`ensure_state_dir`] antes de crear el socket.
pub fn state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os(STATE_DIR_ENV).filter(|v| !v.is_empty()) {
        return PathBuf::from(d);
    }
    home_dir().join(".local/state/mish")
}

/// `state_dir()/daemon.sock` (el daemon lo crea con modo 0600).
pub fn socket_path() -> PathBuf {
    state_dir().join("daemon.sock")
}

/// `state_dir()/daemon.lock` (el daemon lo bloquea con `flock`).
pub fn lock_path() -> PathBuf {
    state_dir().join("daemon.lock")
}

/// `state_dir()/daemon.log`.
pub fn log_path() -> PathBuf {
    state_dir().join("daemon.log")
}

/// Crea (si falta) y verifica [`state_dir`]. Devuelve su ruta.
/// Ver [`ensure_private_dir`].
pub fn ensure_state_dir() -> io::Result<PathBuf> {
    let dir = state_dir();
    ensure_private_dir(&dir)?;
    Ok(dir)
}

/// Crea `dir` con modo 0700 si no existe (los padres se crean con el modo por
/// defecto) y verifica, con `symlink_metadata`, que:
/// - no es un symlink y es un directorio;
/// - su dueño es `getuid()`;
/// - `mode & 0o077 == 0` (nadie más tiene permisos).
///
/// No corrige permisos: si algo no cuadra devuelve `PermissionDenied`.
pub fn ensure_private_dir(dir: &Path) -> io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let md = std::fs::symlink_metadata(dir)?;
    let deny = |why: String| {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{}: {why}", dir.display()),
        ))
    };
    if md.file_type().is_symlink() {
        return deny("es un symlink".into());
    }
    if !md.is_dir() {
        return deny("no es un directorio".into());
    }
    // SAFETY: getuid no tiene precondiciones y no puede fallar.
    let uid = unsafe { libc::getuid() };
    if md.uid() != uid {
        return deny(format!("dueño uid {} distinto de {uid}", md.uid()));
    }
    let mode = md.mode() & 0o7777;
    if mode & 0o077 != 0 {
        return deny(format!("modo {mode:o} demasiado abierto (se exige 0700)"));
    }
    Ok(())
}

fn home_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(h);
    }
    pw_home().unwrap_or_else(|| {
        // Último recurso; ensure_private_dir verificará dueño y modo igualmente.
        // SAFETY: getuid no tiene precondiciones.
        PathBuf::from(format!("/tmp/mish-{}", unsafe { libc::getuid() }))
    })
}

fn pw_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: passwd es POD; se inicializa con ceros y getpwuid_r lo rellena.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut res: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: punteros válidos a pw, buf (con su longitud real) y res.
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pw,
            buf.as_mut_ptr(),
            buf.len(),
            &mut res,
        )
    };
    if rc != 0 || res.is_null() || pw.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir apunta a una cadena C terminada en NUL dentro de buf.
    let dir = unsafe { std::ffi::CStr::from_ptr(pw.pw_dir) };
    Some(PathBuf::from(OsString::from_vec(dir.to_bytes().to_vec())))
}
