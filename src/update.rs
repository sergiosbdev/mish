//! `mish update [--check]` (subcomando y builtin `update`): actualiza Mish.app desde GitHub
//! Releases con el crate `mish-updater`.
//!
//! - Dentro de Mish.app (`TERM_PROGRAM=mish-app` con la salida en el terminal) no hace nada
//!   por su cuenta: emite `OSC 7703;update` (con `--check`) o `OSC 7703;update-now` y la app
//!   comprueba y muestra su aviso; instalar requiere pulsar «Actualizar» en él (el OSC nunca
//!   instala por sí solo).
//! - Fuera, busca el bundle (`$MISH_BUNDLE`, el `.app` que contiene este ejecutable,
//!   `/Applications/Mish.app` o `~/Applications/Mish.app`), comprueba, descarga con barra de
//!   progreso en stderr e instala.
//!
//! Códigos de salida: 0 al día o actualizado, 1 error, 2 hay versión nueva (con `--check`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mish_updater as upd;

pub const EXIT_OK: i32 = 0;
pub const EXIT_ERR: i32 = 1;
pub const EXIT_AVAILABLE: i32 = 2;

#[derive(Debug, PartialEq, Eq)]
struct Opts {
    check: bool,
}

fn parse_args(args: &[String]) -> Result<Option<Opts>, String> {
    let mut check = false;
    for a in args {
        match a.as_str() {
            "--check" | "-c" => check = true,
            "-h" | "--help" => return Ok(None),
            other => return Err(format!("opción desconocida: {other}")),
        }
    }
    Ok(Some(Opts { check }))
}

fn usage() {
    println!("uso: mish update [--check]");
    println!("  sin opciones  descarga e instala la última versión de Mish.app");
    println!("  --check       solo comprueba (sale con 2 si hay una versión nueva)");
}

/// Payload de `OSC 7703` para la app.
fn osc_payload(check: bool) -> &'static str {
    if check { "update" } else { "update-now" }
}

/// Lo que se imprime tras pedir el flujo a Mish.app.
fn inside_app_msg(check: bool) -> &'static str {
    if check { "Mira el aviso de Mish.app." } else { "Pulsa Actualizar en el aviso de Mish.app." }
}

/// ¿Estamos en una sesión de Mish.app con la salida en su terminal?
fn inside_app() -> bool {
    std::env::var("TERM_PROGRAM").is_ok_and(|v| v == "mish-app") && unsafe { libc::isatty(1) } == 1
}

/// Bundle a actualizar: `$MISH_BUNDLE`, el `.app` de este ejecutable, `/Applications/Mish.app`
/// o `~/Applications/Mish.app`; el primero que exista.
fn find_bundle(env_bundle: Option<&str>, exe: Option<&Path>, home: Option<&str>) -> Option<PathBuf> {
    let is_app = |p: &Path| p.extension().is_some_and(|e| e == "app") && p.join("Contents/MacOS").is_dir();
    if let Some(b) = env_bundle.filter(|b| !b.is_empty()).map(PathBuf::from)
        && is_app(&b)
    {
        return Some(b);
    }
    if let Some(b) = exe.and_then(upd::bundle_of)
        && is_app(&b)
    {
        return Some(b);
    }
    let mut cands = vec![PathBuf::from("/Applications/Mish.app")];
    if let Some(h) = home.filter(|h| !h.is_empty()) {
        cands.push(Path::new(h).join("Applications/Mish.app"));
    }
    cands.into_iter().find(|p| is_app(p))
}

fn mb(b: u64) -> f64 {
    b as f64 / (1024.0 * 1024.0)
}

/// Línea de progreso: `[########------------]  40 %  4.8 / 12.0 MB` (o solo MB sin total).
fn progress_line(done: u64, total: Option<u64>) -> String {
    const W: usize = 24;
    match total.filter(|t| *t > 0) {
        Some(t) => {
            let pct = (done.min(t) * 100 / t) as usize;
            let fill = pct * W / 100;
            format!("[{}{}] {pct:>3} %  {:.1} / {:.1} MB", "#".repeat(fill), "-".repeat(W - fill), mb(done), mb(t))
        }
        None => format!("descargados {:.1} MB", mb(done)),
    }
}

pub fn run(args: &[String]) -> i32 {
    let opts = match parse_args(args) {
        Ok(Some(o)) => o,
        Ok(None) => {
            usage();
            return EXIT_OK;
        }
        Err(e) => {
            eprintln!("mish update: {e}");
            usage();
            return EXIT_ERR;
        }
    };

    if inside_app() {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b]7703;{}\x07", osc_payload(opts.check));
        let _ = out.flush();
        println!("{}", inside_app_msg(opts.check));
        return EXIT_OK;
    }

    let exe = std::env::current_exe().ok().map(|e| std::fs::canonicalize(&e).unwrap_or(e));
    let env_bundle = std::env::var("MISH_BUNDLE").ok();
    let home = std::env::var("HOME").ok();
    let Some(bundle) = find_bundle(env_bundle.as_deref(), exe.as_deref(), home.as_deref()) else {
        eprintln!("mish update: {}", upd::UpdError::SinBundle);
        return EXIT_ERR;
    };
    let cfg = upd::default_config(bundle);
    println!("Mish v{} ({})", cfg.current, cfg.bundle.display());

    let rel = match upd::check(&cfg) {
        Ok(None) => {
            println!("Estás al día (v{}).", cfg.current);
            return EXIT_OK;
        }
        Ok(Some(r)) => r,
        Err(e) => {
            eprintln!("mish update: {e}");
            return EXIT_ERR;
        }
    };
    println!("Hay una versión nueva: v{}", rel.version);
    for l in rel.notes.lines().map(str::trim).filter(|l| !l.is_empty()).take(8) {
        println!("  {l}");
    }
    if opts.check {
        println!("Ejecuta «mish update» para instalarla.");
        return EXIT_AVAILABLE;
    }

    let tty = unsafe { libc::isatty(2) } == 1;
    let mut last = Instant::now() - Duration::from_secs(1);
    let mut progress = |done: u64, total: Option<u64>| {
        let finished = total.is_some_and(|t| done >= t);
        if tty && (finished || last.elapsed() >= Duration::from_millis(100)) {
            last = Instant::now();
            eprint!("\r\x1b[2K{}", progress_line(done, total));
            let _ = std::io::stderr().flush();
        }
    };
    eprintln!("Descargando {}…", rel.zip.name);
    let dl = upd::download(&cfg, &rel, &mut progress);
    if tty {
        eprintln!();
    }
    let dl = match dl {
        Ok(d) => d,
        Err(e) => {
            eprintln!("mish update: {e}");
            return EXIT_ERR;
        }
    };
    eprintln!("Instalando…");
    match upd::install(&cfg, &dl) {
        Ok(()) => {
            println!("Actualizado a v{}. Reinicia Mish.app.", rel.version);
            EXIT_OK
        }
        Err(e) => {
            eprintln!("mish update: {e}");
            EXIT_ERR
        }
    }
}
