//! `mish claude-hooks install|uninstall|status [--dry-run]`: registra `mish hook <evento>` en
//! `~/.claude/settings.json` para que el daemon conozca el estado de Claude en cada sesión.
//!
//! - Por cada evento de [`EVENTS`] se añade un grupo sin `matcher`
//!   `{"hooks":[{"type":"command","command":"'<ruta>/mish' hook <Evento>","timeout":5}]}`.
//!   Siempre la forma de **cadena** (nunca `args`) con la ruta entre comillas simples.
//! - Lo nuestro se reconoce porque `command` contiene [`MARK`] (`/mish' hook `). Nada más se
//!   toca: se conservan el orden y todas las demás claves (`serde_json` con `preserve_order`).
//! - Idempotente: `install` no duplica y actualiza la ruta si cambió; `uninstall` quita solo
//!   nuestros handlers y los grupos/eventos que se quedan vacíos por ello.
//! - JSON inválido (o con una forma inesperada) → se aborta sin escribir y exit 1.
//! - Antes de escribir, copia `settings.json.mish-bak-<epoch>` (0600). Escritura atómica
//!   (tmp + rename) con los permisos del original; si el fichero cambia entre la lectura y la
//!   escritura se reintenta (hasta 3 veces) y al final se relee y valida. Si `settings.json` es un
//!   symlink se escribe en su destino real.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

/// Eventos de Claude Code en los que se instala `mish hook`.
pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// Marca que identifica un handler de mish dentro de `command`.
pub const MARK: &str = "/mish' hook ";
/// Segundos de `timeout` del handler.
pub const TIMEOUT_S: u64 = 5;

const USAGE: &str = "uso: mish claude-hooks install|uninstall|status [--dry-run]";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Install,
    Uninstall,
    Status,
}

pub fn run(args: &[String]) -> i32 {
    let mut action = None;
    let mut dry = false;
    for a in args {
        match a.as_str() {
            "install" if action.is_none() => action = Some(Action::Install),
            "uninstall" if action.is_none() => action = Some(Action::Uninstall),
            "status" if action.is_none() => action = Some(Action::Status),
            "--dry-run" | "-n" => dry = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                println!("  install    añade `mish hook <evento>` a ~/.claude/settings.json ({})", EVENTS.join(", "));
                println!("  uninstall  quita solo los handlers de mish");
                println!("  status     dice si están instalados, en qué eventos y con qué ruta");
                println!("  --dry-run  muestra el cambio en \"hooks\" sin escribir");
                return 0;
            }
            other => {
                eprintln!("claude-hooks: argumento inesperado: {other}\n{USAGE}");
                return 1;
            }
        }
    }
    let Some(action) = action else {
        eprintln!("{USAGE}");
        return 1;
    };
    let Some(path) = settings_path() else {
        eprintln!("claude-hooks: no hay HOME");
        return 1;
    };
    let exe = match stable_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("claude-hooks: no se pudo resolver la ruta de mish: {e}");
            return 1;
        }
    };
    let mut out = io::stdout().lock();
    let r = match action {
        Action::Status => status_at(&path, &exe, &mut out),
        a => modify_at(&path, &exe, a, dry, &mut out),
    };
    match r {
        Ok(()) => 0,
        Err(e) => {
            let _ = out.flush();
            eprintln!("claude-hooks: {e}");
            1
        }
    }
}

/// `$HOME/.claude/settings.json`.
pub fn settings_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".claude/settings.json"))
}

/// Ruta estable de este `mish`: si corre desde un bundle `.app`, `<bundle>/Contents/MacOS/mish`;
/// si no, `current_exe` canonicalizado.
pub fn stable_exe() -> io::Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    for anc in exe.ancestors() {
        if anc.extension().is_some_and(|e| e == "app") {
            let c = anc.join("Contents/MacOS/mish");
            if c.is_file() {
                return c.canonicalize();
            }
        }
    }
    Ok(exe)
}

