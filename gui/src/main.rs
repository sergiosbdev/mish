//! Mish.app: ventana nativa (tao + wry) con xterm.js que ejecuta `mish` en un PTY propio.
//! Sin Terminal.app ni zsh interactivo por medio.
//!
//! Las sesiones viven en `mish-daemon` (sobreviven al cierre de la ventana) o, con
//! `MISH_NO_DAEMON=1` o si el daemon no está disponible, en este mismo proceso (ver
//! [`backend`]).

mod backend;
mod daemon_client;
mod envpath;
mod gitpanel;
mod js;
mod menu;
mod updates;
mod vault;

use backend::{Backend, LocalBackend, Mode};
use base64::Engine;
use mish_proto::SessionInfo;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tao::{
    dpi::LogicalSize,
    event::{Event, StartCause, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy},
    platform::macos::{WindowBuilderExtMacOS, WindowExtMacOS},
    window::{Window, WindowBuilder},
};
use wry::{
    http::{header::CONTENT_TYPE, Response},
    PageLoadEvent, WebViewBuilder,
};
use zeroize::{Zeroize, Zeroizing};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Tiempo tras el que se vacía el portapapeles si sigue conteniendo el secreto copiado.
const CLIPBOARD_CLEAR: std::time::Duration = std::time::Duration::from_secs(30);

/// Margen para que app.js mande `save_layout` tras `mishBeforeQuit()` antes de salir.
const QUIT_GRACE: Duration = Duration::from_millis(500);

/// Tamaño máximo de `tabs.json` (validado al guardar y al leer).
const LAYOUT_MAX: usize = 256 * 1024;

pub enum UserEvent {
    /// Salida en vivo de una sesión.
    Output(u32, Vec<u8>),
    /// Scrollback al engancharse a una sesión del daemon (al webview igual que `Output`).
    Replay(u32, Vec<u8>),
    /// Fin del replay de la sesión (también tras `attach` en modo local).
    ReplayEnd(u32),
    Exit(u32),
    /// Metadatos extraídos por el daemon (OSC 0/2/7/7701).
    Meta { id: u32, title: Option<String>, cwd: Option<String>, tint: Option<String> },
    /// Respuesta a `Backend::list`.
    Sessions(Vec<SessionInfo>),
    /// Cambio del estado de Claude Code en una sesión (hooks, vía daemon).
    Claude { id: u32, state: String, detail: Option<String> },
    DaemonError { id: Option<u32>, msg: String },
    /// Otra GUI se conectó al daemon: esta ventana pasa a solo lectura.
    TakenOver,
    /// Se perdió la conexión con el daemon sin pedirlo.
    DaemonLost,
    /// La página del webview empezó/terminó de cargar.
    Page(PageLoadEvent),
    Ipc(String),
    /// Fin de crear/desbloquear la caja fuerte (Argon2 corre fuera del event loop).
    VaultDone(vault::Done),
    /// Ítem activado en la barra de menús nativa (muda).
    Menu(muda::MenuId),
    /// Paso del hilo de actualización (check, progreso, instalación).
    Update(updates::Ev),
    /// Respuesta de una petición del panel git (`window.mishGit`).
    Git(gitpanel::Reply),
}

