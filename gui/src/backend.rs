//! Dónde viven las sesiones (PTYs): en este proceso ([`LocalBackend`]) o en `mish-daemon`
//! ([`crate::daemon_client::DaemonBackend`]), que las mantiene vivas al cerrar la ventana.
//!
//! Todas las respuestas asíncronas (salida, fin de sesión, lista de sesiones) llegan al
//! event loop como [`UserEvent`], igual en los dos modos.

use crate::UserEvent;
use mish_proto::SessionInfo;
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tao::event_loop::EventLoopProxy;

/// Máximo de argumentos y de bytes totales aceptados en `argv` de `new`.
const ARGV_MAX_ITEMS: usize = 256;
const ARGV_MAX_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Daemon,
    Local,
    /// Otra GUI tomó el control del daemon: esta ventana ya no manda nada.
    ReadOnly,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Daemon => "daemon",
            Mode::Local => "local",
            Mode::ReadOnly => "readonly",
        }
    }
}

pub trait Backend {
    fn mode(&self) -> Mode;
    /// Crea la sesión `id` (elegido por app.js) y la engancha. `cwd` ya validado o `None`.
    fn new_session(&mut self, id: u32, cols: u16, rows: u16, cwd: Option<&str>, argv: Option<&[String]>)
    -> Result<(), String>;
    /// Se engancha a una sesión viva (daemon: REPLAY… + `replay_end`).
    fn attach(&mut self, id: u32, cols: u16, rows: u16) -> Result<(), String>;
    /// Bytes de teclado (o de la caja fuerte). `false` si la sesión no existe o falla.
    fn input(&mut self, id: u32, data: &[u8]) -> bool;
    fn resize(&mut self, id: u32, cols: u16, rows: u16);
    /// Mata la sesión (llegará `Exit`).
    fn close(&mut self, id: u32);
    /// La sesión terminó (tras `UserEvent::Exit`): liberar recursos.
    fn exited(&mut self, _id: u32) {}
    fn kill_all(&mut self);
    /// Pide la lista de sesiones vivas; la respuesta llega como `UserEvent::Sessions`.
    fn list(&mut self);
    /// Fin: el daemon se desconecta (las sesiones siguen vivas salvo `kill`); en local se
    /// matan siempre. Idempotente.
    fn shutdown(&mut self, kill: bool);
}

/// `MISH_NO_DAEMON` activo (cualquier valor no vacío salvo `0`/`false`) → modo local.
pub fn daemon_disabled(var: Option<&OsStr>) -> bool {
    match var.and_then(OsStr::to_str) {
        None => var.is_some_and(|v| !v.is_empty()),
        Some(v) => {
            let v = v.trim();
            !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false")
        }
    }
}

/// Directorio inicial de una sesión: el cwd pedido solo si es absoluto y es un directorio;
/// en otro caso, $HOME.
pub fn start_dir(cwd: Option<&str>, home: &str) -> PathBuf {
    match cwd.map(Path::new) {
        Some(p) if p.is_absolute() && p.is_dir() => p.to_path_buf(),
        _ => PathBuf::from(home),
    }
}

/// cwd que se reenvía al daemon: solo si es absoluto y existe como directorio.
pub fn valid_cwd(cwd: Option<&str>) -> Option<&str> {
    cwd.filter(|c| Path::new(c).is_absolute() && Path::new(c).is_dir())
}

/// `argv` de `new`: no vacío, sin NUL y acotado. `None` = la shell (mish).
pub fn validate_argv(argv: Option<&[String]>) -> Result<Option<&[String]>, String> {
    let Some(a) = argv else { return Ok(None) };
    if a.is_empty() || a[0].is_empty() {
        return Err("argv vacío".into());
    }
    if a.len() > ARGV_MAX_ITEMS || a.iter().map(String::len).sum::<usize>() > ARGV_MAX_BYTES {
        return Err("argv demasiado grande".into());
    }
    if a.iter().any(|s| s.contains('\0')) {
        return Err("argv con NUL".into());
    }
    Ok(Some(a))
}

