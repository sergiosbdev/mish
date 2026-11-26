//! Cliente del daemon de sesiones (`mish-daemon`) por socket Unix (protocolo `mish-proto`).
//!
//! - [`connect`] conecta a `proto::socket_path()`; si no hay daemon (ENOENT/ECONNREFUSED) lo
//!   lanza desde `<dir del ejecutable>/mish-daemon` en su propia sesión (`setsid`) y reintenta
//!   durante 2 s. Después hace el handshake `hello` → `welcome`.
//! - [`DaemonBackend`]: un hilo escritor (cola mpsc → `write_frame`) y un hilo lector que
//!   convierte los frames en [`UserEvent`] para el event loop.
//!
//! Política ante `taken_over` (otra GUI se conectó): el lector emite `UserEvent::TakenOver`
//! y deja de informar de la desconexión; la app pasa a **solo lectura** (no manda nada más,
//! no guarda `tabs.json` para no pisar el de la otra ventana) y muestra un aviso. Al salir
//! no mata nada.

use crate::UserEvent;
use crate::backend::{self, Backend, Mode};
use mish_proto::{self as proto, Ctl, Frame, Role};
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tao::event_loop::EventLoopProxy;

/// Identificador de esta compilación (build-app.sh exporta `MISH_BUILD`).
pub const BUILD: &str = match option_env!("MISH_BUILD") {
    Some(b) => b,
    None => "dev",
};

const START_TIMEOUT: Duration = Duration::from_secs(2);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const RETRY_EVERY: Duration = Duration::from_millis(40);
/// Trozos de teclado/pegado (muy por debajo de `MAX_DATA`).
const INPUT_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub proto: u32,
    pub build: String,
    pub pid: u32,
    pub hostname: String,
}

/// Compatibilidad del daemon con esta app.
#[derive(Debug, PartialEq, Eq)]
pub enum Compat {
    Same,
    /// Mismo protocolo, otra compilación: funciona, pero conviene reiniciarlo.
    BuildDiffers,
    /// Protocolo distinto: no se puede usar; modo local.
    ProtoDiffers,
}

pub fn compat(w: &Welcome, proto: u32, build: &str) -> Compat {
    if w.proto != proto {
        Compat::ProtoDiffers
    } else if w.build != build {
        Compat::BuildDiffers
    } else {
        Compat::Same
    }
}

#[derive(Debug)]
pub enum ConnectError {
    /// No hay daemon y no se pudo arrancar (o no contesta).
    Unavailable(String),
    /// Hay daemon pero de otro protocolo.
    ProtoMismatch(Welcome),
}

/// `MISH_DAEMON_BIN` (desarrollo) o `<dir de current_exe>/mish-daemon`.
fn daemon_bin() -> io::Result<PathBuf> {
    if let Some(p) = std::env::var_os("MISH_DAEMON_BIN").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    Ok(dir.join("mish-daemon"))
}

/// Lanza `mish-daemon` desacoplado: sesión nueva (`setsid`), stdin/stdout a /dev/null y
/// stderr al log (0600). Un hilo recoge al hijo para no dejar zombis.
fn spawn_daemon() -> io::Result<()> {
    let bin = daemon_bin()?;
    if !bin.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} no existe", bin.display())));
    }
    proto::ensure_state_dir()?;
    let log = std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(proto::log_path())?;
    let mut cmd = Command::new(&bin);
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(log).current_dir("/");
    // SAFETY: setsid es async-signal-safe; el cierre no reserva memoria ni toma locks.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn no_daemon(e: &io::Error) -> bool {
    matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
}

/// Conecta a `sock`; si no hay nadie escuchando llama a `spawn` (una vez) y reintenta hasta
/// `timeout`.
pub fn connect_or_spawn(sock: &Path, spawn: impl FnOnce() -> io::Result<()>, timeout: Duration) -> io::Result<UnixStream> {
    match UnixStream::connect(sock) {
        Ok(s) => return Ok(s),
        Err(e) if no_daemon(&e) => {}
        Err(e) => return Err(e),
    }
    spawn()?;
    let deadline = Instant::now() + timeout;
    loop {
        match UnixStream::connect(sock) {
            Ok(s) => return Ok(s),
            Err(e) if no_daemon(&e) && Instant::now() < deadline => std::thread::sleep(RETRY_EVERY),
            Err(e) => return Err(e),
        }
    }
}

