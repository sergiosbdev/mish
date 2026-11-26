//! `mish hook <evento>`: manejador de los hooks de Claude Code (ver `claude_hooks`).
//!
//! Claude Code lo ejecuta con el JSON del evento en stdin. Se despacha lo primero en `main()`
//! porque `mish` sin argumentos y con stdin que no es un TTY *ejecuta* stdin como script.
//!
//! Reglas duras:
//! - **Nada en stdout**: el stdout de un hook se inyecta en el contexto de Claude.
//! - **Exit 0 siempre**, también ante errores o pánicos (como mucho, un mensaje en stderr si
//!   `MISH_HOOK_DEBUG=1`).
//! - Sin `MISH_SESSION` (u32) no se conecta a nada: se descarta stdin y se sale.
//! - Del JSON solo se extraen `hook_event_name`, `notification_type`, `tool_name` y `message`
//!   (truncado y sin saltos de línea). Nunca `tool_input`, `prompt`, `transcript_path`, `cwd`…
//! - Conexión al daemon (`MISH_SOCK` o `mish_proto::socket_path()`) con un tope total de
//!   [`TIMEOUT`]: `hello{role:hook}` + `hook{…}` y se cierra sin esperar respuesta (el daemon no
//!   la exige: procesa los frames en orden y el cierre llega después).

use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mish_proto::{Ctl, Frame, PROTO, Role, encode_frame};
use serde::Deserialize;

/// Máximo de stdin que se lee (el JSON de PostToolUse puede ser grande).
pub const MAX_STDIN: usize = 8 * 1024 * 1024;
/// Tope total para conectar y escribir.
pub const TIMEOUT: Duration = Duration::from_millis(300);

pub use mish_proto::{
    MAX_HOOK_EVENT as MAX_EVENT, MAX_HOOK_MSG as MAX_MSG, MAX_HOOK_NTYPE as MAX_NTYPE, MAX_HOOK_TOOL as MAX_TOOL,
};

const BUILD: &str = match option_env!("MISH_BUILD") {
    Some(b) => b,
    None => "dev",
};

/// Lo único que sale de este proceso hacia el daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookMsg {
    pub event: String,
    pub ntype: Option<String>,
    pub tool: Option<String>,
    pub msg: Option<String>,
}

/// Campos que se leen del JSON; el resto (tool_input, prompt, transcript_path, cwd…) se ignora
/// sin guardarlo (serde lo salta).
#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    hook_event_name: Option<serde_json::Value>,
    #[serde(default)]
    notification_type: Option<serde_json::Value>,
    #[serde(default)]
    tool_name: Option<serde_json::Value>,
    #[serde(default)]
    message: Option<serde_json::Value>,
}

/// Punto de entrada. No vuelve nunca y sale siempre con 0.
pub fn run(event_arg: Option<&OsStr>) -> ! {
    let debug = std::env::var_os("MISH_HOOK_DEBUG").is_some_and(|v| v == "1");
    // Un pánico no debe imprimir nada (salvo en depuración) ni cambiar el código de salida.
    if !debug {
        std::panic::set_hook(Box::new(|_| {}));
    }
    let arg = event_arg.and_then(|a| a.to_str()).map(str::to_owned);
    let r = std::panic::catch_unwind(move || inner(arg.as_deref()));
    if debug {
        match r {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("mish hook: {e}"),
            Err(_) => eprintln!("mish hook: pánico"),
        }
    }
    let _ = io::stderr().flush();
    std::process::exit(0)
}

fn inner(arg: Option<&str>) -> Result<(), String> {
    let stdin_tty = unsafe { libc::isatty(0) } == 1;
    let session = std::env::var("MISH_SESSION").ok().and_then(|s| s.trim().parse::<u32>().ok());
    let Some(session) = session else {
        if !stdin_tty {
            let _ = read_limited(io::stdin().lock(), MAX_STDIN);
        }
        return Err("sin MISH_SESSION: no se hace nada".into());
    };
    if stdin_tty {
        return Err("stdin es un TTY: no hay JSON".into());
    }
    let (input, overflow) = read_limited(io::stdin().lock(), MAX_STDIN).map_err(|e| format!("stdin: {e}"))?;
    let msg = if overflow {
        // JSON truncado: solo se sabe el evento por el argumento.
        let event = clean_event(arg).ok_or("stdin > 8 MiB y evento inválido")?;
        HookMsg { event, ntype: None, tool: None, msg: None }
    } else {
        extract(&input, arg).ok_or("JSON inválido o sin evento")?
    };
    drop(input);
    send(session, msg)
}

