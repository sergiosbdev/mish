//! Integración con Terminal.app: color/título de la pestaña propia y notificaciones.

use crate::theme::Rgb;
use std::process::{Command, Stdio};

pub fn is_apple_terminal() -> bool {
    std::env::var("TERM_PROGRAM").map(|v| v == "Apple_Terminal").unwrap_or(false)
}

pub fn my_tty() -> Option<String> {
    let p = unsafe { libc::ttyname(0) };
    if p.is_null() {
        return None;
    }
    Some(unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().to_string())
}

fn osascript(script: &str) -> Option<String> {
    let out = Command::new("osascript").arg("-e").arg(script).stderr(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Estado previo de la pestaña, para restaurarlo.
pub struct TabState {
    tty: String,
    bg: String,
    title: String,
}

/// Cambia fondo y título de la pestaña de este TTY. Devuelve el estado anterior.
pub fn tint_tab(bg: Rgb, title: &str) -> Option<TabState> {
    if !is_apple_terminal() {
        return None;
    }
    let tty = my_tty()?;
    let (r, g, b) = (bg.0 as u32 * 257, bg.1 as u32 * 257, bg.2 as u32 * 257);
    let script = format!(
        r#"tell application "Terminal"
  repeat with w in windows
    try
      repeat with t in tabs of w
        if tty of t is "{tty}" then
          set oldbg to background color of t
          set oldtitle to custom title of t
          set background color of t to {{{r}, {g}, {b}}}
          set custom title of t to "{title}"
          set title displays custom title of t to true
          return ((item 1 of oldbg) as text) & "," & ((item 2 of oldbg) as text) & "," & ((item 3 of oldbg) as text) & "|" & oldtitle
        end if
      end repeat
    end try
  end repeat
end tell"#,
        tty = esc(&tty),
        title = esc(title)
    );
    let out = osascript(&script)?;
    let (bg_s, title_s) = out.split_once('|')?;
    Some(TabState { tty, bg: bg_s.to_string(), title: title_s.to_string() })
}

pub fn restore_tab(st: &TabState) {
    let script = format!(
        r#"tell application "Terminal"
  repeat with w in windows
    try
      repeat with t in tabs of w
        if tty of t is "{tty}" then
          set background color of t to {{{bg}}}
          set custom title of t to "{title}"
          return
        end if
      end repeat
    end try
  end repeat
end tell"#,
        tty = esc(&st.tty),
        bg = st.bg,
        title = esc(&st.title)
    );
    osascript(&script);
}

/// ¿Está el usuario mirando esta pestaña ahora mismo?
pub fn tab_focused() -> bool {
    if !is_apple_terminal() {
        return false;
    }
    let Some(tty) = my_tty() else { return false };
    let script = r#"tell application "Terminal" to return (frontmost as text) & "|" & (tty of selected tab of front window)"#;
    match osascript(script) {
        Some(s) => s == format!("true|{tty}"),
        None => false,
    }
}

pub fn notify(title: &str, msg: &str) {
    let script = format!(r#"display notification "{}" with title "{}" sound name "Glass""#, esc(msg), esc(title));
    let _ = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

/// ¿Corremos dentro de Mish.app?
pub fn is_mish_app() -> bool {
    std::env::var("TERM_PROGRAM").map(|v| v == "mish-app").unwrap_or(false)
}

fn osc(body: &str) {
    use std::io::Write;
    print!("\x1b]{body}\x07");
    let _ = std::io::stdout().flush();
}

/// Mish.app: teñir la pestaña (OSC 7701).
pub fn app_tint(bg: Rgb, title: &str) {
    osc(&format!("7701;bg=#{:02x}{:02x}{:02x};title={}", bg.0, bg.1, bg.2, title.replace([';', '\x07', '\x1b'], " ")));
}

pub fn app_untint() {
    osc("7701;reset");
}

/// Título de la pestaña (OSC 0, estándar).
pub fn set_title(t: &str) {
    osc(&format!("0;{}", t.replace(['\x07', '\x1b'], " ")));
}

/// Mish.app: notificación (OSC 7702); la app decide si mostrarla según el foco.
pub fn app_notify(title: &str, msg: &str) {
    let clean = |s: &str| s.replace([';', '\x07', '\x1b'], " ");
    osc(&format!("7702;{};{}", clean(title), clean(msg)));
}

/// Bytes que van tal cual en la URL de OSC 7: no reservados (RFC 3986) y `/`.
fn keep(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// Codificación %XX de todo lo que no sea no reservado o `/`.
pub fn percent_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        if keep(b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// Cuerpo de la secuencia OSC 7 (sin `ESC ]` ni BEL).
pub fn osc7_body(host: &str, path: &[u8]) -> String {
    format!("7;file://{}{}", percent_encode(host.as_bytes()), percent_encode(path))
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    let r = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
    if r != 0 {
        return String::new();
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

/// Informa del cwd al terminal (OSC 7: `ESC ]7;file://<host><ruta>BEL`).
pub fn report_cwd(path: &std::path::Path) {
    use std::os::unix::ffi::OsStrExt;
    osc(&osc7_body(&hostname(), path.as_os_str().as_bytes()));
}
