//! Genera (o reutiliza) el par Ed25519 de actualizaciones.
//!
//!   cargo run --example keygen -- <clave-privada.pem> <update-key.pub>
//!
//! - Si la privada no existe, la crea (PKCS#8 PEM, modo 0600, `create_new`).
//! - Si ya existe, no la toca: solo recalcula la pública.
//! - Escribe la pública (hex) en `<update-key.pub>`.
//! - Nunca imprime la clave privada.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;

use ed25519_dalek::SigningKey;
use ed25519_dalek::pkcs8::spki::der::pem::LineEnding;
use ed25519_dalek::pkcs8::{DecodePrivateKey, EncodePrivateKey};
use zeroize::Zeroize;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [pem_path, pub_path] = args.as_slice() else {
        eprintln!("uso: keygen <clave-privada.pem> <update-key.pub>");
        return ExitCode::from(2);
    };
    match run(Path::new(pem_path), Path::new(pub_path)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(pem_path: &Path, pub_path: &Path) -> Result<(), String> {
    let sk = if pem_path.exists() {
        let mode = fs::metadata(pem_path)
            .map_err(|e| e.to_string())?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} tiene permisos demasiado abiertos ({:o}); usa chmod 600",
                pem_path.display(),
                mode & 0o777
            ));
        }
        let pem = zeroize::Zeroizing::new(fs::read_to_string(pem_path).map_err(|e| e.to_string())?);
        let sk = SigningKey::from_pkcs8_pem(&pem)
            .map_err(|_| "la clave privada no es un PEM PKCS#8 Ed25519".to_string())?;
        eprintln!(
            "clave privada existente: {} (no se modifica)",
            pem_path.display()
        );
        sk
    } else {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| format!("sin aleatoriedad del sistema: {e}"))?;
        let sk = SigningKey::from_bytes(&seed);
        seed.zeroize();
        let pem = sk.to_pkcs8_pem(LineEnding::LF).map_err(|e| e.to_string())?;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pem_path)
            .map_err(|e| format!("no se pudo crear {}: {e}", pem_path.display()))?;
        f.write_all(pem.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        eprintln!("clave privada nueva: {} (0600)", pem_path.display());
        sk
    };
    let pubkey = sk.verifying_key().to_bytes();
    fs::write(pub_path, mish_updater::format::pubkey_file(&pubkey)).map_err(|e| e.to_string())?;
    eprintln!("clave pública escrita en {}", pub_path.display());
    println!("{}", mish_updater::format::to_hex(&pubkey));
    Ok(())
}