/// `mish` del bundle (hermano de este ejecutable, como en el daemon), si no
/// `~/.cargo/bin/mish` y si tampoco, `mish` a secas (PATH).
pub fn mish_path() -> String {
    if let Some(p) = mish_updater::current_bundle().map(|b| b.join("Contents/MacOS/mish"))
        && p.is_file()
    {
        return p.to_string_lossy().into_owned();
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let p = format!("{home}/.cargo/bin/mish");
    if std::path::Path::new(&p).exists() { p } else { "mish".into() }
}

/// `dir` delante de `path` (sin duplicarlo si ya es el primero): las sesiones encuentran el
/// `mish` del bundle (`mish update`) aunque no haya cargo. zsh -il puede reordenar el PATH;
/// por eso también se exporta `MISH_BUNDLE`.
pub fn path_with_first(dir: &Path, path: Option<&OsStr>) -> std::ffi::OsString {
    let rest: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).filter(|d| d != dir).collect()).unwrap_or_default();
    std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest)).unwrap_or_else(|_| {
        let mut s = dir.as_os_str().to_owned();
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            s.push(":");
            s.push(p);
        }
        s
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

struct Session {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    cols: u16,
    rows: u16,
    started: u64,
}

fn spawn_session(
    id: u32,
    cols: u16,
    rows: u16,
    cwd: Option<&str>,
    argv: Option<&[String]>,
    proxy: EventLoopProxy<UserEvent>,
) -> Result<Session, Box<dyn std::error::Error>> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

    // zsh -il carga ~/.zprofile y ~/.zshrc (PATH de brew, cargo, claude…) y se sustituye por mish.
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let mut cmd = CommandBuilder::new("/bin/zsh");
    match argv {
        // argv explícito: sin interpolar nada en la línea de comandos de zsh.
        Some(a) => {
            cmd.args(["-il", "-c", "exec \"$@\"", "mish-app"]);
            cmd.args(a);
        }
        None => {
            // MISH_APP_CMD (desarrollo): ejecutar otro comando en vez de mish.
            let target = std::env::var("MISH_APP_CMD").unwrap_or_else(|_| format!("exec '{}'", mish_path().replace('\'', "'\\''")));
            cmd.args(["-il", "-c", &target]);
        }
    }
    cmd.cwd(start_dir(cwd, &home));
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "mish-app");
    cmd.env("MISH_SESSION", id.to_string());
    cmd.env("MISH_SOCK", mish_proto::socket_path());
    if let Some(bundle) = mish_updater::current_bundle() {
        cmd.env("PATH", path_with_first(&bundle.join("Contents/MacOS"), std::env::var_os("PATH").as_deref()));
        cmd.env("MISH_BUNDLE", bundle);
    }
    if std::env::var("LANG").is_err() {
        cmd.env("LANG", "es_ES.UTF-8");
    }
    let child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if proxy.send_event(UserEvent::Output(id, buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = proxy.send_event(UserEvent::Exit(id));
    });
    Ok(Session { writer, master: pair.master, child, cols, rows, started: now_secs() })
}

/// PTYs dentro del propio proceso de la app (comportamiento anterior al daemon). Las
/// sesiones mueren al cerrar la ventana.
pub struct LocalBackend {
    sessions: HashMap<u32, Session>,
    proxy: EventLoopProxy<UserEvent>,
}

impl LocalBackend {
    pub fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        LocalBackend { sessions: HashMap::new(), proxy }
    }
}

impl Backend for LocalBackend {
    fn mode(&self) -> Mode {
        Mode::Local
    }

    fn new_session(&mut self, id: u32, cols: u16, rows: u16, cwd: Option<&str>, argv: Option<&[String]>)
    -> Result<(), String> {
        let argv = validate_argv(argv)?;
        if let Some(mut old) = self.sessions.remove(&id) {
            let _ = old.child.kill();
        }
        let s = spawn_session(id, cols, rows, cwd, argv, self.proxy.clone()).map_err(|e| e.to_string())?;
        self.sessions.insert(id, s);
        Ok(())
    }

    fn attach(&mut self, id: u32, cols: u16, rows: u16) -> Result<(), String> {
        if !self.sessions.contains_key(&id) {
            // Ya no existe: que app.js la trate como terminada.
            let _ = self.proxy.send_event(UserEvent::Exit(id));
            return Err(format!("la sesión {id} no existe"));
        }
        self.resize(id, cols, rows);
        let _ = self.proxy.send_event(UserEvent::ReplayEnd(id));
        Ok(())
    }

    fn input(&mut self, id: u32, data: &[u8]) -> bool {
        let Some(s) = self.sessions.get_mut(&id) else { return false };
        let ok = s.writer.write_all(data).is_ok();
        let _ = s.writer.flush();
        ok
    }

    fn resize(&mut self, id: u32, cols: u16, rows: u16) {
        if let Some(s) = self.sessions.get_mut(&id) {
            s.cols = cols;
            s.rows = rows;
            let _ = s.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        }
    }

    fn close(&mut self, id: u32) {
        if let Some(mut s) = self.sessions.remove(&id) {
            let _ = s.child.kill();
        }
    }

    fn exited(&mut self, id: u32) {
        if let Some(mut s) = self.sessions.remove(&id) {
            let _ = s.child.wait();
        }
    }

    fn kill_all(&mut self) {
        for (_, mut s) in self.sessions.drain() {
            let _ = s.child.kill();
        }
    }

    fn list(&mut self) {
        let mut list: Vec<SessionInfo> = self
            .sessions
            .iter()
            .map(|(&id, s)| SessionInfo {
                id,
                pid: s.child.process_id().unwrap_or(0),
                cwd: None,
                title: None,
                tint: None,
                cols: s.cols,
                rows: s.rows,
                started: s.started,
                claude: None,
            })
            .collect();
        list.sort_by_key(|s| s.id);
        let _ = self.proxy.send_event(UserEvent::Sessions(list));
    }

    fn shutdown(&mut self, _kill: bool) {
        self.kill_all();
    }
}

/// Sin sesiones: la ventana quedó en solo lectura tras `taken_over`. Todo se ignora.
pub struct ReadOnlyBackend;

impl Backend for ReadOnlyBackend {
    fn mode(&self) -> Mode {
        Mode::ReadOnly
    }
    fn new_session(&mut self, _: u32, _: u16, _: u16, _: Option<&str>, _: Option<&[String]>) -> Result<(), String> {
        Err("ventana en solo lectura: otra ventana de Mish controla las sesiones".into())
    }
    fn attach(&mut self, _: u32, _: u16, _: u16) -> Result<(), String> {
        Err("ventana en solo lectura".into())
    }
    fn input(&mut self, _: u32, _: &[u8]) -> bool {
        false
    }
    fn resize(&mut self, _: u32, _: u16, _: u16) {}
    fn close(&mut self, _: u32) {}
    fn kill_all(&mut self) {}
    fn list(&mut self) {}
    fn shutdown(&mut self, _: bool) {}
}