#[derive(Deserialize)]
#[serde(tag = "t")]
enum Msg {
    #[serde(rename = "new")]
    New {
        id: u32,
        cols: u16,
        rows: u16,
        /// cwd heredado de la pestaña activa (OSC 7). Se valida en `start_dir`.
        #[serde(default)]
        cwd: Option<String>,
        /// Comando a ejecutar en vez de la shell (validado en `backend::validate_argv`).
        #[serde(default)]
        argv: Option<Vec<String>>,
    },
    /// Engancharse a una sesión viva (de `mishBoot.sessions`).
    #[serde(rename = "attach")]
    Attach { id: u32, cols: u16, rows: u16 },
    /// Guarda `tabs.json`. `json` es el texto del layout (o el objeto directamente).
    #[serde(rename = "save_layout")]
    SaveLayout { json: serde_json::Value },
    /// Mata todas las sesiones (sin salir).
    #[serde(rename = "kill_all")]
    KillAll,
    /// ⌘⌥Q: mata todas las sesiones y sale.
    #[serde(rename = "quit_all")]
    QuitAll,
    /// Botón de un aviso (`mishNotice`): `restart` apaga el daemon (mata sus sesiones), lo
    /// relanza y recarga la página; `reattach` vuelve a conectar como GUI (recupera el
    /// control tras `taken_over`, o reconecta tras perder el daemon) y repite `mishBoot`;
    /// `local` (seguir en local) no hace nada. Actualizaciones (ver [`updates`]): `update`
    /// descarga e instala, `retry` vuelve a comprobar, `later` cierra y `relaunch` reinicia la
    /// app (no el daemon) tras instalar.
    #[serde(rename = "notice_action")]
    NoticeAction { id: String },
    /// Buscar actualizaciones (OSC 7703 de `mish update`): solo comprueba y muestra el aviso;
    /// instalar requiere el clic en «Actualizar» (`notice_action` "update"). Campos extra
    /// (p. ej. un `install` antiguo) se ignoran.
    #[serde(rename = "update_check")]
    UpdateCheck {},
    /// Opcional: app.js lista para `mishBoot` (normalmente basta el fin de carga).
    #[serde(rename = "ready")]
    Ready,
    #[serde(rename = "in")]
    Input { id: u32, d: String },
    #[serde(rename = "resize")]
    Resize { id: u32, cols: u16, rows: u16 },
    #[serde(rename = "close")]
    Close { id: u32 },
    #[serde(rename = "copy")]
    Copy { text: String },
    #[serde(rename = "paste")]
    Paste { id: u32 },
    #[serde(rename = "open")]
    Open { url: String },
    #[serde(rename = "notify")]
    Notify { title: String, msg: String },
    #[serde(rename = "drag")]
    Drag,
    #[serde(rename = "zoom")]
    Zoom,
    #[serde(rename = "fullscreen")]
    Fullscreen,
    #[serde(rename = "quit")]
    Quit,
    // Caja fuerte. Las contraseñas y secretos llegan en Zeroizing; nunca vuelven al webview.
    #[serde(rename = "vault_status")]
    VaultStatus,
    #[serde(rename = "vault_create")]
    VaultCreate { pw: Zeroizing<String> },
    #[serde(rename = "vault_unlock")]
    VaultUnlock { pw: Zeroizing<String> },
    #[serde(rename = "vault_lock")]
    VaultLock,
    #[serde(rename = "vault_add")]
    VaultAdd { name: String, user: String, secret: Zeroizing<String> },
    #[serde(rename = "vault_delete")]
    VaultDelete { name: String },
    #[serde(rename = "vault_type")]
    VaultType { name: String, id: u32 },
    #[serde(rename = "vault_copy")]
    VaultCopy { name: String },
    /// Panel git: `{t:"git", req, op, sid, …}` (ver [`gitpanel`]).
    #[serde(rename = "git")]
    Git(gitpanel::Req),
}

fn asset(path: &str) -> Option<(&'static [u8], &'static str)> {
    Some(match path {
        "/" | "/index.html" => (include_bytes!("../assets/index.html"), "text/html"),
        "/app.js" => (include_bytes!("../assets/app.js"), "text/javascript"),
        "/app.css" => (include_bytes!("../assets/app.css"), "text/css"),
        "/xterm.js" => (include_bytes!("../assets/xterm.js"), "text/javascript"),
        "/xterm.css" => (include_bytes!("../assets/xterm.css"), "text/css"),
        "/addon-fit.js" => (include_bytes!("../assets/addon-fit.js"), "text/javascript"),
        "/addon-webgl.js" => (include_bytes!("../assets/addon-webgl.js"), "text/javascript"),
        "/addon-web-links.js" => (include_bytes!("../assets/addon-web-links.js"), "text/javascript"),
        "/vault.js" => (include_bytes!("../assets/vault.js"), "text/javascript"),
        "/vault.css" => (include_bytes!("../assets/vault.css"), "text/css"),
        "/addon-search.js" => (include_bytes!("../assets/addon-search.js"), "text/javascript"),
        "/search.js" => (include_bytes!("../assets/search.js"), "text/javascript"),
        "/search.css" => (include_bytes!("../assets/search.css"), "text/css"),
        "/layout.js" => (include_bytes!("../assets/layout.js"), "text/javascript"),
        "/help.js" => (include_bytes!("../assets/help.js"), "text/javascript"),
        "/help.css" => (include_bytes!("../assets/help.css"), "text/css"),
        "/notice.css" => (include_bytes!("../assets/notice.css"), "text/css"),
        "/claude.css" => (include_bytes!("../assets/claude.css"), "text/css"),
        "/git.js" => (include_bytes!("../assets/git.js"), "text/javascript"),
        "/side.js" => (include_bytes!("../assets/side.js"), "text/javascript"),
        "/git.css" => (include_bytes!("../assets/git.css"), "text/css"),
        _ => return None,
    })
}

