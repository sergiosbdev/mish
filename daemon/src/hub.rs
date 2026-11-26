//! Hub: un solo hilo dueño de todo el estado (sesiones y clientes). El resto
//! de hilos (accept, lectores/escritores de clientes, lectores/escritores/
//! reapers de PTYs) solo le mandan [`Event`]s por un mpsc.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mish_proto::{Ctl, Frame, MAX_DATA, PROTO, Role, SessionInfo, encode_frame};
use portable_pty::{MasterPty, PtySize};

use crate::claude::{Claude, State as ClaudeState};
use crate::client::{self, Bytes};
use crate::log;
use crate::notify::Notifier;
use crate::ring::{RING_CAP, Ring};
use crate::session::{self, SpawnReq};
use crate::vtscan::{VtScan, strip_queries};

pub enum Event {
    /// Conexión aceptada (UID ya comprobado).
    Conn(UnixStream),
    Frame {
        cid: u64,
        frame: Frame,
    },
    /// Frame inválido pero el flujo sigue alineado.
    BadFrame {
        cid: u64,
        why: String,
    },
    /// El cliente cerró o rompió el framing.
    Gone {
        cid: u64,
        why: String,
    },
    PtyOut {
        key: u64,
        data: Vec<u8>,
    },
    PtyEof {
        key: u64,
    },
    ChildExit {
        key: u64,
    },
}

pub struct Config {
    pub sock: PathBuf,
    pub build: String,
    pub hostname: String,
    pub idle: Duration,
}

/// Retraso entre los dos resizes del "rebote" tras un attach.
const BOUNCE_DELAY: Duration = Duration::from_millis(40);
/// Tras salir el hijo, margen para recoger la salida pendiente del PTY.
const EOF_GRACE: Duration = Duration::from_millis(500);
/// Tras SIGHUP, cuánto se espera antes de SIGKILL.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Margen de `shutdown` para que los hijos salgan solos.
const SHUTDOWN_GRACE: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CState {
    AwaitHello,
    Ready(Role),
    /// `proto` distinto: solo se acepta `shutdown`.
    Incompatible,
}

struct Client {
    stream: UnixStream,
    tx: SyncSender<Bytes>,
    state: CState,
    attached: HashSet<u32>,
}

struct Session {
    key: u64,
    pid: u32,
    master: Box<dyn MasterPty + Send>,
    input: Sender<Vec<u8>>,
    ring: Ring,
    /// Estado actual (metadatos y modos al final del anillo).
    scan: VtScan,
    /// Modos al *inicio* del anillo: se alimenta con lo que se recorta. Su
    /// prefijo + el contenido del anillo reproducen el estado actual.
    base: VtScan,
    cols: u16,
    rows: u16,
    started: u64,
    attached: HashSet<u64>,
    exited: bool,
    eof: bool,
    finalize_at: Option<Instant>,
    kill_at: Option<Instant>,
    bounce: Option<(Instant, u16, u16)>,
    /// Estado de Claude Code (hooks).
    claude: Claude,
}

impl Session {
    fn info(&self, id: u32) -> SessionInfo {
        SessionInfo {
            id,
            pid: self.pid,
            cwd: self.scan.cwd().map(str::to_string),
            title: self.scan.title().map(str::to_string),
            tint: self.scan.tint().map(str::to_string),
            cols: self.cols,
            rows: self.rows,
            started: self.started,
            claude: self.claude.state.info(),
        }
    }

    fn meta(&self, id: u32) -> Ctl {
        Ctl::Meta {
            id,
            title: self.scan.title().map(str::to_string),
            cwd: self.scan.cwd().map(str::to_string),
            tint: self.scan.tint().map(str::to_string),
        }
    }

    fn claude_ctl(&self, id: u32) -> Ctl {
        Ctl::Claude {
            id,
            state: self.claude.state.as_str().to_string(),
            detail: self.claude.detail.clone(),
        }
    }