/// `hello` → `welcome`. Ignora frames recuperables mal formados; cualquier otro mensaje
/// antes de `welcome` es un error.
pub fn handshake(stream: &mut UnixStream, role: Role, build: &str) -> io::Result<Welcome> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    proto::write_frame(stream, &Frame::Ctl(Ctl::Hello { proto: proto::PROTO, role, build: build.into() }))?;
    let w = loop {
        match proto::read_frame(stream) {
            Ok(Frame::Ctl(Ctl::Welcome { proto, build, pid, hostname })) => {
                break Welcome { proto, build, pid, hostname };
            }
            Ok(Frame::Ctl(Ctl::Error { msg, .. })) => {
                return Err(io::Error::other(format!("el daemon rechazó el saludo: {msg}")));
            }
            Ok(_) => return Err(io::Error::other("respuesta inesperada al saludo")),
            Err(e) if proto::is_recoverable(&e) => continue,
            Err(e) => return Err(e),
        }
    };
    stream.set_read_timeout(None)?;
    Ok(w)
}

/// Conecta (arrancando el daemon si hace falta) y saluda como GUI.
pub fn connect(proxy: EventLoopProxy<UserEvent>) -> Result<(DaemonBackend, Welcome), ConnectError> {
    let sock = proto::socket_path();
    let mut stream = connect_or_spawn(&sock, spawn_daemon, START_TIMEOUT)
        .map_err(|e| ConnectError::Unavailable(e.to_string()))?;
    let w = handshake(&mut stream, Role::Gui, BUILD).map_err(|e| ConnectError::Unavailable(e.to_string()))?;
    if compat(&w, proto::PROTO, BUILD) == Compat::ProtoDiffers {
        let _ = stream.shutdown(std::net::Shutdown::Both);
        return Err(ConnectError::ProtoMismatch(w));
    }
    let backend = DaemonBackend::start(stream, move |ev| proxy.send_event(ev).is_ok())
        .map_err(|e| ConnectError::Unavailable(e.to_string()))?;
    Ok((backend, w))
}

/// Apaga el daemon que haya (de cualquier versión: `hello`/`welcome`/`shutdown` son
/// estables) y espera hasta 3 s a que deje de aceptar conexiones. `Ok` si no había daemon.
pub fn shutdown_daemon() -> io::Result<()> {
    let sock = proto::socket_path();
    let mut s = match UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) if no_daemon(&e) => return Ok(()),
        Err(e) => return Err(e),
    };
    handshake(&mut s, Role::Ctl, BUILD)?;
    proto::write_frame(&mut s, &Frame::Ctl(Ctl::Shutdown {}))?;
    drop(s);
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match UnixStream::connect(&sock) {
            Err(e) if no_daemon(&e) => return Ok(()),
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    Err(io::Error::new(io::ErrorKind::TimedOut, "el daemon no se ha cerrado"))
}

/// Traduce un frame del daemon a evento del event loop. `None` = ignorar.
fn frame_event(f: Frame) -> Option<UserEvent> {
    Some(match f {
        Frame::Out { sid, data } => UserEvent::Output(sid, data),
        Frame::Replay { sid, data } => UserEvent::Replay(sid, data),
        Frame::In { .. } => return None,
        Frame::Ctl(c) => match c {
            Ctl::Sessions { list } => UserEvent::Sessions(list),
            Ctl::Exit { id } => UserEvent::Exit(id),
            Ctl::ReplayEnd { id } => UserEvent::ReplayEnd(id),
            Ctl::Meta { id, title, cwd, tint } => UserEvent::Meta { id, title, cwd, tint },
            Ctl::Error { id, msg } => UserEvent::DaemonError { id, msg },
            Ctl::TakenOver { .. } => UserEvent::TakenOver,
            Ctl::Claude { id, state, detail } => UserEvent::Claude { id, state, detail },
            _ => return None,
        },
    })
}