/// Abre una URL http(s) en el navegador por defecto (`open`, sin shell). El hijo se
/// recoge en otro hilo para no dejar zombis.
pub fn open_external(url: &str) -> std::io::Result<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "URL no http(s)"));
    }
    let mut child = Command::new("/usr/bin/open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// pbcopy/pbpaste con locale UTF-8 explícito: lanzada desde Finder la app no
/// hereda LANG y pbpaste devolvería texto no-ASCII mal codificado.
fn clipboard_cmd(bin: &str) -> Command {
    let mut c = Command::new(bin);
    c.env("LANG", "en_US.UTF-8").env("LC_ALL", "en_US.UTF-8");
    c
}

fn pbcopy(data: &[u8]) -> bool {
    let Ok(mut p) = clipboard_cmd("pbcopy").stdin(std::process::Stdio::piped()).spawn() else { return false };
    let ok = p.stdin.take().is_some_and(|mut i| i.write_all(data).is_ok());
    p.wait().is_ok_and(|s| s.success()) && ok
}

/// Copia un secreto y, pasados 30 s, vacía el portapapeles solo si sigue conteniendo
/// exactamente ese valor. No se registra nada del contenido.
fn copy_secret(secret: Zeroizing<String>) -> bool {
    if !pbcopy(secret.as_bytes()) {
        return false;
    }
    std::thread::spawn(move || {
        std::thread::sleep(CLIPBOARD_CLEAR);
        if let Ok(out) = clipboard_cmd("pbpaste").output() {
            let current = Zeroizing::new(out.stdout);
            if current.as_slice() == secret.as_bytes() {
                pbcopy(b"");
            }
        }
    });
    true
}

/// Nombre del equipo (gethostname vía /bin/hostname), el mismo que mish pone en OSC 7.
/// app.js lo usa para ignorar OSC 7 de equipos remotos.
fn local_hostname() -> String {
    Command::new("/bin/hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// `tabs.json` (contrato común): `~/.local/share/mish/tabs.json`, 0600, escritura atómica.
fn tabs_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    Path::new(&home).join(".local/share/mish/tabs.json")
}

#[derive(Debug, PartialEq, Eq)]
enum LayoutError {
    TooLarge,
    BadJson,
    /// No es un objeto con `"v": 1`.
    BadVersion,
}

/// Valida un `tabs.json`: menos de 256 KB, JSON válido y objeto con `v == 1`. Nunca se
/// interpreta nada más aquí: el contenido lo reconcilia app.js.
fn validate_layout(text: &str) -> Result<serde_json::Value, LayoutError> {
    if text.len() >= LAYOUT_MAX {
        return Err(LayoutError::TooLarge);
    }
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| LayoutError::BadJson)?;
    if v.get("v").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err(LayoutError::BadVersion);
    }
    Ok(v)
}

/// `save_layout{json}`: acepta el texto JSON o el objeto ya parseado.
fn layout_from_msg(json: &serde_json::Value) -> Result<String, LayoutError> {
    let text = match json {
        serde_json::Value::String(s) => s.clone(),
        v @ serde_json::Value::Object(_) => v.to_string(),
        _ => return Err(LayoutError::BadVersion),
    };
    validate_layout(&text)?;
    Ok(text)
}

/// Lee y valida `tabs.json`; `None` si no existe o no es válido.
fn load_layout(path: &Path) -> Option<serde_json::Value> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() >= LAYOUT_MAX as u64 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    match validate_layout(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            eprintln!("mish-app: {} ignorado ({e:?})", path.display());
            None
        }
    }
}

/// Decide el backend al arrancar. Devuelve también el aviso a mostrar (si lo hay).
fn start_backend(proxy: &EventLoopProxy<UserEvent>) -> (Box<dyn Backend>, Option<serde_json::Value>) {
    let local = || Box::new(LocalBackend::new(proxy.clone())) as Box<dyn Backend>;
    if backend::daemon_disabled(std::env::var_os("MISH_NO_DAEMON").as_deref()) {
        return (local(), None);
    }
    match daemon_client::connect(proxy.clone()) {
        Ok((b, w)) => {
            let notice = (daemon_client::compat(&w, mish_proto::PROTO, daemon_client::BUILD)
                == daemon_client::Compat::BuildDiffers)
                .then(|| {
                    serde_json::json!({
                        "kind": "build",
                        "msg": "El daemon de sesiones es de otra compilación de Mish. Funciona, pero conviene reiniciarlo cuando no tengas trabajo abierto (se cerrarán las sesiones).",
                        "daemon_build": w.build,
                        "app_build": daemon_client::BUILD,
                        "actions": [{"id": "restart", "label": "Reiniciar sesiones"}],
                    })
                });
            (Box::new(b), notice)
        }
        Err(daemon_client::ConnectError::ProtoMismatch(w)) => (
            local(),
            Some(serde_json::json!({
                "kind": "proto",
                "msg": "El daemon de sesiones usa otra versión del protocolo. Mish funciona en modo local: estas pestañas no sobrevivirán al cierre. Reiniciar las sesiones cierra las del daemon antiguo.",
                "daemon_proto": w.proto,
                "app_proto": mish_proto::PROTO,
                "daemon_build": w.build,
                "app_build": daemon_client::BUILD,
                "actions": [
                    {"id": "restart", "label": "Reiniciar sesiones"},
                    {"id": "local", "label": "Seguir en local"},
                ],
            })),
        ),
        Err(daemon_client::ConnectError::Unavailable(e)) => {
            eprintln!("mish-app: daemon no disponible ({e}); modo local");
            (
                local(),
                Some(serde_json::json!({
                    "kind": "local",
                    "msg": format!("No se pudo usar el daemon de sesiones ({e}). Mish funciona en modo local: las pestañas no sobrevivirán al cierre."),
                })),
            )
        }
    }
}

