//! Formatos de los ficheros auxiliares de una release y de la clave pública.
//! Lo usa la lib para verificar y `examples/sign.rs` para generar, de modo
//! que ambos lados comparten exactamente el mismo formato.
//!
//! - `<zip>.sig`: firma Ed25519 *pura* (RFC 8032, no prehash) de los bytes
//!   del zip, codificada en **base64 estándar con relleno** (88 caracteres)
//!   en una línea, con `\n` final opcional. Por compatibilidad también se
//!   aceptan 64 bytes en crudo.
//! - `<zip>.sha256`: `<64 hex>  <nombre-del-zip>\n` (la salida de
//!   `shasum -a 256 <nombre>` ejecutado en el directorio del zip). El nombre
//!   es opcional; si está, debe coincidir (se ignora un `*` inicial y la ruta).
//! - `update-key.pub`: 32 bytes de la clave pública en hex (64 caracteres).
//!   Se ignoran espacios y líneas que empiezan por `#`.

use base64::Engine as _;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256};

use crate::UpdError;

pub const SIG_LEN: usize = 64;

/// Codifica una firma tal y como se publica en el `.sig`.
pub fn encode_sig(sig: &[u8; SIG_LEN]) -> String {
    let mut s = base64::engine::general_purpose::STANDARD.encode(sig);
    s.push('\n');
    s
}

/// Lee el contenido de un `.sig` (base64 o 64 bytes en crudo).
pub fn parse_sig(raw: &[u8]) -> Result<[u8; SIG_LEN], UpdError> {
    if raw.len() == SIG_LEN {
        let mut out = [0u8; SIG_LEN];
        out.copy_from_slice(raw);
        return Ok(out);
    }
    let txt = std::str::from_utf8(raw)
        .map_err(|_| UpdError::Firma)?
        .trim();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(txt)
        .map_err(|_| UpdError::Firma)?;
    bytes.try_into().map_err(|_| UpdError::Firma)
}

/// Línea del `.sha256` para un zip.
pub fn sha256_line(digest: &[u8; 32], name: &str) -> String {
    format!("{}  {name}\n", to_hex(digest))
}

/// Lee el `.sha256` y devuelve el digest esperado para `zip_name`.
pub fn parse_sha256(raw: &[u8], zip_name: &str) -> Result<[u8; 32], UpdError> {
    let txt =
        std::str::from_utf8(raw).map_err(|_| UpdError::Formato("el .sha256 no es texto".into()))?;
    let mut it = txt.split_whitespace();
    let hex = it
        .next()
        .ok_or_else(|| UpdError::Formato("el .sha256 está vacío".into()))?;
    let digest = from_hex::<32>(hex)
        .ok_or_else(|| UpdError::Formato("el .sha256 no contiene un hash válido".into()))?;
    if let Some(name) = it.next() {
        let name = name.trim_start_matches('*');
        let base = name.rsplit('/').next().unwrap_or(name);
        if base != zip_name {
            return Err(UpdError::Formato(
                "el .sha256 corresponde a otro fichero".into(),
            ));
        }
    }
    Ok(digest)
}

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

/// Verificación estricta (`verify_strict`) de la firma de `msg`.
pub fn verify(msg: &[u8], sig: &[u8; SIG_LEN], pubkey: &[u8; 32]) -> Result<(), UpdError> {
    let key = VerifyingKey::from_bytes(pubkey).map_err(|_| UpdError::SinClave)?;
    if key.is_weak() {
        return Err(UpdError::SinClave);
    }
    let sig = Signature::from_bytes(sig);
    key.verify_strict(msg, &sig).map_err(|_| UpdError::Firma)
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn from_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.as_bytes();
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in s.as_chunks::<2>().0.iter().enumerate() {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        out[i] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

/// Texto del fichero `update-key.pub` para una clave pública.
pub fn pubkey_file(pubkey: &[u8; 32]) -> String {
    format!(
        "# Clave pública Ed25519 de las actualizaciones de Mish (32 bytes en hex).\n\
         # La genera app/gen-update-key.sh; la privada NUNCA va en el repo.\n{}\n",
        to_hex(pubkey)
    )
}

const fn hex_nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("update-key.pub: carácter no hexadecimal"),
    }
}

/// Decodifica `update-key.pub` en tiempo de compilación (un fichero mal
/// formado es un error de compilación, no un fallo en ejecución).
pub const fn decode_pubkey_file(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut nibbles = 0usize;
    let mut i = 0usize;
    let mut comment = false;
    let mut line_start = true;
    while i < b.len() {
        let c = b[i];
        i += 1;
        if c == b'\n' {
            comment = false;
            line_start = true;
            continue;
        }
        if comment {
            continue;
        }
        if line_start && c == b'#' {
            comment = true;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            continue;
        }
        line_start = false;
        if nibbles >= 64 {
            panic!("update-key.pub: más de 32 bytes");
        }
        let v = hex_nibble(c);
        if nibbles.is_multiple_of(2) {
            out[nibbles / 2] = v << 4;
        } else {
            out[nibbles / 2] |= v;
        }
        nibbles += 1;
    }
    if nibbles != 64 {
        panic!("update-key.pub: se esperaban 64 caracteres hex");
    }
    out
}
