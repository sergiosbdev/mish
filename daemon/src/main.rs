//! `mish-daemon`: mantiene vivas las sesiones de terminal de Mish.app aunque
//! la GUI se cierre. Habla `mish-proto` por un socket Unix en
//! `~/.local/state/mish/daemon.sock` (0600, directorio 0700).
//!
//! Uso: `mish-daemon [--foreground] [--state-dir DIR]`
//!
//! - Solo hay un daemon por directorio de estado (`flock` en `daemon.lock`):
//!   si ya hay otro, sale con 0 sin decir nada.
//! - Solo acepta conexiones del mismo UID (`getpeereid`).
//! - Con 0 sesiones y 0 clientes durante 10 s, sale y borra el socket.
//!
//! Variables de entorno: `MISH_STATE_DIR` (directorio de estado),
//! `MISH_APP_CMD` (tests: comando de las sesiones vía `/bin/sh -c`),
//! `MISH_DAEMON_IDLE_MS` (tests: inactividad antes de salir),
//! `MISH_DAEMON_NOTIFY` (`off` | `log`: notificaciones del sistema; ver `notify`).
//! `MISH_BUILD` se fija al compilar (por defecto "dev").

mod claude;
mod client;
mod hub;
mod log;
mod notify;
mod ring;
mod session;
mod vtscan;

use std::fs::OpenOptions;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

use mish_proto::{STATE_DIR_ENV, ensure_state_dir, lock_path, log_path, socket_path};

const IDLE: Duration = Duration::from_secs(10);
/// Eventos en cola hacia el hub.
const EVENT_QUEUE: usize = 1024;

fn usage() -> ! {
    eprintln!("uso: mish-daemon [--foreground] [--state-dir DIR]");
    std::process::exit(2);
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: buffer válido con su longitud.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

fn main() {
    let mut foreground = false;
    let mut state_dir: Option<PathBuf> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--foreground") => foreground = true,
            Some("--state-dir") => {
                state_dir = Some(args.next().map(PathBuf::from).unwrap_or_else(|| usage()))
            }
            Some("-h" | "--help") => usage(),
            _ => usage(),
        }
    }
    if let Some(d) = state_dir {
        let d = if d.is_absolute() {
            d
        } else {
            std::env::current_dir().unwrap_or_default().join(d)
        };
        // SAFETY: todavía no hay más hilos.
        unsafe { std::env::set_var(STATE_DIR_ENV, d) };
    }

    if let Err(e) = ensure_state_dir() {
        eprintln!("mish-daemon: directorio de estado no válido: {e}");
        std::process::exit(1);
    }

    // Un solo daemon por directorio de estado.
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(lock_path())
    {
        Ok(f) => f,
        Err(e) => {
            eprintln!("mish-daemon: no se pudo abrir el lock: {e}");
            std::process::exit(1);
        }
    };
    // SAFETY: fd válido mientras vive `lock` (hasta el final de main).
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        // Ya hay otro daemon: no es un error.
        std::process::exit(0);
    }

    log::init(&log_path(), foreground);
    let sock = socket_path();

    // Socket viejo de un daemon que murió: con el lock en la mano es seguro borrarlo.
    if let Ok(md) = std::fs::symlink_metadata(&sock) {
        if md.file_type().is_socket() || md.file_type().is_symlink() || md.is_file() {
            let _ = std::fs::remove_file(&sock);
        } else {
            log!("{} existe y no es un socket", sock.display());
            std::process::exit(1);
        }
    }

    // umask 077 solo durante el bind (los hijos heredan la umask original).
    // SAFETY: umask no tiene precondiciones; aún no hay más hilos.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(&sock);
    unsafe { libc::umask(old) };
    let listener = match listener {
        Ok(l) => l,
        Err(e) => {
            log!("no se pudo crear el socket {}: {e}", sock.display());
            std::process::exit(1);
        }
    };
    if let Err(e) = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)) {
        log!("chmod del socket: {e}");
        let _ = std::fs::remove_file(&sock);
        std::process::exit(1);
    }

    let idle = std::env::var("MISH_DAEMON_IDLE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(IDLE);
    let cfg = hub::Config {
        sock: sock.clone(),
        build: option_env!("MISH_BUILD").unwrap_or("dev").to_string(),
        hostname: hostname(),
        idle,
    };
    log!(
        "arrancado: proto={} build={} socket={}",
        mish_proto::PROTO,
        cfg.build,
        sock.display()
    );

    // Cola acotada: si el hub va por detrás (p. ej. `yes`), los lectores de PTY
    // se bloquean (contrapresión) en vez de acumular memoria y latencia.
    let (tx, rx) = mpsc::sync_channel(EVENT_QUEUE);
    let accept_tx = tx.clone();
    // SAFETY: getuid no tiene precondiciones.
    let uid = unsafe { libc::getuid() };
    std::thread::Builder::new()
        .name("accept".into())
        .spawn(move || {
            for conn in listener.incoming() {
                match conn {
                    Ok(s) => match client::peer_uid(&s) {
                        Ok(peer) if peer == uid => {
                            if accept_tx.send(hub::Event::Conn(s)).is_err() {
                                return;
                            }
                        }
                        Ok(peer) => log!("conexión rechazada: uid {peer} distinto de {uid}"),
                        Err(e) => log!("conexión rechazada: getpeereid: {e}"),
                    },
                    Err(e) => {
                        log!("accept: {e}");
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            }
        })
        .expect("hilo accept");

    let mut hub = hub::Hub::new(cfg, tx);
    hub.run(rx);

    let _ = std::fs::remove_file(&sock);
    // Margen para que los escritores vacíen sus colas (exit, taken_over…).
    std::thread::sleep(Duration::from_millis(100));
    drop(lock);
    std::process::exit(0);
}