/// Salida pendiente: se espera `save_layout` (o el plazo) antes de cerrar.
struct Quitting {
    deadline: Instant,
    /// ⌘⌥Q: matar también las sesiones del daemon.
    kill: bool,
    /// «Reiniciar Mish» tras actualizar: bundle a abrir de nuevo al salir.
    relaunch: Option<PathBuf>,
}

/// Primer paso de la salida: bloquea la caja fuerte y pide a app.js que guarde el layout.
/// Idempotente: ⌘Q puede llegar a la vez por el menú nativo y por IPC desde app.js.
fn begin_quit(
    webview: &wry::WebView,
    vault: &mut vault::Vault,
    quitting: &mut Option<Quitting>,
    kill: bool,
) {
    vault.lock();
    if let Some(q) = quitting {
        q.kill |= kill;
        return;
    }
    let _ = webview.evaluate_script("window.mishBeforeQuit&&window.mishBeforeQuit()");
    *quitting = Some(Quitting { deadline: Instant::now() + QUIT_GRACE, kill, relaunch: None });
}

/// Salida ordenada: bloquea la caja fuerte, desconecta del daemon (las sesiones siguen
/// vivas; con `kill`, las mata) o, en modo local, mata las sesiones; y sale del event loop.
/// Con `relaunch`, deja lanzado (desacoplado) el `open -n` del bundle para dentro de 1 s.
fn quit(vault: &mut vault::Vault, backend: &mut dyn Backend, q: Quitting, control_flow: &mut ControlFlow) {
    vault.lock();
    backend.shutdown(q.kill);
    if let Some(b) = q.relaunch
        && let Err(e) = updates::spawn_relaunch(&b)
    {
        eprintln!("mish-app: no se pudo programar el reinicio: {e}");
    }
    *control_flow = ControlFlow::Exit;
}

/// Envío de pasos del hilo de actualización al event loop.
fn upd_sender(proxy: &EventLoopProxy<UserEvent>) -> impl Fn(updates::Ev) + Send + 'static {
    let p = proxy.clone();
    move |e| {
        let _ = p.send_event(UserEvent::Update(e));
    }
}

/// «Instalar comando mish en el terminal…»: aviso con el resultado.
fn install_cli_notice() -> serde_json::Value {
    let Some(bundle) = mish_updater::current_bundle() else {
        return updates::notice_install_cli(Err(updates::NOT_INSTALLED.into()), true);
    };
    let Some(home) = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from) else {
        return updates::notice_install_cli(Err("no se conoce la carpeta personal (HOME)".into()), true);
    };
    let res = updates::install_cli(&bundle, &home);
    let in_path = updates::local_bin_in_path(&home, std::env::var("PATH").ok().as_deref());
    updates::notice_install_cli(res, in_path)
}

/// Aviso al webview (`mishNotice`); antes del arranque se encola.
fn notice(webview: &wry::WebView, booted: bool, pending: &mut Vec<serde_json::Value>, n: serde_json::Value) {
    if booted {
        let _ = webview.evaluate_script(&js::call("mishNotice", &n));
    } else {
        pending.push(n);
    }
}

fn out_script(id: u32, data: &[u8]) -> String {
    format!("mishOut({id},'{}')", B64.encode(data))
}

#[derive(Serialize)]
struct Boot<'a> {
    layout: Option<serde_json::Value>,
    sessions: &'a [SessionInfo],
    hostname: &'a str,
    mode: &'static str,
}

/// Notificación de macOS. Título y mensaje van como argv de `on run argv`: nunca se
/// interpolan en el AppleScript (sin inyección posible).
fn notify_cmd(title: &str, msg: &str) -> Command {
    let clean = |s: &str, max: usize| {
        let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        mish_proto::clamp_chars(s.trim(), max)
    };
    let mut cmd = Command::new("/usr/bin/osascript");
    cmd.args([
        "-e",
        "on run argv",
        "-e",
        "display notification (item 2 of argv) with title (item 1 of argv) sound name \"Glass\"",
        "-e",
        "end run",
    ])
    .arg(clean(title, 80))
    .arg(clean(msg, 200))
    .stdin(std::process::Stdio::null())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null());
    cmd
}

/// Badge del Dock: número de sesiones con Claude esperando (`None` si ninguna).
fn claude_badge(states: &HashMap<u32, String>) -> Option<String> {
    let n = states.values().filter(|s| s.as_str() == "waiting").count();
    (n > 0).then(|| n.to_string())
}

/// Actualiza el mapa id→estado (`none` lo quita) y el badge si cambió.
fn set_claude(
    window: &Window,
    states: &mut HashMap<u32, String>,
    badge: &mut Option<String>,
    id: u32,
    state: Option<&str>,
) {
    match state {
        Some(s) if s != "none" => {
            states.insert(id, s.to_string());
        }
        _ => {
            states.remove(&id);
        }
    }
    sync_badge(window, states, badge);
}