    /// Título de la notificación: "Claude · <carpeta>".
    fn notify_title(&self) -> String {
        let dir = self
            .scan
            .cwd()
            .and_then(|c| c.trim_end_matches('/').rsplit('/').next())
            .filter(|d| !d.is_empty());
        match dir {
            Some(d) => format!("Claude · {d}"),
            None => "Claude".to_string(),
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }

    /// Señal al grupo de procesos del hijo (es líder de su sesión: pgid == pid).
    fn signal(&self, sig: libc::c_int) {
        if self.exited || self.pid <= 1 {
            return;
        }
        // SAFETY: kill no tiene precondiciones de memoria.
        unsafe {
            if libc::kill(-(self.pid as libc::pid_t), sig) != 0 {
                libc::kill(self.pid as libc::pid_t, sig);
            }
        }
    }
}

pub struct Hub {
    cfg: Config,
    events_tx: SyncSender<Event>,
    clients: HashMap<u64, Client>,
    sessions: BTreeMap<u32, Session>,
    keys: HashMap<u64, u32>,
    gui: Option<u64>,
    next_cid: u64,
    next_key: u64,
    idle_since: Option<Instant>,
    shutdown_at: Option<Instant>,
    notifier: Notifier,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn enc(f: &Frame) -> Option<Bytes> {
    match encode_frame(f) {
        Ok(b) => Some(Arc::new(b)),
        Err(e) => {
            log!("error codificando frame: {e}");
            None
        }
    }
}

fn ctl(c: Ctl) -> Option<Bytes> {
    enc(&Frame::Ctl(c))
}

fn short(s: &str) -> String {
    s.chars()
        .take(160)
        .collect::<String>()
        .escape_debug()
        .to_string()
}

impl Hub {
    pub fn new(cfg: Config, events_tx: SyncSender<Event>) -> Hub {
        Hub {
            cfg,
            events_tx,
            clients: HashMap::new(),
            sessions: BTreeMap::new(),
            keys: HashMap::new(),
            gui: None,
            next_cid: 1,
            next_key: 1,
            idle_since: Some(Instant::now()),
            shutdown_at: None,
            notifier: Notifier::from_env(),
        }
    }

