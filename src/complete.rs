//! Autocompletado con Tab: builtins, alias, atajos, binarios del PATH, servidores, dirs y rutas.

use crate::State;
use reedline::{Completer, CompletionResult, Span, Suggestion};
use std::sync::{Arc, Mutex};

pub const BUILTINS: &[(&str, &str)] = &[
    ("cd", "cambiar de directorio"),
    ("ir", "saltar a un dir de [dirs]"),
    ("z", "saltar a un dir usado a menudo"),
    ("l", "listado visual de archivos"),
    ("s", "conectar a un servidor"),
    ("servers", "listar servidores (-p: comprobar conexión)"),
    ("reload", "recargar config"),
    ("alias", "ver/crear alias"),
    ("export", "definir variable de entorno"),
    ("unset", "borrar variable de entorno"),
    ("jobs", "trabajos detenidos"),
    ("fg", "reanudar trabajo"),
    ("update", "actualizar Mish.app (--check: solo comprobar)"),
    ("help", "ayuda"),
    ("exit", "salir"),
];

const SSH_CMDS: &[&str] = &["ssh", "scp", "sftp", "rsync", "mosh", "ssh-copy-id"];

pub struct MishCompleter {
    pub state: Arc<Mutex<State>>,
}

struct Cand {
    value: String,
    desc: Option<String>,
    space: bool,
}

impl Completer for MishCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> CompletionResult {
        let pos = pos.min(line.len());
        let before = &line[..pos];
        let start = word_start(before);
        let word = &before[start..];
        let head = before[..start].trim_end();
        let is_cmd = head.is_empty() || head.ends_with(['|', ';', '&']);
        let args: Vec<&str> = head.rsplit(['|', ';', '&']).next().unwrap_or("").split_whitespace().collect();

        let st = self.state.lock().unwrap();
        let mut c: Vec<Cand> = Vec::new();
        let push = |c: &mut Vec<Cand>, v: &str, d: Option<String>, space: bool| {
            if v.starts_with(word) {
                c.push(Cand { value: v.to_string(), desc: d, space });
            }
        };

        if is_cmd && !word.contains('/') {
            for (b, d) in BUILTINS {
                push(&mut c, b, Some(d.to_string()), true);
            }
            for (k, v) in &st.cfg.alias {
                push(&mut c, k, Some(format!("alias: {v}")), true);
            }
            for (k, v) in &st.cfg.atajos {
                push(&mut c, k, Some(format!("atajo: {v}")), true);
            }
            if !word.is_empty() {
                for b in &st.path_bins {
                    push(&mut c, b, None, true);
                }
            }
            if word.starts_with('.') || word.starts_with('~') {
                c.extend(paths(word));
            }
        } else {
            match args.first().copied() {
                Some("s") if args.len() == 1 => {
                    for (name, sv) in &st.servers {
                        let vars: Vec<&str> = [&sv.vpn, &sv.publico]
                            .into_iter()
                            .flatten()
                            .map(|h| h.rsplit('-').next().unwrap_or(""))
                            .collect();
                        let d = if vars.is_empty() { None } else { Some(vars.join(" · ")) };
                        push(&mut c, name, d, true);
                    }
                    for h in crate::servers::all_hosts(&st.servers) {
                        if !st.servers.contains_key(&h) && !word.is_empty() {
                            push(&mut c, &h, None, true);
                        }
                    }
                }
                Some(cmd) if SSH_CMDS.contains(&cmd) && !word.contains('/') => {
                    for h in crate::servers::all_hosts(&st.servers) {
                        push(&mut c, &h, None, true);
                    }
                    c.extend(paths(word));
                }
                Some("ir") if args.len() == 1 => {
                    for (k, v) in &st.cfg.dirs {
                        push(&mut c, k, Some(v.clone()), true);
                    }
                }
                Some("cd") | Some("l") => c.extend(paths(word).into_iter().filter(|p| !p.space)),
                Some("fg") => {}
                _ => c.extend(paths(word)),
            }
        }

        c.sort_by(|a, b| a.value.cmp(&b.value));
        c.dedup_by(|a, b| a.value == b.value);
        c.truncate(400);

        let sugg: Vec<Suggestion> = c
            .into_iter()
            .map(|c| Suggestion {
                value: c.value,
                description: c.desc,
                span: Span::new(start, pos),
                append_whitespace: c.space,
                ..Default::default()
            })
            .collect();
        CompletionResult::fresh(sugg)
    }
}

/// Inicio de la palabra actual (respeta espacios escapados con `\ `).
fn word_start(before: &str) -> usize {
    let b = before.as_bytes();
    let mut i = b.len();
    while i > 0 {
        let ch = b[i - 1];
        if matches!(ch, b' ' | b'|' | b';' | b'&' | b'\t') && !(i >= 2 && b[i - 2] == b'\\') {
            break;
        }
        i -= 1;
    }
    i
}

fn paths(word: &str) -> Vec<Cand> {
    let unescaped = word.replace("\\ ", " ");
    let (dir_raw, prefix) = match unescaped.rfind('/') {
        Some(i) => (&unescaped[..=i], &unescaped[i + 1..]),
        None => ("", unescaped.as_str()),
    };
    let dir_fs = if dir_raw.is_empty() { ".".to_string() } else { crate::parse::expand_tilde(dir_raw) };
    let Ok(rd) = std::fs::read_dir(&dir_fs) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
            continue;
        }
        let is_dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
        let mut v = format!("{dir_raw}{name}").replace(' ', "\\ ");
        if is_dir {
            v.push('/');
        }
        out.push(Cand { value: v, desc: None, space: !is_dir });
    }
    out
}

/// Ejecutables del PATH (se calcula al arrancar y en `reload`).
pub fn scan_path() -> Vec<String> {
    use std::os::unix::fs::PermissionsExt;
    let mut v = Vec::new();
    for dir in std::env::var("PATH").unwrap_or_default().split(':') {
        let Ok(rd) = std::fs::read_dir(dir) else { continue };
        for e in rd.flatten() {
            if let Ok(m) = std::fs::metadata(e.path()) {
                if m.is_file() && m.permissions().mode() & 0o111 != 0 {
                    v.push(e.file_name().to_string_lossy().to_string());
                }
            }
        }
    }
    v.sort();
    v.dedup();
    v
}
