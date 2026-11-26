//! Firma zips de release con el formato que verifica `mish-updater`.
//!
//!   cargo run --release --example sign -- --key-env MISH_UPDATE_KEY dist/Mish-1.2.3-macos-arm64.zip
//!   cargo run --release --example sign -- --key ~/.config/mish-export/update-key.pem <zip>...
//!
//! Por cada `<zip>` escribe, a su lado:
//! - `<zip>.sig`    firma Ed25519 en base64 (una línea).
//! - `<zip>.sha256` `<hex>  <nombre-del-zip>`.
//!
//! Antes de escribir comprueba que la clave corresponde a la pública embebida
//! (`updater/update-key.pub`) y que la firma verifica; si no, falla.
//! `--allow-other-key` desactiva lo primero (solo para pruebas).
//! La clave privada nunca se imprime ni se escribe en disco.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ed25519_dalek::pkcs8::DecodePrivateKey;
use ed25519_dalek::{Signer, SigningKey};
use mish_updater::format;
use zeroize::Zeroizing;

fn usage() -> ExitCode {
    eprintln!("uso: sign (--key <fichero.pem> | --key-env <VAR>) [--allow-other-key] <zip>...");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut pem: Option<Zeroizing<String>> = None;
    let mut allow_other = false;
    let mut zips: Vec<PathBuf> = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--key" => {
                let Some(p) = args.next() else { return usage() };
                match fs::read_to_string(&p) {
                    Ok(s) => pem = Some(Zeroizing::new(s)),
                    Err(e) => {
                        eprintln!("error: no se pudo leer la clave: {}", e.kind());
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--key-env" => {
                let Some(v) = args.next() else { return usage() };
                match std::env::var(&v) {
                    Ok(s) if !s.trim().is_empty() => pem = Some(Zeroizing::new(s)),
                    _ => {
                        eprintln!("error: la variable {v} no está definida o está vacía");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--allow-other-key" => allow_other = true,
            "-h" | "--help" => return usage(),
            _ if a.starts_with('-') => return usage(),
            _ => zips.push(PathBuf::from(a)),
        }
    }
    let (Some(pem), false) = (pem, zips.is_empty()) else {
        return usage();
    };
    let sk = match SigningKey::from_pkcs8_pem(pem.trim()) {
        Ok(k) => k,
        Err(_) => {
            eprintln!("error: la clave no es un PEM PKCS#8 Ed25519 válido");
            return ExitCode::FAILURE;
        }
    };
    drop(pem);
    let pubkey = sk.verifying_key().to_bytes();
    if pubkey != mish_updater::EMBEDDED_PUBKEY && !allow_other {
        eprintln!(
            "error: la clave privada no corresponde a updater/update-key.pub \
             (las apps instaladas rechazarían la actualización)"
        );
        return ExitCode::FAILURE;
    }
    for zip in &zips {
        if let Err(e) = sign_one(&sk, &pubkey, zip) {
            eprintln!("error: {}: {e}", zip.display());
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

fn sign_one(sk: &SigningKey, pubkey: &[u8; 32], zip: &Path) -> Result<(), String> {
    let name = zip
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("nombre de fichero no válido")?
        .to_string();
    let data = fs::read(zip).map_err(|e| e.kind().to_string())?;
    if data.len() as u64 > mish_updater::MAX_ZIP {
        return Err("supera el tamaño máximo que acepta el actualizador".into());
    }
    let sig = sk.sign(&data).to_bytes();
    format::verify(&data, &sig, pubkey).map_err(|e| e.to_string())?;
    let sig_txt = format::encode_sig(&sig);
    // Comprobación de ida y vuelta con el parser de la lib.
    if format::parse_sig(sig_txt.as_bytes()).ok() != Some(sig) {
        return Err("fallo interno codificando la firma".into());
    }
    let sha_txt = format::sha256_line(&format::sha256(&data), &name);
    let with_ext = |ext: &str| {
        let mut p = zip.as_os_str().to_owned();
        p.push(ext);
        PathBuf::from(p)
    };
    fs::write(with_ext(".sig"), sig_txt).map_err(|e| e.kind().to_string())?;
    fs::write(with_ext(".sha256"), sha_txt).map_err(|e| e.kind().to_string())?;
    println!("firmado: {name} (+ .sig, .sha256)");
    Ok(())
}
