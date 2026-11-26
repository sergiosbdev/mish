//! Verifica un zip de release igual que lo hará la app: `<zip>.sha256` y
//! `<zip>.sig` contra la clave pública embebida (`updater/update-key.pub`).
//!
//!   cargo run --release --example verify -- dist/Mish-1.2.3-macos-arm64.zip

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use mish_updater::{EMBEDDED_PUBKEY, format};

fn main() -> ExitCode {
    let zips: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if zips.is_empty() {
        eprintln!("uso: verify <zip>...");
        return ExitCode::from(2);
    }
    for zip in &zips {
        if let Err(e) = verify(zip) {
            eprintln!("error: {}: {e}", zip.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn verify(zip: &PathBuf) -> Result<(), String> {
    let name = zip
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("nombre no válido")?;
    let side = |ext: &str| {
        let mut p = zip.as_os_str().to_owned();
        p.push(ext);
        fs::read(PathBuf::from(p)).map_err(|e| format!("{ext}: {}", e.kind()))
    };
    let data = fs::read(zip).map_err(|e| e.kind().to_string())?;
    let expected = format::parse_sha256(&side(".sha256")?, name).map_err(|e| e.to_string())?;
    if format::sha256(&data) != expected {
        return Err("sha256 distinto".into());
    }
    let sig = format::parse_sig(&side(".sig")?).map_err(|e| e.to_string())?;
    format::verify(&data, &sig, &EMBEDDED_PUBKEY).map_err(|e| e.to_string())?;
    println!("ok: {name} (sha256 y firma Ed25519)");
    Ok(())
}
