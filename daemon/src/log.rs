//! Log mínimo en `daemon.log` (0600). Nunca se registra contenido del
//! terminal ni lo que escribe el usuario: solo eventos (conexiones, sesiones,
//! errores).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Si el log supera este tamaño al arrancar, se rota a `daemon.log.old`.
const MAX_LOG: u64 = 1024 * 1024;

struct Logger {
    file: Option<Mutex<File>>,
    stderr: bool,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn init(path: &Path, stderr: bool) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_LOG) {
        let _ = std::fs::rename(path, path.with_extension("log.old"));
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .ok()
        .map(Mutex::new);
    let _ = LOGGER.set(Logger { file, stderr });
}

pub fn write(msg: std::fmt::Arguments<'_>) {
    let Some(l) = LOGGER.get() else { return };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{ts} [{}] {msg}\n", std::process::id());
    if let Some(f) = &l.file
        && let Ok(mut f) = f.lock()
    {
        let _ = f.write_all(line.as_bytes());
    }
    if l.stderr {
        let _ = std::io::stderr().write_all(line.as_bytes());
    }
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::write(format_args!($($arg)*)) };
}
