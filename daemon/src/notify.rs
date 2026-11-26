//! Notificaciones de macOS para cuando Claude espera o termina y no hay GUI
//! conectada (con GUI, avisa la propia app).
//!
//! Se lanza `/usr/bin/osascript` con el título y el mensaje como **argv**
//! (`on run argv`): nunca se interpolan en el código AppleScript, así que no
//! hay inyección posible. Como mucho una notificación por sesión cada
//! [`MIN_GAP`].
//!
//! `MISH_DAEMON_NOTIFY=off` las desactiva; `=log` solo las registra en el log
//! (lo usan los tests para no mostrar notificaciones reales).

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::log;

/// Separación mínima entre notificaciones de una misma sesión.
pub const MIN_GAP: Duration = Duration::from_secs(10);
const OSASCRIPT: &str = "/usr/bin/osascript";
const MAX_TITLE: usize = 80;
const MAX_MSG: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Osascript,
    Log,
    Off,
}

pub struct Notifier {
    mode: Mode,
    last: HashMap<u32, Instant>,
}

impl Notifier {
    pub fn new(mode: Mode) -> Notifier {
        Notifier {
            mode,
            last: HashMap::new(),
        }
    }

    pub fn from_env() -> Notifier {
        let mode = match std::env::var("MISH_DAEMON_NOTIFY").as_deref() {
            Ok("off") => Mode::Off,
            Ok("log") => Mode::Log,
            _ => Mode::Osascript,
        };
        Notifier::new(mode)
    }

    /// ¿Toca notificar la sesión `sid` en `now`? Si sí, apunta la hora.
    fn allow(&mut self, sid: u32, now: Instant) -> bool {
        match self.last.get(&sid) {
            Some(&t) if now.saturating_duration_since(t) < MIN_GAP => false,
            _ => {
                self.last.insert(sid, now);
                true
            }
        }
    }

    /// Notifica (sin bloquear) salvo que lo impida el límite. Devuelve si se
    /// ha lanzado.
    pub fn notify(&mut self, sid: u32, what: &str, title: &str, msg: &str) -> bool {
        if self.mode == Mode::Off || !self.allow(sid, Instant::now()) {
            return false;
        }
        // Solo metadatos en el log: nunca el mensaje.
        log!("sesión {sid}: notificación ({what})");
        if self.mode == Mode::Log {
            return true;
        }
        let mut cmd = command(title, msg);
        match cmd.spawn() {
            Ok(mut child) => {
                // Recoge el proceso para no dejar zombis.
                let _ = std::thread::Builder::new()
                    .name("notify".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
                true
            }
            Err(e) => {
                log!("osascript: {e}");
                false
            }
        }
    }

    /// La sesión ya no existe.
    pub fn forget(&mut self, sid: u32) {
        self.last.remove(&sid);
    }
}

/// Quita caracteres de control y acota (en caracteres).
fn clean(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    mish_proto::clamp_chars(s.trim(), max)
}

/// `osascript` con el título y el mensaje como argumentos (no interpolados).
pub fn command(title: &str, msg: &str) -> Command {
    let mut cmd = Command::new(OSASCRIPT);
    cmd.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv)",
        "-e",
        "end run",
    ])
    .arg(clean(title, MAX_TITLE))
    .arg(clean(msg, MAX_MSG))
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    cmd
}
