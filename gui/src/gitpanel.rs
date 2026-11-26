//! Panel git (⌘⇧G): estado, diff, PRs, issues, búsqueda y abrir en el editor.
//!
//! Cada petición (`{t:"git", req, op, sid, …}`) corre en su propio hilo y responde con
//! `window.mishGit({req, op, ok, data?, error?})`. Seguridad:
//! - El directorio sale de los `meta` del daemon (OSC 7 local) por `sid`; solo sin esa
//!   entrada (modo local) se acepta el `cwd` del JS, si es absoluto y es un directorio.
//! - git/gh/editor se lanzan con `Command::arg`, nunca con shell; `-e` y `--` delante de
//!   los datos; `fsmonitor` desactivado, `--no-ext-diff` y `--no-textconv`.
//! - Rutas relativas validadas dentro de la raíz canonicalizada (sin `..` ni symlinks que
//!   salgan del repo); URLs solo `https://github.com/…` o el host https del remoto origin.
//! - Plazo de 15 s por comando (se mata el grupo del hijo) y límites de tamaño.
//! - Nunca se registra contenido de ficheros.

use crate::envpath;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Plazo por comando.
const TIMEOUT: Duration = Duration::from_secs(15);
/// Peticiones simultáneas como mucho (status + prs + issues + diff caben de sobra).
const MAX_ACTIVE: usize = 6;
const DIFF_MAX_BYTES: usize = 1 << 20;
const DIFF_MAX_LINES: usize = 5000;
const STATUS_MAX_BYTES: usize = 8 << 20;
const GH_MAX_BYTES: usize = 4 << 20;
const GREP_MAX_HITS: usize = 500;
const GREP_MAX_Q: usize = 200;
/// Bytes que se guardan de una línea de `git grep` (las minificadas pueden ser enormes).
const GREP_LINE_CAP: usize = 4096;
const GREP_TEXT_CHARS: usize = 300;
const STDERR_MAX: u64 = 16 * 1024;
const PATH_MAX: usize = 4096;
const URL_MAX: usize = 2048;
const CONFIG_MAX: u64 = 1 << 20;

pub const NOT_REPO: &str = "No es un repositorio git";
const NO_EDITOR: &str = "Configura [editor] gui en config.toml";
const BAD_PATH: &str = "Ruta no válida";
const OUT_OF_REPO: &str = "Ruta fuera del repositorio";

/// Petición del webview. Todo es opcional salvo `req` (sin él no se puede responder).
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Req {
    pub req: u32,
    #[serde(deserialize_with = "nullable")]
    pub op: String,
    /// `null` (sin panel enfocado) se lee como 0, que nunca está en el mapa del daemon.
    #[serde(deserialize_with = "nullable")]
    pub sid: u32,
    pub cwd: Option<String>,
    pub path: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub staged: bool,
    #[serde(deserialize_with = "nullable")]
    pub untracked: bool,
    pub q: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub regex: bool,
    pub line: Option<u32>,
    pub url: Option<String>,
}