    /// Bucle principal. Vuelve cuando el daemon debe terminar.
    pub fn run(&mut self, rx: Receiver<Event>) {
        loop {
            let timeout = self.next_timeout();
            match rx.recv_timeout(timeout) {
                Ok(ev) => {
                    self.handle(ev);
                    // Procesa en lote lo que ya esté en cola (acotado, para que los
                    // temporizadores corran aunque una sesión no pare de escribir).
                    for _ in 0..256 {
                        let Ok(ev) = rx.try_recv() else { break };
                        self.handle(ev);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if self.timers() {
                return;
            }
        }
    }

    fn next_timeout(&self) -> Duration {
        let now = Instant::now();
        let mut next = now + Duration::from_secs(1);
        let mut consider = |t: Instant| next = next.min(t);
        for s in self.sessions.values() {
            for t in [s.finalize_at, s.kill_at, s.bounce.map(|(t, _, _)| t)]
                .into_iter()
                .flatten()
            {
                consider(t);
            }
        }
        if let Some(t) = self.shutdown_at {
            consider(t);
        }
        if let Some(t) = self.idle_since {
            consider(t + self.cfg.idle);
        }
        next.saturating_duration_since(now)
            .max(Duration::from_millis(5))
    }

    /// Temporizadores. Devuelve `true` si hay que salir.
    fn timers(&mut self) -> bool {
        let now = Instant::now();
        let mut finalize = Vec::new();
        for (&id, s) in self.sessions.iter_mut() {
            if let Some((t, cols, rows)) = s.bounce
                && now >= t
            {
                s.bounce = None;
                s.resize(cols, rows);
            }
            if s.kill_at.is_some_and(|t| now >= t) {
                s.kill_at = None;
                s.signal(libc::SIGKILL);
            }
            if s.finalize_at.is_some_and(|t| now >= t) {
                finalize.push(id);
            }
        }
        for id in finalize {
            self.finalize(id);
        }

        if let Some(t) = self.shutdown_at {
            if self.sessions.is_empty() || now >= t {
                for s in self.sessions.values() {
                    s.signal(libc::SIGKILL);
                }
                log!("apagado");
                self.close_all_clients();
                return true;
            }
            return false;
        }

        if self.sessions.is_empty() && self.clients.is_empty() {
            let since = *self.idle_since.get_or_insert(now);
            if now.duration_since(since) >= self.cfg.idle {
                log!("inactivo {} ms: saliendo", self.cfg.idle.as_millis());
                return true;
            }
        } else {
            self.idle_since = None;
        }
        false
    }

    fn handle(&mut self, ev: Event) {
        match ev {
            Event::Conn(stream) => self.on_conn(stream),
            Event::Frame { cid, frame } => self.on_frame(cid, frame),
            Event::BadFrame { cid, why } => {
                let Some(c) = self.clients.get(&cid) else {
                    return;
                };
                log!("cliente {cid}: frame inválido: {}", short(&why));
                if c.state == CState::AwaitHello {
                    self.drop_client(cid, false);
                } else {
                    self.send(
                        cid,
                        ctl(Ctl::Error {
                            id: None,
                            msg: "frame inválido".into(),
                        }),
                    );
                }
            }
            Event::Gone { cid, why } => {
                if self.clients.contains_key(&cid) {
                    log!("cliente {cid} desconectado ({})", short(&why));
                    self.drop_client(cid, false);
                }
            }
            Event::PtyOut { key, data } => self.on_pty_out(key, data),
            Event::PtyEof { key } => {
                let Some(&id) = self.keys.get(&key) else {
                    return;
                };
                let s = self.sessions.get_mut(&id).expect("key sin sesión");
                s.eof = true;
                if s.exited {
                    self.finalize(id);
                } else if s.kill_at.is_none() {
                    // El hijo soltó el terminal sin salir: se le pide que salga.
                    s.signal(libc::SIGHUP);
                    s.kill_at = Some(Instant::now() + KILL_GRACE);
                }
            }
            Event::ChildExit { key } => {
                let Some(&id) = self.keys.get(&key) else {
                    return;
                };
                let s = self.sessions.get_mut(&id).expect("key sin sesión");
                s.exited = true;
                s.kill_at = None;
                if s.eof {
                    self.finalize(id);
                } else {
                    s.finalize_at = Some(Instant::now() + EOF_GRACE);
                }
            }
        }
    }

    fn on_conn(&mut self, stream: UnixStream) {
        if self.shutdown_at.is_some() {
            return; // se cierra al soltarlo
        }
        let cid = self.next_cid;
        self.next_cid += 1;
        match client::start(cid, &stream, self.events_tx.clone()) {
            Ok(tx) => {
                log!("cliente {cid} conectado");
                self.clients.insert(
                    cid,
                    Client {
                        stream,
                        tx,
                        state: CState::AwaitHello,
                        attached: HashSet::new(),
                    },
                );
            }
            Err(e) => log!("no se pudo atender la conexión: {e}"),
        }
    }

    /// Encola un frame para un cliente. Si su cola está llena, se le desconecta.
    fn send(&mut self, cid: u64, buf: Option<Bytes>) {
        let Some(buf) = buf else { return };
        let Some(c) = self.clients.get(&cid) else {
            return;
        };
        match c.tx.try_send(buf) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                log!("cliente {cid}: cola llena, se desconecta");
                self.drop_client(cid, false);
            }
            Err(TrySendError::Disconnected(_)) => self.drop_client(cid, false),
        }
    }

    /// Quita un cliente. `graceful`: deja que el escritor vacíe su cola (p. ej.
    /// para que llegue `taken_over`) antes de cerrar; si no, se cierra ya.
    fn drop_client(&mut self, cid: u64, graceful: bool) {
        let Some(c) = self.clients.remove(&cid) else {
            return;
        };
        for sid in &c.attached {
            if let Some(s) = self.sessions.get_mut(sid) {
                s.attached.remove(&cid);
            }
        }
        if self.gui == Some(cid) {
            self.gui = None;
        }
        if !graceful {
            let _ = c.stream.shutdown(Shutdown::Both);
        }
        // Al soltar `c.tx`, el escritor termina tras vaciar la cola y cierra el socket.
    }

    fn close_all_clients(&mut self) {
        let cids: Vec<u64> = self.clients.keys().copied().collect();
        for cid in cids {
            self.drop_client(cid, true);
        }
    }

    fn ready_clients(&self) -> Vec<u64> {
        self.clients
            .iter()
            .filter(|(_, c)| matches!(c.state, CState::Ready(_)))
            .map(|(&k, _)| k)
            .collect()
    }

    fn on_frame(&mut self, cid: u64, frame: Frame) {
        let Some(c) = self.clients.get(&cid) else {
            return;
        };
        match c.state {
            CState::AwaitHello => match frame {
                Frame::Ctl(Ctl::Hello { proto, role, build }) => {
                    self.on_hello(cid, proto, role, &build)
                }
                _ => {
                    log!("cliente {cid}: primer frame no es hello");
                    self.send(
                        cid,
                        ctl(Ctl::Error {
                            id: None,
                            msg: "se esperaba hello".into(),
                        }),
                    );
                    self.drop_client(cid, true);
                }
            },
            CState::Incompatible => match frame {
                Frame::Ctl(Ctl::Shutdown { .. }) => self.start_shutdown(cid),
                _ => self.send(
                    cid,
                    ctl(Ctl::Error {
                        id: None,
                        msg: format!("protocolo incompatible (daemon {PROTO})"),
                    }),
                ),
            },
            CState::Ready(role) => self.on_ready_frame(cid, role, frame),
        }
    }

    fn on_hello(&mut self, cid: u64, proto: u32, role: Role, build: &str) {
        log!(
            "cliente {cid}: hello role={role:?} proto={proto} build={}",
            short(build)
        );
        self.send(
            cid,
            ctl(Ctl::Welcome {
                proto: PROTO,
                build: self.cfg.build.clone(),
                pid: std::process::id(),
                hostname: self.cfg.hostname.clone(),
            }),
        );
        let Some(c) = self.clients.get_mut(&cid) else {
            return;
        };
        if proto != PROTO {
            c.state = CState::Incompatible;
            return;
        }
        c.state = CState::Ready(role);
        if role == Role::Gui {
            if let Some(old) = self.gui.replace(cid)
                && old != cid
            {
                log!("cliente {old}: sustituido por la GUI {cid}");
                self.send(old, ctl(Ctl::TakenOver {}));
                self.drop_client(old, true);
            }
            self.gui = Some(cid);
        }
    }

    fn err(&mut self, cid: u64, id: Option<u32>, msg: impl Into<String>) {
        self.send(
            cid,
            ctl(Ctl::Error {
                id,
                msg: msg.into(),
            }),
        );
    }

    fn on_ready_frame(&mut self, cid: u64, role: Role, frame: Frame) {
        if role == Role::Hook {
            // Los hooks solo consultan/cambian estado: nunca escriben al PTY ni gestionan sesiones.
            match frame {
                Frame::Ctl(Ctl::List { .. }) => self.send_list(cid),
                Frame::Ctl(Ctl::Hook {
                    session,
                    event,
                    ntype,
                    tool,
                    msg,
                }) => self.on_hook(
                    session,
                    &event,
                    ntype.as_deref(),
                    tool.as_deref(),
                    msg.as_deref(),
                ),
                _ => self.err(cid, None, "no permitido para hooks"),
            }
            return;
        }
        match frame {
            Frame::In { sid, data } => match self.sessions.get(&sid) {
                Some(s) => {
                    let _ = s.input.send(data);
                }
                None => self.err(cid, Some(sid), "sesión inexistente"),
            },
            Frame::Ctl(c) => self.on_ctl(cid, c),
            Frame::Out { .. } | Frame::Replay { .. } => {
                self.err(cid, None, "frame no válido del cliente")
            }
        }
    }

    fn on_ctl(&mut self, cid: u64, c: Ctl) {
        match c {
            Ctl::List { .. } => self.send_list(cid),
            Ctl::New {
                id,
                cols,
                rows,
                cwd,
                argv,
            } => self.new_session(cid, id, cols, rows, cwd, argv),
            Ctl::Attach { id, cols, rows } => self.attach(cid, id, cols, rows),
            Ctl::Detach { id } => {
                if let Some(c) = self.clients.get_mut(&cid) {
                    c.attached.remove(&id);
                }
                if let Some(s) = self.sessions.get_mut(&id) {
                    s.attached.remove(&cid);
                }
            }
            Ctl::Resize { id, cols, rows } => match self.sessions.get_mut(&id) {
                Some(s) => {
                    let (cols, rows) = (cols.max(1), rows.max(1));
                    s.bounce = None;
                    s.cols = cols;
                    s.rows = rows;
                    s.resize(cols, rows);
                }
                None => self.err(cid, Some(id), "sesión inexistente"),
            },
            Ctl::Close { id } => {
                if self.sessions.contains_key(&id) {
                    self.kill(id);
                } else {
                    self.err(cid, Some(id), "sesión inexistente");
                }
            }
            Ctl::KillAll { .. } => {
                let ids: Vec<u32> = self.sessions.keys().copied().collect();
                log!("cliente {cid}: kill_all ({} sesiones)", ids.len());
                for id in ids {
                    self.kill(id);
                }
            }
            Ctl::Shutdown { .. } => self.start_shutdown(cid),
            Ctl::Hello { .. } => self.err(cid, None, "hello repetido"),
            _ => self.err(cid, None, "mensaje no válido del cliente"),
        }
    }

    /// Evento de un hook de Claude. Sin respuesta: si la sesión no existe
    /// (p. ej. `MISH_SESSION` de un daemon anterior) se ignora en silencio.
    fn on_hook(
        &mut self,
        id: u32,
        event: &str,
        ntype: Option<&str>,
        tool: Option<&str>,
        msg: Option<&str>,
    ) {
        let Some(s) = self.sessions.get_mut(&id) else {
            return;
        };
        if s.claude.apply(event, ntype, tool, msg) {
            self.claude_changed(id);
        }
    }

    /// El estado de Claude de `id` cambió: se manda a la GUI o, si no hay
    /// GUI y pasa a waiting/done, se notifica al sistema.
    fn claude_changed(&mut self, id: u32) {
        let Some(s) = self.sessions.get(&id) else {
            return;
        };
        let state = s.claude.state;
        let gui = self.gui.filter(|g| {
            matches!(
                self.clients.get(g).map(|c| c.state),
                Some(CState::Ready(Role::Gui))
            )
        });
        match gui {
            Some(g) => {
                let m = ctl(s.claude_ctl(id));
                self.send(g, m);
            }
            None if state.notifies() => {
                let title = s.notify_title();
                let msg = match (state, s.claude.detail.as_deref()) {
                    (ClaudeState::Waiting, Some(d)) => d.to_string(),
                    (ClaudeState::Waiting, None) => "Necesita tu atención".to_string(),
                    _ => "Ha terminado".to_string(),
                };
                self.notifier.notify(id, state.as_str(), &title, &msg);
            }
            None => {}
        }
    }

    fn send_list(&mut self, cid: u64) {
        let list = self.sessions.iter().map(|(&id, s)| s.info(id)).collect();
        self.send(cid, ctl(Ctl::Sessions { list }));
    }

    fn new_session(
        &mut self,
        cid: u64,
        id: u32,
        cols: u16,
        rows: u16,
        cwd: Option<String>,
        argv: Option<Vec<String>>,
    ) {
        if self.sessions.contains_key(&id) {
            return self.err(cid, Some(id), "ya existe una sesión con ese id");
        }
        let (cols, rows) = (cols.max(1), rows.max(1));
        let key = self.next_key;
        self.next_key += 1;
        let req = SpawnReq {
            id,
            key,
            cols,
            rows,
            cwd: cwd.as_deref(),
            argv: argv.as_deref(),
            sock: &self.cfg.sock,
        };
        let sp = match session::spawn(req, self.events_tx.clone()) {
            Ok(sp) => sp,
            Err(e) => {
                log!("sesión {id}: no se pudo lanzar: {e}");
                return self.err(cid, Some(id), format!("no se pudo lanzar la sesión: {e}"));
            }
        };
        log!(
            "sesión {id}: nueva pid={} {cols}x{rows} (cliente {cid})",
            sp.pid
        );
        let mut attached = HashSet::new();
        attached.insert(cid);
        self.sessions.insert(
            id,
            Session {
                key,
                pid: sp.pid,
                master: sp.master,
                input: sp.input,
                ring: Ring::new(RING_CAP),
                scan: VtScan::new(&self.cfg.hostname),
                base: VtScan::new(&self.cfg.hostname),
                cols,
                rows,
                started: now_secs(),
                attached,
                exited: false,
                eof: false,
                finalize_at: None,
                kill_at: None,
                bounce: None,
                claude: Claude::default(),
            },
        );
        self.keys.insert(key, id);
        if let Some(c) = self.clients.get_mut(&cid) {
            c.attached.insert(id);
        }
        // Replay vacío: así `new` y `attach` terminan igual para el cliente.
        self.send(cid, ctl(Ctl::ReplayEnd { id }));
    }

    fn attach(&mut self, cid: u64, id: u32, cols: u16, rows: u16) {
        let Some(s) = self.sessions.get_mut(&id) else {
            return self.err(cid, Some(id), "sesión inexistente");
        };
        s.attached.insert(cid);
        let mut data = s.base.modes().prefix();
        // Sin consultas: el terminal del cliente las respondería como si fueran teclado.
        data.extend(strip_queries(&s.ring.contents()));
        let meta = (s.scan.title().is_some() || s.scan.cwd().is_some() || s.scan.tint().is_some())
            .then(|| s.meta(id));

        // Tamaño: si cambia, basta un resize (SIGWINCH). Si es el mismo, rebote
        // (cols±1 y vuelta) para que los TUIs repinten sobre el replay.
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) != (s.cols, s.rows) {
            s.bounce = None;
            s.resize(cols, rows);
        } else {
            let tmp = if cols > 1 { cols - 1 } else { cols + 1 };
            s.resize(tmp, rows);
            s.bounce = Some((Instant::now() + BOUNCE_DELAY, cols, rows));
        }
        s.cols = cols;
        s.rows = rows;

        if let Some(c) = self.clients.get_mut(&cid) {
            c.attached.insert(id);
        }
        for chunk in data.chunks(MAX_DATA) {
            self.send(
                cid,
                enc(&Frame::Replay {
                    sid: id,
                    data: chunk.to_vec(),
                }),
            );
        }
        self.send(cid, ctl(Ctl::ReplayEnd { id }));
        if let Some(m) = meta {
            self.send(cid, ctl(m));
        }
    }

