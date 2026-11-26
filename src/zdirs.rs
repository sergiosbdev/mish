//! `z consulta`: salta a directorios usados a menudo (frecencia), sin configurarlos.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX: usize = 500;

struct Entry {
    path: String,
    score: f64,
    last: u64,
}

fn db_path() -> PathBuf {
    PathBuf::from(crate::parse::home()).join(".local/share/mish/dirs.tsv")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn load() -> Vec<Entry> {
    let text = std::fs::read_to_string(db_path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut p = l.splitn(3, '\t');
            let score = p.next()?.parse().ok()?;
            let last = p.next()?.parse().ok()?;
            let path = p.next()?.to_string();
            Some(Entry { path, score, last })
        })
        .collect()
}

fn save(v: &[Entry]) {
    let p = db_path();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let text: String = v.iter().map(|e| format!("{:.2}\t{}\t{}\n", e.score, e.last, e.path)).collect();
    let _ = std::fs::write(p, text);
}

/// Registra una visita al directorio.
pub fn visit(path: &str) {
    if path == crate::parse::home() || path == "/" {
        return;
    }
    let mut v = load();
    match v.iter_mut().find(|e| e.path == path) {
        Some(e) => {
            e.score += 1.0;
            e.last = now();
        }
        None => v.push(Entry { path: path.to_string(), score: 1.0, last: now() }),
    }
    if v.len() > MAX {
        v.sort_by(|a, b| frecency(b).total_cmp(&frecency(a)));
        v.truncate(MAX);
    }
    save(&v);
}

fn frecency(e: &Entry) -> f64 {
    let age = now().saturating_sub(e.last);
    let w = match age {
        a if a < 3600 => 4.0,
        a if a < 86_400 => 2.0,
        a if a < 604_800 => 0.5,
        _ => 0.25,
    };
    e.score * w
}

/// Mejor coincidencia: todas las palabras deben aparecer en orden en la ruta; la última en el nombre final.
pub fn best(query: &[String]) -> Option<String> {
    ranked(query).into_iter().find(|p| std::path::Path::new(p).is_dir())
}

pub fn ranked(query: &[String]) -> Vec<String> {
    let q: Vec<String> = query.iter().map(|s| s.to_lowercase()).collect();
    // Cada visita cuenta también (a mitad) para sus carpetas padre dentro del home.
    let home = crate::parse::home();
    let mut scores: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for e in load() {
        let f = frecency(&e);
        *scores.entry(e.path.clone()).or_default() += f;
        let mut p = std::path::Path::new(&e.path).parent();
        while let Some(d) = p {
            let ds = d.to_string_lossy().to_string();
            if ds.len() <= home.len() {
                break;
            }
            *scores.entry(ds).or_default() += f * 0.5;
            p = d.parent();
        }
    }
    let mut v: Vec<(f64, String)> =
        scores.into_iter().filter(|(p, _)| matches(&p.to_lowercase(), &q)).map(|(p, f)| (f, p)).collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
    v.into_iter().map(|(_, p)| p).collect()
}

fn matches(path: &str, q: &[String]) -> bool {
    if q.is_empty() {
        return true;
    }
    let mut rest = path;
    for w in q {
        match rest.find(w.as_str()) {
            Some(i) => rest = &rest[i + w.len()..],
            None => return false,
        }
    }
    let last_comp = path.rsplit('/').next().unwrap_or("");
    last_comp.contains(q.last().unwrap().as_str())
}