/// `null` → valor por defecto (git.js puede mandar `sid: null`).
fn nullable<'de, D: serde::Deserializer<'de>, T: Default + Deserialize<'de>>(d: D) -> Result<T, D::Error> {
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// Respuesta (`window.mishGit`).
#[derive(Debug, Serialize)]
pub struct Reply {
    pub req: u32,
    pub op: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Reply {
    fn new(req: u32, op: &str, res: Result<Value, String>) -> Reply {
        // El op vuelve tal cual solo si es un identificador corto (para que JS lo case).
        let op = if op.len() <= 16 && op.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') { op } else { "" };
        match res {
            Ok(d) => Reply { req, op: op.into(), ok: true, data: Some(d), error: None },
            Err(e) => Reply { req, op: op.into(), ok: false, data: None, error: Some(e) },
        }
    }
}

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// Plaza en el cupo de peticiones simultáneas; se libera al soltarla.
struct Slot;

impl Slot {
    fn acquire() -> Option<Slot> {
        if ACTIVE.fetch_add(1, Ordering::SeqCst) >= MAX_ACTIVE {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        ACTIVE.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Atiende la petición en un hilo propio. `trusted_cwd` es el cwd que el daemon conoce
/// para `req.sid` (si lo hay). `reply` se llama exactamente una vez.
pub fn spawn(req: Req, trusted_cwd: Option<String>, reply: impl FnOnce(Reply) + Send + 'static) {
    let Some(slot) = Slot::acquire() else {
        reply(Reply::new(req.req, &req.op, Err("Demasiadas operaciones git en curso; inténtalo de nuevo".into())));
        return;
    };
    let res = std::thread::Builder::new().name("mish-git".into()).spawn(move || {
        let _slot = slot;
        let res = handle(&req, trusted_cwd.as_deref());
        reply(Reply::new(req.req, &req.op, res));
    });
    if let Err(e) = res {
        eprintln!("mish-app: no se pudo lanzar el hilo git: {e}");
    }
}

fn handle(r: &Req, trusted: Option<&str>) -> Result<Value, String> {
    let root = || workdir(trusted, r.cwd.as_deref()).and_then(|d| repo_root(&d));
    match r.op.as_str() {
        "status" => status(&root()?),
        "diff" => diff(&root()?, r.path.as_deref().unwrap_or(""), r.staged, r.untracked),
        "prs" => prs(&root()?),
        "issues" => issues(&root()?),
        "grep" => grep(&root()?, r.q.as_deref().unwrap_or(""), r.regex),
        "open" => open_file(&root()?, r.path.as_deref().unwrap_or(""), r.line),
        "open_url" => {
            let url = r.url.as_deref().unwrap_or("");
            // El host del remoto solo se consulta si no es github.com (y si hay repo).
            let extra = if url_allowed(url, None) { None } else { root().ok().and_then(|r| origin_https_host(&r)) };
            if !url_allowed(url, extra.as_deref()) {
                return Err("URL no permitida".into());
            }
            crate::open_external(url).map_err(|e| format!("No se pudo abrir el navegador: {e}"))?;
            Ok(json!({}))
        }
        _ => Err("Operación git desconocida".into()),
    }
}

// ---------------------------------------------------------------------------------------
// Directorio y raíz del repositorio

/// cwd de trabajo: el del daemon si lo hay; si no, el del JS. Absoluto, existente y
/// directorio; se devuelve canonicalizado.
fn workdir(trusted: Option<&str>, js: Option<&str>) -> Result<PathBuf, String> {
    let raw = trusted.or(js).ok_or("No se conoce la carpeta de este panel")?;
    let p = Path::new(raw);
    if raw.contains('\0') || !p.is_absolute() {
        return Err("Directorio no válido".into());
    }
    let c = p.canonicalize().map_err(|_| "El directorio de la pestaña no existe".to_string())?;
    if !c.is_dir() {
        return Err("Directorio no válido".into());
    }
    Ok(c)
}

/// `git rev-parse --show-toplevel`, canonicalizado.
fn repo_root(cwd: &Path) -> Result<PathBuf, String> {
    let mut cmd = git(cwd);
    cmd.args(["rev-parse", "--show-toplevel"]);
    let (out, run) = collect(&mut cmd, PATH_MAX * 2)?;
    if !run.success() {
        if String::from_utf8_lossy(&run.stderr).contains("dubious ownership") {
            return Err("Repositorio de otro usuario (ver git config safe.directory)".into());
        }
        return Err(NOT_REPO.into());
    }
    let s = String::from_utf8(out).map_err(|_| NOT_REPO.to_string())?;
    let s = s.strip_suffix('\n').unwrap_or(&s);
    if s.is_empty() || !s.starts_with('/') {
        return Err(NOT_REPO.into());
    }
    Path::new(s).canonicalize().map_err(|_| NOT_REPO.to_string())
}

/// ¿Debe existir la ruta? Los ficheros borrados (diff) no existen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Exist {
    Required,
    MayBeMissing,
}

/// Valida una ruta relativa a `root` (canonicalizada): sin NUL, no absoluta, sin `..`;
/// resuelta (symlinks incluidos) tiene que quedar dentro de `root`. Si no existe y se
/// permite, se comprueba el ancestro existente más cercano. Devuelve la ruta absoluta.
fn validate_path(root: &Path, rel: &str, exist: Exist) -> Result<PathBuf, String> {
    if rel.is_empty() || rel.len() > PATH_MAX || rel.contains('\0') {
        return Err(BAD_PATH.into());
    }
    let p = Path::new(rel);
    let mut normal = false;
    for c in p.components() {
        match c {
            Component::Normal(_) => normal = true,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return Err(BAD_PATH.into()),
        }
    }
    if !normal {
        return Err(BAD_PATH.into());
    }
    let joined = root.join(p);
    match joined.canonicalize() {
        Ok(c) if c.starts_with(root) && c != root => Ok(c),
        Ok(_) => Err(OUT_OF_REPO.into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && exist == Exist::MayBeMissing => {
            let mut anc = joined.parent();
            while let Some(a) = anc {
                if let Ok(c) = a.canonicalize() {
                    return if c.starts_with(root) { Ok(joined) } else { Err(OUT_OF_REPO.into()) };
                }
                anc = a.parent();
            }
            Err(OUT_OF_REPO.into())
        }
        Err(_) => Err("El fichero no existe".into()),
    }
}

// ---------------------------------------------------------------------------------------
// Ejecución de comandos

/// Entorno común: PATH de login, sin prompts ni colores ni locks opcionales, y sin
/// variables de git heredadas que redirijan el repositorio.
fn base_env(c: &mut Command) {
    c.env("PATH", envpath::login_path())
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_SPINNER_DISABLED", "1")
        .env("NO_COLOR", "1")
        .env("LC_ALL", "en_US.UTF-8")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_EXTERNAL_DIFF");
}

/// `git -C dir --no-pager --literal-pathspecs -c core.fsmonitor=false -c core.quotepath=false`.
fn git(dir: &Path) -> Command {
    let bin = envpath::which("git").unwrap_or_else(|| PathBuf::from("/usr/bin/git"));
    let mut c = Command::new(bin);
    base_env(&mut c);
    c.arg("-C").arg(dir).args([
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotepath=false",
        "-c",
        "color.ui=false",
    ]);
    c
}

/// Resultado de [`run`].
#[derive(Debug)]
struct Run {
    status: Option<ExitStatus>,
    stderr: Vec<u8>,
    /// El `sink` pidió parar y se mató el proceso.
    stopped: bool,
}

impl Run {
    fn code(&self) -> Option<i32> {
        self.status.and_then(|s| s.code())
    }
    fn success(&self) -> bool {
        self.code() == Some(0)
    }
    /// Primera línea útil de stderr, para mostrar.
    fn err_line(&self, prefix: &str) -> String {
        let s = String::from_utf8_lossy(&self.stderr);
        let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("error desconocido");
        let line: String = line.chars().filter(|c| !c.is_control()).take(300).collect();
        format!("{prefix}{line}")
    }
}

fn kill_group(child: &std::process::Child) {
    // SAFETY: kill(2) sobre el grupo del hijo (process_group(0)), aún sin recoger.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

/// Lanza `cmd` (stdin vacío, en su propio grupo) y pasa su stdout a `sink` por trozos;
/// si `sink` devuelve `false` o vence el plazo, se mata el grupo entero.
fn run_with(cmd: &mut Command, timeout: Duration, mut sink: impl FnMut(&[u8]) -> bool) -> Result<Run, String> {
    let prog = Path::new(cmd.get_program()).file_name().unwrap_or(OsStr::new("?")).to_string_lossy().into_owned();
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("No se pudo ejecutar {prog}: {e}"))?;
    let deadline = Instant::now() + timeout;
    let mut stdout = child.stdout.take().ok_or("sin stdout")?;
    let mut stderr = child.stderr.take().ok_or("sin stderr")?;

    let (etx, erx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut stderr).take(STDERR_MAX).read_to_end(&mut b);
        // El resto se descarta para que el hijo no se bloquee escribiendo.
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        let _ = etx.send(b);
    });
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(8);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match stdout.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });

    let mut stopped = false;
    let mut timed_out = false;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(chunk) => {
                if !sink(&chunk) {
                    stopped = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                timed_out = true;
                break;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(rx);
    if stopped || timed_out {
        kill_group(&child);
    }
    // Tras cerrar stdout el hijo termina enseguida; si no, también hay plazo.
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Ok(None) => {
                timed_out = true;
                kill_group(&child);
                break child.wait().ok();
            }
            Err(_) => break None,
        }
    };
    let grace = deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(200));
    let stderr = erx.recv_timeout(grace).unwrap_or_default();
    if timed_out {
        return Err(format!("{prog} no respondió en {} s", timeout.as_secs()));
    }
    Ok(Run { status, stderr, stopped })
}

fn run(cmd: &mut Command, sink: impl FnMut(&[u8]) -> bool) -> Result<Run, String> {
    run_with(cmd, TIMEOUT, sink)
}

/// Recoge stdout entero hasta `max` bytes (si se pasa, para y `stopped`).
fn collect(cmd: &mut Command, max: usize) -> Result<(Vec<u8>, Run), String> {
    let mut out = Vec::new();
    let r = run(cmd, |c| {
        out.extend_from_slice(c);
        out.len() <= max
    })?;
    out.truncate(max);
    Ok((out, r))
}