/// Lee hasta `max` bytes. Devuelve (datos, se_pasó_del_límite).
fn read_limited<R: Read>(r: R, max: usize) -> io::Result<(Vec<u8>, bool)> {
    let mut buf = Vec::new();
    r.take(max as u64 + 1).read_to_end(&mut buf)?;
    let over = buf.len() > max;
    buf.truncate(max);
    Ok((buf, over))
}

/// Extrae del JSON de Claude Code lo único que se envía. `None` si el JSON no es válido o no hay
/// un evento válido (ni en el JSON ni en el argumento).
pub fn extract(json: &[u8], arg: Option<&str>) -> Option<HookMsg> {
    // Solo un objeto (serde aceptaría también un array posicional para el struct).
    if json.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return None;
    }
    let v: Input = serde_json::from_slice(json).ok()?;
    let s = |x: &Option<serde_json::Value>| x.as_ref().and_then(|v| v.as_str()).map(str::to_owned);
    let event = clean_event(s(&v.hook_event_name).as_deref()).or_else(|| clean_event(arg))?;
    Some(HookMsg {
        event,
        ntype: clean(s(&v.notification_type).as_deref(), MAX_NTYPE),
        tool: clean(s(&v.tool_name).as_deref(), MAX_TOOL),
        msg: clean(s(&v.message).as_deref(), MAX_MSG),
    })
}

/// Nombre de evento: ASCII alfanumérico, 1..=32 bytes.
fn clean_event(e: Option<&str>) -> Option<String> {
    let e = e?.trim();
    (!e.is_empty() && e.len() <= MAX_EVENT && e.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| e.to_owned())
}

/// Sustituye controles (incluidos saltos de línea y U+2028/U+2029) por espacios, colapsa
/// espacios y recorta a `max` caracteres (los límites de proto son en caracteres).
fn clean(s: Option<&str>, max: usize) -> Option<String> {
    let mut out = String::new();
    let mut n = 0usize;
    let mut space = false;
    for c in s?.chars() {
        let c = if c.is_control() || c == '\u{2028}' || c == '\u{2029}' || c.is_whitespace() { ' ' } else { c };
        if c == ' ' {
            if space || out.is_empty() {
                continue;
            }
            space = true;
        } else {
            space = false;
        }
        if n == max {
            break;
        }
        out.push(c);
        n += 1;
    }
    let out = out.trim_end().to_owned();
    (!out.is_empty()).then_some(out)
}

fn socket() -> PathBuf {
    std::env::var_os("MISH_SOCK").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(mish_proto::socket_path)
}

/// Conecta y escribe en un hilo; el hilo principal espera como mucho [`TIMEOUT`] (un `connect`
/// a un socket Unix con la cola llena puede bloquear).
fn send(session: u32, m: HookMsg) -> Result<(), String> {
    let start = Instant::now();
    let sock = socket();
    // Solo a un socket nuestro: no se filtra nada a un socket de otro usuario.
    let md = std::fs::metadata(&sock).map_err(|e| format!("{}: {e}", sock.display()))?;
    if !md.file_type().is_socket() || md.uid() != unsafe { libc::getuid() } {
        return Err(format!("{}: no es un socket propio", sock.display()));
    }
    let mut buf = encode_frame(&Frame::Ctl(Ctl::Hello { proto: PROTO, role: Role::Hook, build: BUILD.into() }))
        .map_err(|e| e.to_string())?;
    buf.extend(
        encode_frame(&Frame::Ctl(Ctl::Hook {
            session,
            event: m.event,
            ntype: m.ntype,
            tool: m.tool,
            msg: m.msg,
        }))
        .map_err(|e| e.to_string())?,
    );
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = (|| -> io::Result<()> {
            let mut s = UnixStream::connect(&sock)?;
            let left = TIMEOUT.saturating_sub(start.elapsed()).max(Duration::from_millis(1));
            s.set_write_timeout(Some(left))?;
            s.write_all(&buf)?;
            let _ = s.shutdown(std::net::Shutdown::Write);
            Ok(())
        })();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(TIMEOUT.saturating_sub(start.elapsed())) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(format!("socket: {e}")),
        Err(_) => Err("timeout".into()),
    }
}