    fn on_pty_out(&mut self, key: u64, data: Vec<u8>) {
        let Some(&id) = self.keys.get(&key) else {
            return;
        };
        let s = self.sessions.get_mut(&id).expect("key sin sesión");
        let dropped = s.ring.push(&data);
        if !dropped.is_empty() {
            s.base.feed(&dropped);
        }
        let changed = s.scan.feed(&data);
        let meta = changed.then(|| ctl(s.meta(id))).flatten();
        // Un OSC 7 local de la shell = ha vuelto el prompt: Claude ya no corre aquí.
        if s.scan.saw_osc7() && s.claude.reset() {
            self.claude_changed(id);
        }
        let Some(s) = self.sessions.get(&id) else {
            return;
        };
        let targets: Vec<u64> = s.attached.iter().copied().collect();
        if targets.is_empty() {
            return;
        }
        let out: Vec<Bytes> = data
            .chunks(MAX_DATA)
            .filter_map(|c| {
                enc(&Frame::Out {
                    sid: id,
                    data: c.to_vec(),
                })
            })
            .collect();
        for cid in targets {
            for b in &out {
                self.send(cid, Some(b.clone()));
            }
            if let Some(m) = &meta {
                self.send(cid, Some(m.clone()));
            }
        }
    }

    /// Cierra una sesión: SIGHUP al grupo y SIGKILL si no sale a tiempo.
    fn kill(&mut self, id: u32) {
        let Some(s) = self.sessions.get_mut(&id) else {
            return;
        };
        if s.exited {
            return;
        }
        log!("sesión {id}: cerrando");
        s.signal(libc::SIGHUP);
        s.kill_at.get_or_insert(Instant::now() + KILL_GRACE);
    }

    /// Sesión terminada: se quita y se avisa con `exit` a los clientes.
    fn finalize(&mut self, id: u32) {
        let Some(s) = self.sessions.remove(&id) else {
            return;
        };
        self.keys.remove(&s.key);
        self.notifier.forget(id);
        log!("sesión {id}: terminada");
        for cid in &s.attached {
            if let Some(c) = self.clients.get_mut(cid) {
                c.attached.remove(&id);
            }
        }
        let exit = ctl(Ctl::Exit { id });
        for cid in self.ready_clients() {
            self.send(cid, exit.clone());
        }
        // Al soltar `s` se cierran el master y el canal del escritor.
    }

    fn start_shutdown(&mut self, cid: u64) {
        if self.shutdown_at.is_some() {
            return;
        }
        log!("cliente {cid}: shutdown ({} sesiones)", self.sessions.len());
        self.shutdown_at = Some(Instant::now() + SHUTDOWN_GRACE);
        let ids: Vec<u32> = self.sessions.keys().copied().collect();
        for id in ids {
            self.kill(id);
        }
    }
}