// ---------------------------------------------------------------------------------------
// status

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orig: Option<String>,
    pub x: String,
    pub y: String,
    pub kind: &'static str,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Status {
    pub root: String,
    pub repo: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub detached: bool,
    /// Commit actual abreviado (`None` en un repo sin commits).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oid: Option<String>,
    pub files: Vec<FileEntry>,
    /// La salida superó el límite y la lista está incompleta.
    pub truncated: bool,
}

/// Parser de `git status --porcelain=v2 -z --branch`. Robusto ante rutas con espacios,
/// UTF-8 y registros desconocidos (se ignoran).
fn parse_status(data: &[u8]) -> Status {
    let mut st = Status::default();
    let mut recs = data.split(|&b| b == 0);
    let xy = |s: &str| {
        let mut c = s.chars();
        (c.next().unwrap_or('.').to_string(), c.next().unwrap_or('.').to_string())
    };
    let push_changed = |st: &mut Status, code: &str, path: &str, orig: Option<String>| {
        let (x, y) = xy(code);
        if x != "." {
            st.files.push(FileEntry { path: path.into(), orig: orig.clone(), x: x.clone(), y: y.clone(), kind: "staged" });
        }
        if y != "." {
            st.files.push(FileEntry { path: path.into(), orig, x, y, kind: "unstaged" });
        }
    };
    while let Some(rec) = recs.next() {
        if rec.is_empty() {
            continue;
        }
        let s = String::from_utf8_lossy(rec);
        if let Some(h) = s.strip_prefix("# ") {
            let (k, v) = h.split_once(' ').unwrap_or((h, ""));
            match k {
                "branch.oid" if v != "(initial)" => st.oid = Some(v.chars().take(7).collect()),
                "branch.head" => {
                    st.detached = v == "(detached)";
                    st.branch = v.into();
                }
                "branch.upstream" => st.upstream = Some(v.into()),
                "branch.ab" => {
                    for n in v.split(' ') {
                        if let Some(a) = n.strip_prefix('+') {
                            st.ahead = a.parse().unwrap_or(0);
                        } else if let Some(b) = n.strip_prefix('-') {
                            st.behind = b.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        match rec[0] {
            b'1' => {
                let f: Vec<&str> = s.splitn(9, ' ').collect();
                if f.len() == 9 {
                    push_changed(&mut st, f[1], f[8], None);
                }
            }
            b'2' => {
                let f: Vec<&str> = s.splitn(10, ' ').collect();
                // El origen va en el registro siguiente (separado por NUL).
                let orig = recs.next().map(|o| String::from_utf8_lossy(o).into_owned());
                if f.len() == 10 {
                    push_changed(&mut st, f[1], f[9], orig);
                }
            }
            b'u' => {
                let f: Vec<&str> = s.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let (x, y) = xy(f[1]);
                    st.files.push(FileEntry { path: f[10].into(), orig: None, x, y, kind: "conflict" });
                }
            }
            b'?' => {
                if let Some(p) = s.strip_prefix("? ").filter(|p| !p.is_empty()) {
                    st.files.push(FileEntry { path: p.into(), orig: None, x: "?".into(), y: "?".into(), kind: "untracked" });
                }
            }
            _ => {}
        }
    }
    st
}

fn status(root: &Path) -> Result<Value, String> {
    let mut cmd = git(root);
    cmd.args(["status", "--porcelain=v2", "-z", "--branch"]);
    let (mut out, r) = collect(&mut cmd, STATUS_MAX_BYTES)?;
    if !r.stopped && !r.success() {
        return Err(r.err_line("git status: "));
    }
    if r.stopped {
        // El último registro puede haber quedado a medias.
        let end = out.iter().rposition(|&b| b == 0).map_or(0, |i| i + 1);
        out.truncate(end);
    }
    let mut st = parse_status(&out);
    st.truncated = r.stopped;
    st.root = root.to_string_lossy().into_owned();
    st.repo = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    serde_json::to_value(st).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------------------
// diff

/// Parte la salida en líneas (sin el `\n`), con un máximo de líneas. Si los bytes se
/// cortaron, la última línea (incompleta) se descarta.
fn diff_lines(out: &[u8], bytes_cut: bool) -> (Vec<String>, bool) {
    let mut parts: Vec<&[u8]> = out.split(|&b| b == b'\n').collect();
    if bytes_cut || parts.last().is_some_and(|l| l.is_empty()) {
        parts.pop();
    }
    let truncated = bytes_cut || parts.len() > DIFF_MAX_LINES;
    let lines = parts.into_iter().take(DIFF_MAX_LINES).map(|l| String::from_utf8_lossy(l).into_owned()).collect();
    (lines, truncated)
}

fn diff(root: &Path, path: &str, staged: bool, untracked: bool) -> Result<Value, String> {
    let exist = if untracked { Exist::Required } else { Exist::MayBeMissing };
    let abs = validate_path(root, path, exist)?;
    let mut cmd = git(root);
    if untracked {
        if abs.is_dir() {
            return Err("Es un directorio sin seguimiento".into());
        }
        cmd.args(["diff", "--no-index", "--no-color", "--no-ext-diff", "--no-textconv", "--", "/dev/null"]).arg(path);
    } else {
        cmd.args(["diff", "--no-color", "--no-ext-diff", "--no-textconv"]);
        if staged {
            cmd.arg("--cached");
        }
        cmd.arg("--").arg(path);
    }
    let mut out = Vec::new();
    let mut nl = 0usize;
    let r = run(&mut cmd, |c| {
        out.extend_from_slice(c);
        nl += c.iter().filter(|&&b| b == b'\n').count();
        out.len() <= DIFF_MAX_BYTES && nl <= DIFF_MAX_LINES
    })?;
    // --no-index devuelve 1 cuando hay diferencias.
    let ok = r.stopped || r.success() || (untracked && r.code() == Some(1));
    if !ok {
        return Err(r.err_line("git diff: "));
    }
    let bytes_cut = out.len() > DIFF_MAX_BYTES;
    out.truncate(DIFF_MAX_BYTES);
    // Parado por líneas: la última puede estar a medias igualmente.
    let (lines, truncated) = diff_lines(&out, bytes_cut || r.stopped);
    Ok(json!({"path": path, "staged": staged, "untracked": untracked, "lines": lines, "truncated": truncated || r.stopped}))
}

// ---------------------------------------------------------------------------------------
// grep

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub text: String,
}

fn valid_query(q: &str) -> Result<(), String> {
    if q.is_empty() {
        return Err("Escribe algo que buscar".into());
    }
    if q.chars().count() > GREP_MAX_Q {
        return Err(format!("Búsqueda demasiado larga (máx. {GREP_MAX_Q} caracteres)"));
    }
    if q.chars().any(char::is_control) {
        return Err("Búsqueda no válida".into());
    }
    Ok(())
}

/// Una línea de `git grep -n -z`: `ruta\0línea\0texto`.
fn parse_hit(line: &[u8]) -> Option<Hit> {
    let mut it = line.splitn(3, |&b| b == 0);
    let path = it.next().filter(|p| !p.is_empty())?;
    let n = std::str::from_utf8(it.next()?).ok()?.parse().ok()?;
    let text = String::from_utf8_lossy(it.next().unwrap_or(b""));
    let text: String = text.trim_end_matches('\r').chars().take(GREP_TEXT_CHARS).collect();
    Some(Hit { path: String::from_utf8_lossy(path).into_owned(), line: n, text })
}

/// Acumula líneas de una salida por trozos, guardando como mucho `GREP_LINE_CAP` bytes de
/// cada una. `on_line` devuelve `false` para parar.
struct LineSplitter {
    cur: Vec<u8>,
}

impl LineSplitter {
    fn feed(&mut self, chunk: &[u8], on_line: &mut impl FnMut(&[u8]) -> bool) -> bool {
        let mut rest = chunk;
        while let Some(i) = rest.iter().position(|&b| b == b'\n') {
            self.push(&rest[..i]);
            let go = on_line(&self.cur);
            self.cur.clear();
            if !go {
                return false;
            }
            rest = &rest[i + 1..];
        }
        self.push(rest);
        true
    }
    fn push(&mut self, b: &[u8]) {
        let room = GREP_LINE_CAP.saturating_sub(self.cur.len());
        self.cur.extend_from_slice(&b[..b.len().min(room)]);
    }
}

fn grep(root: &Path, q: &str, regex: bool) -> Result<Value, String> {
    valid_query(q)?;
    let mut cmd = git(root);
    cmd.args(["grep", "-n", "-I", "--no-color", "--full-name", "-z", if regex { "-E" } else { "-F" }, "-e"])
        .arg(q)
        .arg("--");
    let mut hits: Vec<Hit> = Vec::new();
    let mut sp = LineSplitter { cur: Vec::new() };
    let mut on_line = |l: &[u8]| {
        if let Some(h) = parse_hit(l) {
            hits.push(h);
        }
        hits.len() <= GREP_MAX_HITS
    };
    let r = run(&mut cmd, |c| sp.feed(c, &mut on_line))?;
    if !r.stopped && !sp.cur.is_empty() {
        let last = std::mem::take(&mut sp.cur);
        on_line(&last);
    }
    match r.code() {
        _ if r.stopped => {}
        Some(0) => {}
        Some(1) => hits.clear(), // sin coincidencias
        _ => return Err(r.err_line("git grep: ")),
    }
    let truncated = hits.len() > GREP_MAX_HITS;
    hits.truncate(GREP_MAX_HITS);
    Ok(json!({"hits": hits, "truncated": truncated}))
}

// ---------------------------------------------------------------------------------------
// gh (PRs e issues)

#[derive(Deserialize)]
struct GhAuthor {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    #[serde(default)]
    title: String,
    author: Option<GhAuthor>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    status_check_rollup: Option<Vec<Value>>,
    #[serde(default)]
    review_decision: Option<String>,
}

#[derive(Deserialize)]
struct GhLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhIssue {
    number: u64,
    #[serde(default)]
    title: String,
    author: Option<GhAuthor>,
    #[serde(default)]
    url: String,
    #[serde(default)]
    labels: Vec<GhLabel>,
    #[serde(default)]
    updated_at: String,
}

/// Estado de un elemento de `statusCheckRollup` (CheckRun o StatusContext), en mayúsculas.
fn check_state(v: &Value) -> String {
    let s = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_ascii_uppercase);
    if let Some(st) = s("status")
        && st != "COMPLETED"
    {
        return st;
    }
    s("conclusion").or_else(|| s("state")).unwrap_or_else(|| "PENDING".into())
}

/// Resume el CI: algún fallo → fail; algo sin terminar (o desconocido) → pending;
/// todo SUCCESS/NEUTRAL/SKIPPED → ok; sin checks → none.
fn summarize_ci(checks: &[Value]) -> &'static str {
    if checks.is_empty() {
        return "none";
    }
    let states: Vec<String> = checks.iter().map(check_state).collect();
    const FAIL: [&str; 6] = ["FAILURE", "ERROR", "CANCELLED", "TIMED_OUT", "ACTION_REQUIRED", "STARTUP_FAILURE"];
    const OK: [&str; 3] = ["SUCCESS", "NEUTRAL", "SKIPPED"];
    if states.iter().any(|s| FAIL.contains(&s.as_str())) {
        "fail"
    } else if states.iter().all(|s| OK.contains(&s.as_str())) {
        "ok"
    } else {
        "pending"
    }
}

/// Mensaje legible para un fallo de gh.
fn gh_error(stderr: &str) -> String {
    let l = stderr.to_ascii_lowercase();
    if l.contains("gh auth login") || l.contains("not logged") || l.contains("authentication") {
        "gh no está autenticado: ejecuta «gh auth login» en un terminal".into()
    } else if l.contains("no git remotes") || l.contains("none of the git remotes") || l.contains("not a github") {
        "El repositorio no tiene un remoto de GitHub".into()
    } else {
        let line = stderr.lines().map(str::trim).find(|x| !x.is_empty()).unwrap_or("error desconocido");
        format!("gh: {}", line.chars().filter(|c| !c.is_control()).take(300).collect::<String>())
    }
}

fn gh_json(root: &Path, args: &[&str]) -> Result<Value, String> {
    let bin = envpath::which("gh").ok_or("gh no está instalado (brew install gh)")?;
    let mut cmd = Command::new(bin);
    base_env(&mut cmd);
    cmd.current_dir(root).args(args);
    let (out, r) = collect(&mut cmd, GH_MAX_BYTES)?;
    if r.stopped {
        return Err("Respuesta de gh demasiado grande".into());
    }
    if !r.success() {
        return Err(gh_error(&String::from_utf8_lossy(&r.stderr)));
    }
    serde_json::from_slice(&out).map_err(|_| "Respuesta de gh no válida".to_string())
}

fn prs_from(v: Value) -> Result<Value, String> {
    let list: Vec<GhPr> = serde_json::from_value(v).map_err(|_| "Respuesta de gh no válida".to_string())?;
    let out: Vec<Value> = list
        .into_iter()
        .map(|p| {
            let mut o = json!({
                "number": p.number,
                "title": p.title,
                "author": p.author.map(|a| a.login).unwrap_or_default(),
                "url": p.url,
                "draft": p.is_draft,
                "ci": summarize_ci(p.status_check_rollup.as_deref().unwrap_or(&[])),
            });
            if let Some(r) = p.review_decision.filter(|r| !r.is_empty()) {
                o["review"] = Value::String(r);
            }
            o
        })
        .collect();
    Ok(Value::Array(out))
}

/// Color de etiqueta: 6 dígitos hex (sin `#`), o vacío si no lo es.
fn label_color(c: &str) -> String {
    if c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit()) { c.to_ascii_lowercase() } else { String::new() }
}

fn issues_from(v: Value) -> Result<Value, String> {
    let list: Vec<GhIssue> = serde_json::from_value(v).map_err(|_| "Respuesta de gh no válida".to_string())?;
    let out: Vec<Value> = list
        .into_iter()
        .map(|i| {
            let labels: Vec<Value> =
                i.labels.into_iter().map(|l| json!({"name": l.name, "color": label_color(&l.color)})).collect();
            json!({
                "number": i.number,
                "title": i.title,
                "author": i.author.map(|a| a.login).unwrap_or_default(),
                "url": i.url,
                "labels": labels,
                "updated": i.updated_at,
            })
        })
        .collect();
    Ok(Value::Array(out))
}

fn prs(root: &Path) -> Result<Value, String> {
    let v = gh_json(
        root,
        &["pr", "list", "--json", "number,title,author,url,isDraft,statusCheckRollup,reviewDecision", "--limit", "30"],
    )?;
    prs_from(v)
}

fn issues(root: &Path) -> Result<Value, String> {
    let v = gh_json(root, &["issue", "list", "--json", "number,title,author,url,labels,updatedAt", "--limit", "30"])?;
    issues_from(v)
}

// ---------------------------------------------------------------------------------------
// Abrir en el editor

/// `[editor] gui` de un config.toml (solo esa clave).
fn template_from_toml(text: &str) -> Option<String> {
    let t: toml::Table = toml::from_str(text).ok()?;
    let s = t.get("editor")?.get("gui")?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn editor_template() -> Option<String> {
    let home = std::env::var("HOME").ok().filter(|h| h.starts_with('/'))?;
    let path = Path::new(&home).join(".config/mish/config.toml");
    let f = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    f.take(CONFIG_MAX).read_to_string(&mut text).ok()?;
    template_from_toml(&text)
}

/// Sustituye `{file}` y `{line}` en una sola pasada (una ruta con `{line}` dentro no se
/// vuelve a sustituir).
fn subst(tok: &str, file: &str, line: u32) -> (String, bool) {
    let mut out = String::new();
    let mut rest = tok;
    let mut used_file = false;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let r = &rest[i..];
        if let Some(after) = r.strip_prefix("{file}") {
            out.push_str(file);
            used_file = true;
            rest = after;
        } else if let Some(after) = r.strip_prefix("{line}") {
            out.push_str(&line.to_string());
            rest = after;
        } else {
            out.push('{');
            rest = &r[1..];
        }
    }
    out.push_str(rest);
    (out, used_file)
}

/// argv del editor (el primero, el programa resuelto). La plantilla se separa por espacios
/// ANTES de sustituir, así que una ruta con espacios sigue siendo un único argumento. Sin
/// `{file}` en la plantilla, la ruta va al final. Sin plantilla, `code -g file:line`.
fn editor_argv(
    template: Option<&str>,
    file: &Path,
    line: u32,
    which: impl Fn(&str) -> Option<PathBuf>,
) -> Result<Vec<String>, String> {
    let file = file.to_str().ok_or(BAD_PATH)?;
    let line = line.max(1);
    let Some(t) = template else {
        let code = which("code").ok_or(NO_EDITOR)?;
        return Ok(vec![code.to_string_lossy().into_owned(), "-g".into(), format!("{file}:{line}")]);
    };
    let mut toks = t.split_whitespace();
    let prog = toks.next().ok_or(NO_EDITOR)?;
    if prog.contains('{') {
        return Err("Plantilla de editor no válida: el primer elemento debe ser el programa".into());
    }
    let bin = which(prog).ok_or_else(|| format!("No se encuentra el editor «{prog}»"))?;
    let mut argv = vec![bin.to_string_lossy().into_owned()];
    let mut used = false;
    for tok in toks {
        let (a, u) = subst(tok, file, line);
        used |= u;
        argv.push(a);
    }
    if !used {
        argv.push(file.into());
    }
    Ok(argv)
}

fn open_file(root: &Path, path: &str, line: Option<u32>) -> Result<Value, String> {
    let abs = validate_path(root, path, Exist::Required)?;
    let argv = editor_argv(editor_template().as_deref(), &abs, line.unwrap_or(1), envpath::which)?;
    let mut cmd = Command::new(&argv[0]);
    base_env(&mut cmd);
    let mut child = cmd
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("No se pudo abrir el editor: {e}"))?;
    // Se recoge fuera para no dejar zombis (el editor puede quedarse abierto).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(json!({}))
}

