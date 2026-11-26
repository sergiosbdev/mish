//! PATH de la shell de login del usuario, para lanzar git/gh/code desde la app.
//!
//! Abierta desde Finder, la app hereda un PATH mínimo (`/usr/bin:/bin:…`) sin Homebrew ni
//! cargo. Se pregunta una vez a `zsh -il` (que carga `~/.zprofile` y `~/.zshrc`) y se
//! guarda; si tarda más de 3 s o falla, se usa un PATH razonable fijo.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::time::Duration;

const ZSH_TIMEOUT: Duration = Duration::from_secs(3);

/// Lo que `print -rn` puede devolver como mucho (rc ruidosos incluidos).
const MAX_OUT: u64 = 64 * 1024;

static LOGIN_PATH: OnceLock<String> = OnceLock::new();

/// PATH de login (calculado la primera vez; las siguientes es inmediato).
pub fn login_path() -> &'static str {
    LOGIN_PATH.get_or_init(compute)
}

/// Busca `name` en el PATH de login. Nombres con `/` se devuelven tal cual si existen.
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, login_path())
}

fn which_in(name: &str, path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let exec = |p: &Path| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    if name.is_empty() || name.contains('\0') {
        return None;
    }
    if name.contains('/') {
        let p = PathBuf::from(name);
        return (p.is_absolute() && exec(&p)).then_some(p);
    }
    path.split(':').filter(|d| d.starts_with('/')).map(|d| Path::new(d).join(name)).find(|p| exec(p))
}

fn fallback() -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut v = vec!["/opt/homebrew/bin".to_string(), "/usr/local/bin".to_string()];
    if home.starts_with('/') {
        v.push(format!("{home}/.cargo/bin"));
    }
    v.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(String::from));
    v.join(":")
}

fn compute() -> String {
    query_zsh().and_then(|out| clean(&out)).unwrap_or_else(fallback)
}

/// Lanza `zsh -ilc 'print -rn -- $PATH'` con plazo; mata el grupo entero si no termina.
fn query_zsh() -> Option<String> {
    let mut child = Command::new("/bin/zsh")
        .args(["-ilc", "print -rn -- $PATH"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = (&mut stdout).take(MAX_OUT).read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    let out = rx.recv_timeout(ZSH_TIMEOUT);
    if out.is_err() {
        // Plazo vencido: se mata el grupo entero (rc que se queda esperando o lanza hijos).
        // SAFETY: kill(2) sobre el grupo propio del hijo, que aún no se ha recogido.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
    }
    let _ = child.wait();
    String::from_utf8(out.ok()?).ok()
}

/// Valida la salida: última línea (por si el rc imprime algo antes), solo entradas
/// absolutas, sin duplicados. `None` si no queda nada usable.
fn clean(out: &str) -> Option<String> {
    let last = out.rsplit('\n').next().unwrap_or("").trim_end_matches('\r');
    let mut seen = std::collections::HashSet::new();
    let dirs: Vec<&str> = last
        .split(':')
        .filter(|d| d.starts_with('/') && !d.chars().any(char::is_control))
        .filter(|d| seen.insert(*d))
        .collect();
    (!dirs.is_empty()).then(|| dirs.join(":"))
}