/// Bucle del hilo lector. `send` devuelve `false` si el event loop ya no existe.
/// Al terminar emite `DaemonLost` salvo que el cierre fuera nuestro (`closing`) o tras
/// `taken_over`.
fn reader_loop(mut r: UnixStream, closing: Arc<AtomicBool>, mut send: impl FnMut(UserEvent) -> bool) {
    loop {
        match proto::read_frame(&mut r) {
            Ok(f) => {
                let Some(ev) = frame_event(f) else { continue };
                let taken = matches!(ev, UserEvent::TakenOver);
                if taken {
                    closing.store(true, Ordering::SeqCst);
                }
                if !send(ev) || taken {
                    return;
                }
            }
            Err(e) if proto::is_recoverable(&e) => continue,
            Err(_) => break,
        }
    }
    if !closing.load(Ordering::SeqCst) {
        send(UserEvent::DaemonLost);
    }
}

pub struct DaemonBackend {
    tx: Option<mpsc::Sender<Frame>>,
    writer: Option<JoinHandle<()>>,
    stream: UnixStream,
    closing: Arc<AtomicBool>,
}

impl DaemonBackend {
    /// Arranca los hilos lector y escritor sobre una conexión ya saludada.
    pub fn start(stream: UnixStream, send: impl FnMut(UserEvent) -> bool + Send + 'static) -> io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let closing = Arc::new(AtomicBool::new(false));
        let r = stream.try_clone()?;
        let mut w = stream.try_clone()?;
        let c = closing.clone();
        std::thread::spawn(move || reader_loop(r, c, send));
        let (tx, rx) = mpsc::channel::<Frame>();
        let writer = std::thread::spawn(move || {
            for f in rx {
                if proto::write_frame(&mut w, &f).is_err() {
                    break;
                }
            }
        });
        Ok(DaemonBackend { tx: Some(tx), writer: Some(writer), stream, closing })
    }

    fn send(&self, f: Frame) -> bool {
        self.tx.as_ref().is_some_and(|tx| tx.send(f).is_ok())
    }

    fn ctl(&self, c: Ctl) -> bool {
        self.send(Frame::Ctl(c))
    }

    /// Vacía la cola de escritura y cierra la conexión (las sesiones siguen en el daemon).
    fn disconnect(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        drop(self.tx.take());
        if let Some(h) = self.writer.take() {
            // Acotado por WRITE_TIMEOUT del socket.
            let _ = h.join();
        }
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl Backend for DaemonBackend {
    fn mode(&self) -> Mode {
        Mode::Daemon
    }

    fn new_session(&mut self, id: u32, cols: u16, rows: u16, cwd: Option<&str>, argv: Option<&[String]>)
    -> Result<(), String> {
        let argv = backend::validate_argv(argv)?.map(<[String]>::to_vec);
        let cwd = backend::valid_cwd(cwd).map(str::to_string);
        if self.ctl(Ctl::New { id, cols, rows, cwd, argv }) {
            Ok(())
        } else {
            Err("conexión con mish-daemon cerrada".into())
        }
    }

    fn attach(&mut self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        if self.ctl(Ctl::Attach { id, cols, rows }) { Ok(()) } else { Err("conexión con mish-daemon cerrada".into()) }
    }

    fn input(&mut self, id: u32, data: &[u8]) -> bool {
        data.chunks(INPUT_CHUNK).all(|c| self.send(Frame::In { sid: id, data: c.to_vec() }))
    }

    fn resize(&mut self, id: u32, cols: u16, rows: u16) {
        self.ctl(Ctl::Resize { id, cols, rows });
    }

    fn close(&mut self, id: u32) {
        self.ctl(Ctl::Close { id });
    }

    fn kill_all(&mut self) {
        self.ctl(Ctl::KillAll {});
    }

    fn list(&mut self) {
        self.ctl(Ctl::List {});
    }

    fn shutdown(&mut self, kill: bool) {
        if kill {
            self.kill_all();
        }
        self.disconnect();
    }
}

impl Drop for DaemonBackend {
    fn drop(&mut self) {
        self.disconnect();
    }
}