// ---------------------------------------------------------------------------------------
// URLs

/// Host de una URL `https://host/…` (sin userinfo ni mayúsculas); `None` si no lo es.
fn https_host(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    if host.is_empty() || host.contains('@') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Solo `https://github.com/…` o `https://<host del remoto origin>/…`.
fn url_allowed(url: &str, extra_host: Option<&str>) -> bool {
    if url.is_empty() || url.len() > URL_MAX || url.chars().any(|c| c.is_control() || c.is_whitespace() || c == '\\') {
        return false;
    }
    let Some(host) = https_host(url) else { return false };
    let rest = &url["https://".len() + host.len()..];
    if !rest.starts_with('/') {
        return false;
    }
    host == "github.com" || extra_host.is_some_and(|h| h.eq_ignore_ascii_case(&host))
}

/// Host del remoto `origin` si es https (con userinfo quitado).
fn origin_https_host(root: &Path) -> Option<String> {
    let mut cmd = git(root);
    cmd.args(["remote", "get-url", "origin"]);
    let (out, r) = collect(&mut cmd, URL_MAX).ok()?;
    if !r.success() {
        return None;
    }
    let s = String::from_utf8(out).ok()?;
    let rest = s.trim().strip_prefix("https://")?;
    let auth = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
    https_host(&format!("https://{host}/"))
}