/// Comillas simples para `sh`: `'` → `'\''`.
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Inversa de [`shell_quote`] para el primer token de `command` (solo comillas simples y `\`).
fn first_word(cmd: &str) -> String {
    let mut out = String::new();
    let mut it = cmd.chars();
    let mut quoted = false;
    while let Some(c) = it.next() {
        match c {
            '\'' => quoted = !quoted,
            '\\' if !quoted => {
                if let Some(n) = it.next() {
                    out.push(n);
                }
            }
            c if c.is_whitespace() && !quoted => break,
            c => out.push(c),
        }
    }
    out
}

pub fn command_for(exe: &str, event: &str) -> String {
    format!("{} hook {event}", shell_quote(exe))
}

fn handler_for(exe: &str, event: &str) -> Value {
    json!({"type": "command", "command": command_for(exe, event), "timeout": TIMEOUT_S})
}

pub fn is_ours(h: &Value) -> bool {
    h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(MARK))
}

/// Ajusta los grupos de un evento: deja como mucho un handler nuestro (igual a `want`) o
/// ninguno (`want = None`). Los grupos que se quedan vacíos *por esto* se quitan. Devuelve si
/// cambió algo.
fn fix_event(arr: &mut Vec<Value>, want: Option<&Value>) -> bool {
    let mut changed = false;
    let mut placed = false;
    arr.retain_mut(|g| {
        let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = hs.len();
        hs.retain_mut(|h| {
            if !is_ours(h) {
                return true;
            }
            match want {
                Some(w) if !placed => {
                    placed = true;
                    if h != w {
                        *h = w.clone();
                        changed = true;
                    }
                    true
                }
                _ => {
                    changed = true;
                    false
                }
            }
        });
        !(hs.is_empty() && hs.len() != before)
    });
    if let Some(w) = want
        && !placed
    {
        arr.push(json!({"hooks": [w]}));
        changed = true;
    }
    changed
}

fn hooks_obj(root: &mut Value, create: bool) -> Result<Option<&mut Map<String, Value>>, String> {
    let obj = root.as_object_mut().ok_or("la raíz de settings.json no es un objeto")?;
    if !obj.contains_key("hooks") {
        if !create {
            return Ok(None);
        }
        obj.insert("hooks".into(), json!({}));
    }
    obj.get_mut("hooks")
        .and_then(Value::as_object_mut)
        .map(Some)
        .ok_or_else(|| "\"hooks\" no es un objeto".to_string())
}

/// Aplica `install` sobre el JSON. Devuelve si cambió.
pub fn apply_install(root: &mut Value, exe: &str) -> Result<bool, String> {
    let had_hooks = root.get("hooks").is_some();
    let hooks = hooks_obj(root, true)?.expect("creado");
    // Validar antes de tocar nada: todo evento que vayamos a tocar debe ser un array.
    for (k, v) in hooks.iter() {
        let touches = EVENTS.contains(&k.as_str()) || v.as_array().is_some_and(|a| a.iter().any(group_has_ours));
        if touches && !v.is_array() {
            return Err(format!("\"hooks.{k}\" no es un array"));
        }
    }
    let mut changed = !had_hooks;
    // Nuestros handlers en eventos que ya no usamos (versiones anteriores) se quitan.
    let stale: Vec<String> = hooks
        .iter()
        .filter(|(k, v)| !EVENTS.contains(&k.as_str()) && v.as_array().is_some_and(|a| a.iter().any(group_has_ours)))
        .map(|(k, _)| k.clone())
        .collect();
    for k in stale {
        let arr = hooks.get_mut(&k).and_then(Value::as_array_mut).expect("validado");
        changed |= fix_event(arr, None);
        if arr.is_empty() {
            hooks.shift_remove(&k);
        }
    }
    for ev in EVENTS {
        let want = handler_for(exe, ev);
        if !hooks.contains_key(*ev) {
            hooks.insert((*ev).into(), json!([]));
        }
        let arr = hooks.get_mut(*ev).and_then(Value::as_array_mut).expect("validado");
        changed |= fix_event(arr, Some(&want));
    }
    Ok(changed)
}

/// Aplica `uninstall` sobre el JSON. Devuelve si cambió.
pub fn apply_uninstall(root: &mut Value) -> Result<bool, String> {
    let Some(hooks) = hooks_obj(root, false)? else {
        return Ok(false);
    };
    let mut changed = false;
    let keys: Vec<String> = hooks.keys().cloned().collect();
    for k in keys {
        let Some(arr) = hooks.get_mut(&k).and_then(Value::as_array_mut) else {
            continue;
        };
        if fix_event(arr, None) {
            changed = true;
            if arr.is_empty() {
                hooks.shift_remove(&k);
            }
        }
    }
    if changed && hooks.is_empty() {
        root.as_object_mut().expect("objeto").shift_remove("hooks");
    }
    Ok(changed)
}

fn group_has_ours(g: &Value) -> bool {
    g.get("hooks").and_then(Value::as_array).is_some_and(|hs| hs.iter().any(is_ours))
}

/// `(evento, command)` de cada handler nuestro, en orden.
pub fn ours(root: &Value) -> Vec<(String, String)> {
    let mut v = Vec::new();
    let Some(hooks) = root.get("hooks").and_then(Value::as_object) else {
        return v;
    };
    for (ev, arr) in hooks {
        for g in arr.as_array().into_iter().flatten() {
            for h in g.get("hooks").and_then(Value::as_array).into_iter().flatten() {
                if is_ours(h) {
                    v.push((ev.clone(), h["command"].as_str().unwrap_or_default().to_owned()));
                }
            }
        }
    }
    v
}

// ---------------------------------------------------------------- fichero

/// Huella para detectar cambios entre la lectura y la escritura.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    mtime: Option<SystemTime>,
    len: u64,
    ino: u64,
    mode: u32,
}

impl Stamp {
    fn of(md: &fs::Metadata) -> Self {
        Stamp { mtime: md.modified().ok(), len: md.len(), ino: md.ino(), mode: md.permissions().mode() & 0o7777 }
    }
}

/// Destino real: si `p` es un symlink, su destino canonicalizado (error si está roto).
fn resolve(p: &Path) -> Result<PathBuf, String> {
    match fs::symlink_metadata(p) {
        Ok(md) if md.file_type().is_symlink() => {
            fs::canonicalize(p).map_err(|e| format!("{} es un symlink sin destino válido: {e}", p.display()))
        }
        _ => Ok(p.to_path_buf()),
    }
}

/// Lee el fichero (o `None` si no existe) con su huella.
fn read_settings(p: &Path) -> Result<(Option<String>, Option<Stamp>), String> {
    match fs::read(p) {
        Ok(b) => {
            let md = fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let s = String::from_utf8(b).map_err(|_| format!("{}: no es UTF-8; no se escribe", p.display()))?;
            Ok((Some(s), Some(Stamp::of(&md))))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok((None, None)),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

fn parse(text: Option<&str>, p: &Path) -> Result<Value, String> {
    let t = text.unwrap_or("{}");
    let v: Value = serde_json::from_str(t)
        .map_err(|e| format!("{}: JSON inválido ({e}); no se escribe nada", p.display()))?;
    if !v.is_object() {
        return Err(format!("{}: la raíz no es un objeto; no se escribe nada", p.display()));
    }
    Ok(v)
}

fn render(v: &Value, trailing_nl: bool) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("Value siempre serializa");
    if trailing_nl {
        s.push('\n');
    }
    s
}

fn epoch() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Crea `<p>.mish-bak-<epoch>[-n]` con modo 0600 y el contenido dado.
fn backup(p: &Path, text: &str) -> Result<PathBuf, String> {
    let base = format!("{}.mish-bak-{}", p.display(), epoch());
    for n in 0..100 {
        let cand = PathBuf::from(if n == 0 { base.clone() } else { format!("{base}-{n}") });
        match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&cand) {
            Ok(mut f) => {
                // create_new + mode ya da 0600 (umask solo quita bits); se fija por si acaso.
                let _ = f.set_permissions(fs::Permissions::from_mode(0o600));
                f.write_all(text.as_bytes())
                    .and_then(|_| f.sync_all())
                    .map_err(|e| format!("copia {}: {e}", cand.display()))?;
                return Ok(cand);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("copia {}: {e}", cand.display())),
        }
    }
    Err("no se pudo crear la copia de seguridad".into())
}

/// Escribe `text` en un temporal del mismo directorio con modo `mode`.
fn write_tmp(target: &Path, text: &str, mode: u32) -> Result<PathBuf, String> {
    let dir = target.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("settings.json");
    let tmp = dir.join(format!(".{name}.mish-tmp-{}-{}", std::process::id(), epoch()));
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    let r = f
        .write_all(text.as_bytes())
        .and_then(|_| f.set_permissions(fs::Permissions::from_mode(mode)))
        .and_then(|_| f.sync_all());
    if let Err(e) = r {
        let _ = fs::remove_file(&tmp);
        return Err(format!("{}: {e}", tmp.display()));
    }
    Ok(tmp)
}

fn current_stamp(p: &Path) -> Option<Stamp> {
    fs::metadata(p).ok().map(|m| Stamp::of(&m))
}

/// `install`/`uninstall` sobre `path` (normalmente `~/.claude/settings.json`).
pub fn modify_at(path: &Path, exe: &Path, action: Action, dry: bool, out: &mut dyn Write) -> Result<(), String> {
    let exe_s = exe.to_str().ok_or("la ruta de mish no es UTF-8")?;
    if action == Action::Install {
        if exe.file_name().is_none_or(|n| n != "mish") {
            return Err(format!("el ejecutable debe llamarse «mish» para reconocer sus hooks: {}", exe.display()));
        }
        if let Some(w) = unstable_exe_warning(exe_s) {
            eprintln!("aviso: {w}");
        }
    }
    let target = resolve(path)?;
    if target != path {
        let _ = writeln!(out, "{} → {}", path.display(), target.display());
    }
    let w = |e: io::Error| e.to_string();
    for _attempt in 0..3 {
        let (text, stamp) = read_settings(&target)?;
        let before = parse(text.as_deref(), &target)?;
        let mut after = before.clone();
        let changed = match action {
            Action::Install => apply_install(&mut after, exe_s)?,
            Action::Uninstall => apply_uninstall(&mut after)?,
            Action::Status => unreachable!(),
        };
        if !changed {
            let msg = match action {
                Action::Install => "ya instalados y al día: no hay nada que cambiar",
                _ => "no hay hooks de mish: no hay nada que cambiar",
            };
            writeln!(out, "{msg}").map_err(w)?;
            return Ok(());
        }
        let nl = text.as_deref().is_none_or(|t| t.ends_with('\n'));
        let new_text = render(&after, nl);
        if dry {
            writeln!(out, "--dry-run: {} (sin escribir)", target.display()).map_err(w)?;
            if text.as_deref().is_some_and(|t| t != render(&before, nl)) {
                writeln!(out, "nota: el fichero se reescribiría con formato normalizado (2 espacios)").map_err(w)?;
            }
            write_hooks_diff(&before, &after, out).map_err(w)?;
            return Ok(());
        }
        let mode = stamp.as_ref().map(|s| s.mode).unwrap_or(0o600);
        if let Some(parent) = target.parent()
            && !parent.as_os_str().is_empty()
            && !parent.exists()
        {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let bak = match &text {
            Some(t) => Some(backup(&target, t)?),
            None => None,
        };
        let tmp = write_tmp(&target, &new_text, mode)?;
        // ¿Cambió mientras tanto? Entonces se descarta y se vuelve a empezar.
        if current_stamp(&target) != stamp {
            let _ = fs::remove_file(&tmp);
            if let Some(b) = bak {
                let _ = fs::remove_file(b);
            }
            continue;
        }
        fs::rename(&tmp, &target).map_err(|e| {
            let _ = fs::remove_file(&tmp);
            format!("rename → {}: {e}", target.display())
        })?;
        // Releer y validar.
        let (re, _) = read_settings(&target)?;
        let re = parse(re.as_deref(), &target)?;
        if re != after {
            return Err(format!("{}: tras escribir, el contenido no coincide (¿otro proceso?)", target.display()));
        }
        if let Some(b) = &bak {
            writeln!(out, "copia de seguridad: {}", b.display()).map_err(w)?;
        }
        match action {
            Action::Install => writeln!(out, "instalados {} hooks con {}", EVENTS.len(), exe.display()),
            _ => writeln!(out, "hooks de mish eliminados de {}", target.display()),
        }
        .map_err(w)?;
        return Ok(());
    }
    Err(format!("{} cambió durante la escritura 3 veces; no se ha escrito. Reinténtalo.", target.display()))
}

fn unstable_exe_warning(exe: &str) -> Option<&'static str> {
    if exe.contains("/AppTranslocation/") {
        Some("mish corre desde una copia temporal de macOS (AppTranslocation); mueve Mish.app a /Applications")
    } else if exe.contains("/target/debug/") || exe.contains("/target/release/") {
        Some("la ruta es un binario de cargo (target/); al recompilar o limpiar puede cambiar o desaparecer")
    } else {
        None
    }
}

/// `status` sobre `path`.
pub fn status_at(path: &Path, exe: &Path, out: &mut dyn Write) -> Result<(), String> {
    let w = |e: io::Error| e.to_string();
    let target = resolve(path)?;
    if target != path {
        writeln!(out, "settings: {} → {}", path.display(), target.display()).map_err(w)?;
    } else {
        writeln!(out, "settings: {}", target.display()).map_err(w)?;
    }
    let (text, _) = read_settings(&target)?;
    if text.is_none() {
        writeln!(out, "(no existe)").map_err(w)?;
    }
    let v = parse(text.as_deref(), &target)?;
    let exe_s = exe.to_string_lossy();
    writeln!(out, "mish actual: {exe_s}").map_err(w)?;
    let found = ours(&v);
    if found.is_empty() {
        writeln!(out, "hooks de mish: no instalados").map_err(w)?;
        return Ok(());
    }
    let have: Vec<&str> = EVENTS.iter().copied().filter(|e| found.iter().any(|(ev, _)| ev == e)).collect();
    writeln!(out, "hooks de mish: instalados en {}/{} eventos", have.len(), EVENTS.len()).map_err(w)?;
    let mut paths: Vec<String> = Vec::new();
    for (ev, cmd) in &found {
        let p = first_word(cmd);
        let mut notes = Vec::new();
        if !EVENTS.contains(&ev.as_str()) {
            notes.push("evento no gestionado");
        }
        if !cmd.ends_with(&format!(" hook {ev}")) {
            notes.push("evento distinto en el comando");
        }
        if !Path::new(&p).is_file() {
            notes.push("el binario no existe");
        }
        let n = if notes.is_empty() { String::new() } else { format!("  [{}]", notes.join(", ")) };
        writeln!(out, "  {ev:<18} {p}{n}").map_err(w)?;
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    let missing: Vec<&str> = EVENTS.iter().copied().filter(|e| !have.contains(e)).collect();
    if !missing.is_empty() {
        writeln!(out, "faltan: {}", missing.join(", ")).map_err(w)?;
    }
    if paths.len() > 1 || paths.first().is_some_and(|p| *p != exe_s) {
        writeln!(out, "la ruta instalada no es la de este mish: `mish claude-hooks install` la actualiza").map_err(w)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- diff

/// Diff por líneas (LCS) del objeto `"hooks"` antes/después, con 2 líneas de contexto. Solo se
/// muestra `"hooks"`: el resto de settings.json no cambia y podría tener datos sensibles.
fn write_hooks_diff(before: &Value, after: &Value, out: &mut dyn Write) -> io::Result<()> {
    let empty = json!({});
    let pretty = |v: &Value| serde_json::to_string_pretty(v.get("hooks").unwrap_or(&empty)).unwrap_or_default();
    let (a, b) = (pretty(before), pretty(after));
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    writeln!(out, "--- \"hooks\" actual")?;
    writeln!(out, "+++ \"hooks\" resultante")?;
    if a.len() * b.len() > 4_000_000 {
        for l in &b {
            writeln!(out, "  {l}")?;
        }
        return Ok(());
    }
    // ops: (tipo, línea) con ' ', '-', '+'
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] { lcs[i + 1][j + 1] + 1 } else { lcs[i + 1][j].max(lcs[i][j + 1]) };
        }
    }
    let mut ops: Vec<(char, &str)> = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', a[i]));
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i][j + 1] >= lcs[i + 1][j]) {
            ops.push(('+', b[j]));
            j += 1;
        } else {
            ops.push(('-', a[i]));
            i += 1;
        }
    }
    const CTX: usize = 2;
    let near: Vec<bool> = (0..ops.len())
        .map(|k| {
            let lo = k.saturating_sub(CTX);
            let hi = (k + CTX).min(ops.len() - 1);
            (lo..=hi).any(|x| ops[x].0 != ' ')
        })
        .collect();
    let mut gap = false;
    for (k, (t, l)) in ops.iter().enumerate() {
        if near[k] {
            if gap {
                writeln!(out, "@@")?;
                gap = false;
            }
            writeln!(out, "{t} {l}")?;
        } else {
            gap = true;
        }
    }
    Ok(())
}