fn sync_badge(window: &Window, states: &HashMap<u32, String>, badge: &mut Option<String>) {
    let b = claude_badge(states);
    if b != *badge {
        window.set_badge_label(b.clone());
        *badge = b;
    }
}

fn toggle_fullscreen(window: &Window) {
    if window.fullscreen().is_some() {
        window.set_fullscreen(None);
    } else {
        window.set_fullscreen(Some(tao::window::Fullscreen::Borderless(None)));
    }
}

fn main() -> wry::Result<()> {
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Conexión al daemon antes de crear la ventana: si ya corre es inmediato; si hay que
    // arrancarlo, como mucho ~2 s.
    let (mut backend, first_notice) = start_backend(&proxy);

    let window = WindowBuilder::new()
        .with_title("mish")
        .with_inner_size(LogicalSize::new(1000.0, 640.0))
        .with_min_inner_size(LogicalSize::new(420.0, 260.0))
        .with_resizable(true)
        .with_maximizable(true)
        .with_titlebar_transparent(true)
        .with_title_hidden(true)
        .with_fullsize_content_view(true)
        .build(&event_loop)
        .expect("no se pudo crear la ventana");

    let ipc_proxy = proxy.clone();
    let page_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_custom_protocol("mish".into(), move |_id, req| {
            let path = req.uri().path().to_string();
            match asset(&path) {
                Some((body, mime)) => Response::builder()
                    .header(CONTENT_TYPE, mime)
                    .body(Cow::Borrowed(body))
                    .unwrap(),
                None => Response::builder().status(404).body(Cow::Borrowed(&b"404"[..])).unwrap(),
            }
        })
        .with_url("mish://localhost/")
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(UserEvent::Ipc(req.body().clone()));
        })
        .with_on_page_load_handler(move |ev, _url| {
            let _ = page_proxy.send_event(UserEvent::Page(ev));
        })
        .with_navigation_handler(|url| url.starts_with("mish://"))
        .with_background_color((12, 16, 26, 255))
        .with_accept_first_mouse(true)
        .with_devtools(cfg!(debug_assertions))
        .build(&window)?;

    let hostname = local_hostname();
    let layout_path = tabs_path();
    let mut vault = vault::Vault::default_location();
    // Arranque: al terminar de cargar la página se pide la lista de sesiones; con la
    // respuesta se llama a mishBoot. Los avisos esperan a mishBoot.
    let mut booted = false;
    let mut awaiting_list = false;
    let mut notices: Vec<serde_json::Value> = first_notice.into_iter().collect();
    let mut quitting: Option<Quitting> = None;
    // Tras quit(): tao sigue entregando eventos hasta cerrar el loop; se ignoran.
    let mut exiting = false;
    // La barra de menús se instala en StartCause::Init (NSApp ya lanzado) y vive con el loop.
    let mut _menu_bar: Option<muda::Menu> = None;
    let mut updater = updates::Updater::default();
    // Estado de Claude por sesión (solo los distintos de `none`) y badge actual del Dock.
    let mut claude: HashMap<u32, String> = HashMap::new();
    let mut badge: Option<String> = None;
    // cwd por sesión según el daemon (OSC 7 local, ya filtrado por host): es el único
    // directorio en el que confía el panel git. En modo local queda vacío.
    let mut cwds: HashMap<u32, String> = HashMap::new();
    // PATH de login (zsh -il) en segundo plano: la primera petición git no lo espera.
    std::thread::spawn(|| {
        envpath::login_path();
    });
    let menu_proxy = proxy.clone();
    muda::MenuEvent::set_event_handler(Some(std::sync::Arc::new(move |e: muda::MenuEvent| {
        let _ = menu_proxy.send_event(UserEvent::Menu(e.id));
    })));

    event_loop.run(move |event, _, control_flow| {
        if exiting {
            return;
        }
        *control_flow = ControlFlow::Wait;
        // Bloqueo automático: se comprueba en cada evento y, mientras está desbloqueada,
        // el loop se despierta en el plazo (WaitUntil, al final).
        if vault.expire_if_idle() {
            let _ = webview.evaluate_script(&vault.status_script(None));
        }
        let mut begin_boot = false;
        match event {
            Event::NewEvents(StartCause::Init) => match menu::install() {
                Ok(m) => _menu_bar = Some(m),
                Err(e) => eprintln!("mish-app: no se pudo crear la barra de menús: {e}"),
            },
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                begin_quit(&webview, &mut vault, &mut quitting, false);
            }
            Event::UserEvent(UserEvent::Menu(id)) => match menu::action(&id) {
                Some(menu::Action::Quit) => begin_quit(&webview, &mut vault, &mut quitting, false),
                Some(menu::Action::QuitAll) => begin_quit(&webview, &mut vault, &mut quitting, true),
                Some(menu::Action::Fullscreen) => toggle_fullscreen(&window),
                Some(menu::Action::Shortcuts) => {
                    let _ = webview.evaluate_script(&js::call("mishHelp", &serde_json::json!({"open": true})));
                }
                Some(menu::Action::CheckUpdates) => {
                    if let Some(n) = updater.check(upd_sender(&proxy)) {
                        notice(&webview, booted, &mut notices, n);
                    }
                }
                Some(menu::Action::InstallCli) => notice(&webview, booted, &mut notices, install_cli_notice()),
                None => {}
            },
            Event::UserEvent(UserEvent::Update(ev)) => {
                if let Some(n) = updater.on_event(ev) {
                    notice(&webview, booted, &mut notices, n);
                }
            }
            Event::UserEvent(UserEvent::Page(PageLoadEvent::Started)) => {
                booted = false;
                awaiting_list = false;
            }
            Event::UserEvent(UserEvent::Page(PageLoadEvent::Finished)) => begin_boot = true,
            Event::UserEvent(UserEvent::Sessions(list)) => {
                if awaiting_list {
                    awaiting_list = false;
                    booted = true;
                    // Las sessions de mishBoot ya llevan `claude`; el badge se rehace con ellas.
                    cwds = list.iter().filter_map(|s| s.cwd.clone().map(|c| (s.id, c))).collect();
                    claude = list
                        .iter()
                        .filter_map(|s| s.claude.clone().filter(|c| c != "none").map(|c| (s.id, c)))
                        .collect();
                    sync_badge(&window, &claude, &mut badge);
                    // Un layout de otra ventana (solo lectura) no se restaura aquí.
                    let layout = (backend.mode() != Mode::ReadOnly).then(|| load_layout(&layout_path)).flatten();
                    let _ = webview.evaluate_script(&js::call("mishHost", &hostname));
                    let boot = Boot { layout, sessions: &list, hostname: &hostname, mode: backend.mode().as_str() };
                    let _ = webview.evaluate_script(&js::call("mishBoot", &boot));
                    for n in notices.drain(..) {
                        let _ = webview.evaluate_script(&js::call("mishNotice", &n));
                    }
                }
            }
            Event::UserEvent(UserEvent::VaultDone(done)) => {
                let err = vault.finish(done).err().map(|e| e.to_string());
                let _ = webview.evaluate_script(&vault.status_script(err.as_deref()));
            }
            Event::UserEvent(UserEvent::Output(id, data) | UserEvent::Replay(id, data)) => {
                let _ = webview.evaluate_script(&out_script(id, &data));
            }
            Event::UserEvent(UserEvent::ReplayEnd(id)) => {
                let _ = webview.evaluate_script(&js::call("mishReplayEnd", &id));
            }
            Event::UserEvent(UserEvent::Meta { id, title, cwd, tint }) => {
                if let Some(c) = &cwd {
                    cwds.insert(id, c.clone());
                }
                let m = serde_json::json!({"id": id, "title": title, "cwd": cwd, "tint": tint});
                let _ = webview.evaluate_script(&js::call("mishMeta", &m));
            }
            Event::UserEvent(UserEvent::Claude { id, state, detail }) => {
                set_claude(&window, &mut claude, &mut badge, id, Some(&state));
                let m = serde_json::json!({"id": id, "state": state, "detail": detail});
                let _ = webview.evaluate_script(&js::call("mishClaude", &m));
            }
            Event::UserEvent(UserEvent::Git(reply)) => {
                let _ = webview.evaluate_script(&js::call("mishGit", &reply));
            }
            Event::UserEvent(UserEvent::Exit(id)) => {
                cwds.remove(&id);
                set_claude(&window, &mut claude, &mut badge, id, None);
                backend.exited(id);
                let _ = webview.evaluate_script(&format!("mishExit({id})"));
            }
            Event::UserEvent(UserEvent::DaemonError { id, msg }) => match id {
                Some(id) => {
                    let text = format!("\r\nmish-daemon: {msg}\r\n");
                    let _ = webview.evaluate_script(&out_script(id, text.as_bytes()));
                }
                None => eprintln!("mish-app: error del daemon: {msg}"),
            },
            Event::UserEvent(UserEvent::TakenOver) => {
                // Solo lectura: no se manda nada más al daemon ni se guarda tabs.json.
                backend.shutdown(false);
                claude.clear();
                cwds.clear();
                sync_badge(&window, &claude, &mut badge);
                backend = Box::new(backend::ReadOnlyBackend);
                let n = serde_json::json!({
                    "kind": "taken_over",
                    "msg": "Otra ventana de Mish ha tomado el control de las sesiones. Esta queda en solo lectura; ciérrala con ⌘Q (las sesiones siguen vivas).",
                    "readonly": true,
                    "actions": [{"id": "reattach", "label": "Recuperar el control"}],
                });
                notice(&webview, booted, &mut notices, n);
            }
            Event::UserEvent(UserEvent::DaemonLost) => {
                if backend.mode() == Mode::Daemon {
                    backend.shutdown(false);
                    backend = Box::new(LocalBackend::new(proxy.clone()));
                    claude.clear();
                    cwds.clear();
                    sync_badge(&window, &claude, &mut badge);
                    let n = serde_json::json!({
                        "kind": "lost",
                        "msg": "Se perdió la conexión con el daemon de sesiones. Las pestañas abiertas ya no responden; las nuevas funcionan en modo local.",
                        "actions": [{"id": "reattach", "label": "Reconectar"}],
                    });
                    notice(&webview, booted, &mut notices, n);
                }
            }
            Event::UserEvent(UserEvent::Ipc(mut body)) => 'ipc: {
                let parsed = serde_json::from_str::<Msg>(&body);
                // El cuerpo crudo puede llevar la contraseña maestra o un secreto.
                body.zeroize();
                let Ok(msg) = parsed else { break 'ipc };
                match msg {
                    Msg::New { id, cols, rows, cwd, argv } => {
                        // La página ya está cargada (nos habla por IPC). Se manda antes de lanzar
                        // el PTY para que llegue antes que cualquier OSC 7 de esta sesión.
                        let _ = webview.evaluate_script(&js::call("mishHost", &hostname));
                        if let Err(e) = backend.new_session(id, cols, rows, cwd.as_deref(), argv.as_deref()) {
                            let text = format!("\r\nmish-app: no se pudo arrancar mish: {e}\r\n");
                            let _ = webview.evaluate_script(&out_script(id, text.as_bytes()));
                        }
                    }
                    Msg::Attach { id, cols, rows } => {
                        if let Err(e) = backend.attach(id, cols, rows) {
                            eprintln!("mish-app: attach {id}: {e}");
                        }
                    }
                    Msg::Input { id, d } => {
                        backend.input(id, d.as_bytes());
                    }
                    Msg::Resize { id, cols, rows } => backend.resize(id, cols, rows),
                    Msg::Close { id } => backend.close(id),
                    Msg::KillAll => backend.kill_all(),
                    Msg::SaveLayout { json } => {
                        // En solo lectura el layout es el de la otra ventana: no se pisa.
                        if backend.mode() != Mode::ReadOnly {
                            match layout_from_msg(&json) {
                                Ok(text) => {
                                    if let Err(e) = vault::write_atomic(&layout_path, text.as_bytes()) {
                                        eprintln!("mish-app: no se pudo guardar {}: {e}", layout_path.display());
                                    }
                                }
                                Err(e) => eprintln!("mish-app: save_layout rechazado ({e:?})"),
                            }
                        }
                        if let Some(q) = quitting.take() {
                            quit(&mut vault, backend.as_mut(), q, control_flow);
                            exiting = true;
                        }
                    }
                    Msg::Ready => begin_boot = true,
                    Msg::NoticeAction { id } if id == "restart" => {
                        // Reinicio explícito: cierra las sesiones (locales y del daemon), relanza
                        // el daemon y recarga la página, que restaura el layout guardado.
                        backend.shutdown(true);
                        if let Err(e) = daemon_client::shutdown_daemon() {
                            eprintln!("mish-app: no se pudo apagar el daemon: {e}");
                        }
                        let (b, n) = start_backend(&proxy);
                        backend = b;
                        cwds.clear();
                        notices.extend(n);
                        booted = false;
                        awaiting_list = false;
                        let _ = webview.reload();
                    }
                    Msg::NoticeAction { id } if id == "reattach" => {
                        // Volver a conectar sin recargar: app.js recibe un mishBoot tardío y
                        // engancha las sesiones vivas (la otra GUI, si la hay, pasa a taken_over).
                        backend.shutdown(false);
                        let (b, n) = start_backend(&proxy);
                        backend = b;
                        if let Some(n) = n {
                            notice(&webview, booted, &mut notices, n);
                        }
                        booted = false;
                        awaiting_list = false;
                        begin_boot = true;
                    }
                    Msg::NoticeAction { id } if id == "relaunch" => {
                        // Tras actualizar: mismo camino que ⌘Q (bloquea la caja fuerte, guarda el
                        // layout, con daemon desconecta sin matar sesiones) y al salir reabre el bundle.
                        match updater.bundle().or_else(mish_updater::current_bundle) {
                            Some(b) => {
                                begin_quit(&webview, &mut vault, &mut quitting, false);
                                if let Some(q) = quitting.as_mut() {
                                    q.relaunch = Some(b);
                                }
                            }
                            None => notice(&webview, booted, &mut notices, updates::notice_error(updates::NOT_INSTALLED, false)),
                        }
                    }
                    Msg::NoticeAction { id } => {
                        if let Some(Some(n)) = updater.action(&id, upd_sender(&proxy)) {
                            notice(&webview, booted, &mut notices, n);
                        }
                    }
                    Msg::UpdateCheck {} => {
                        if let Some(n) = updater.check(upd_sender(&proxy)) {
                            notice(&webview, booted, &mut notices, n);
                        }
                    }
                    Msg::Copy { text } => {
                        if let Ok(mut p) = clipboard_cmd("pbcopy").stdin(std::process::Stdio::piped()).spawn() {
                            if let Some(mut i) = p.stdin.take() {
                                let _ = i.write_all(text.as_bytes());
                            }
                            let _ = p.wait();
                        }
                    }
                    Msg::Paste { id } => {
                        if let Ok(out) = clipboard_cmd("pbpaste").output() {
                            let js = format!("mishPaste({id},'{}')", B64.encode(&out.stdout));
                            let _ = webview.evaluate_script(&js);
                        }
                    }
                    Msg::Open { url } => {
                        let _ = open_external(&url);
                    }
                    Msg::Git(r) => {
                        let trusted = cwds.get(&r.sid).cloned();
                        let p = proxy.clone();
                        gitpanel::spawn(r, trusted, move |reply| {
                            let _ = p.send_event(UserEvent::Git(reply));
                        });
                    }
                    Msg::Notify { title, msg } => {
                        if let Ok(mut child) = notify_cmd(&title, &msg).spawn() {
                            // Se recoge fuera del event loop para no dejar zombis.
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            });
                        }
                    }
                    Msg::Drag => {
                        let _ = window.drag_window();
                    }
                    Msg::Zoom => {
                        window.set_maximized(!window.is_maximized());
                    }
                    Msg::Fullscreen => toggle_fullscreen(&window),
                    // mishBeforeQuit() termina mandando "quit" (tras su save_layout): si ya se
                    // estaba saliendo, no hace falta agotar el margen.
                    Msg::Quit => match quitting.take() {
                        Some(q) => {
                            quit(&mut vault, backend.as_mut(), q, control_flow);
                            exiting = true;
                        }
                        None => begin_quit(&webview, &mut vault, &mut quitting, false),
                    },
                    Msg::QuitAll => begin_quit(&webview, &mut vault, &mut quitting, true),
                    vault_msg => {
                        // Cada operación responde con window.mishVault({state, entries, error?}).
                        // Los errores son mensajes genéricos de VaultError: nunca llevan secretos.
                        let res: Result<bool, String> = match vault_msg {
                            Msg::VaultStatus => Ok(true),
                            Msg::VaultLock => {
                                vault.lock();
                                Ok(true)
                            }
                            Msg::VaultCreate { pw } => {
                                let p = proxy.clone();
                                vault
                                    .begin_create(pw, move |d| {
                                        let _ = p.send_event(UserEvent::VaultDone(d));
                                    })
                                    .map(|started| !started)
                                    .map_err(|e| e.to_string())
                            }
                            Msg::VaultUnlock { pw } => {
                                let p = proxy.clone();
                                vault
                                    .begin_unlock(pw, move |d| {
                                        let _ = p.send_event(UserEvent::VaultDone(d));
                                    })
                                    .map(|started| !started)
                                    .map_err(|e| e.to_string())
                            }
                            Msg::VaultAdd { name, user, secret } => {
                                vault.add(&name, &user, secret).map(|_| true).map_err(|e| e.to_string())
                            }
                            Msg::VaultDelete { name } => {
                                vault.delete(&name).map(|_| true).map_err(|e| e.to_string())
                            }
                            // Mismo camino que Msg::Input (backend.input), sin salto de línea.
                            Msg::VaultType { name, id } => match vault.secret(&name) {
                                Ok(s) => {
                                    if backend.input(id, s.as_bytes()) {
                                        Ok(true)
                                    } else {
                                        Err("No se pudo escribir en la pestaña".into())
                                    }
                                }
                                Err(e) => Err(e.to_string()),
                            },
                            Msg::VaultCopy { name } => match vault.secret(&name) {
                                Ok(s) => {
                                    if copy_secret(s) { Ok(true) } else { Err("No se pudo copiar al portapapeles".into()) }
                                }
                                Err(e) => Err(e.to_string()),
                            },
                            _ => Ok(false),
                        };
                        // Ok(false): operación lanzada en segundo plano; responde VaultDone.
                        match res {
                            Ok(true) => {
                                let _ = webview.evaluate_script(&vault.status_script(None));
                            }
                            Ok(false) => {}
                            Err(e) => {
                                let _ = webview.evaluate_script(&vault.status_script(Some(&e)));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
        if begin_boot && !booted && !awaiting_list && quitting.is_none() {
            awaiting_list = true;
            backend.list();
        }
        if exiting {
            return;
        }
        // Plazo de salida vencido sin save_layout: salir igualmente.
        if quitting.as_ref().is_some_and(|q| Instant::now() >= q.deadline) {
            let q = quitting.take().unwrap();
            quit(&mut vault, backend.as_mut(), q, control_flow);
            exiting = true;
        }
        // Despertar el loop en el plazo del bloqueo automático o de la salida (sin pisar un Exit).
        if *control_flow == ControlFlow::Wait {
            let deadline = [vault.deadline(), quitting.as_ref().map(|q| q.deadline)].into_iter().flatten().min();
            if let Some(d) = deadline {
                *control_flow = ControlFlow::WaitUntil(d);
            }
        }
    });
}
