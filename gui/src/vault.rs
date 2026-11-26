//! Caja fuerte de contraseñas de Mish.app.
//!
//! Fichero `~/.config/mish/vault.bin` (0600, escritura atómica):
//!
//! ```text
//! b"MISHV1" | salt (16 B) | nonce (24 B) | XChaCha20-Poly1305(serde_json(Vec<Entry>))
//! ```
//!
//! - Clave de 32 B derivada con Argon2id (m = 64 MiB, t = 3, p = 1) a partir de la
//!   contraseña maestra y el salt.
//! - AAD = cabecera mágica + salt: cualquier cambio en la cabecera, el salt, el nonce
//!   o el ciphertext hace fallar la autenticación.
//! - Diseño clave/salt: el salt se genera al crear la caja fuerte y la clave derivada
//!   se mantiene en memoria (Zeroizing) *junto con su salt* mientras está desbloqueada.
//!   Cada guardado usa un nonce aleatorio nuevo de 24 B (XChaCha: colisión despreciable)
//!   con esa misma clave/salt. Cambiar el salt en cada guardado obligaría a re-derivar
//!   con Argon2 (≈0,3-1 s por guardado) y, peor, a retener la contraseña maestra en
//!   memoria; reutilizar el salt de un mismo vault no debilita nada (el salt solo evita
//!   precomputación entre vaults/contraseñas), lo crítico es que el nonce no se repita.
//! - Los secretos nunca salen de este módulo salvo por `Vault::secret` (para escribirlos
//!   en el PTY o copiarlos); el estado que va al webview solo lleva nombres y usuarios.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 6] = b"MISHV1";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN;
/// Límite defensivo al leer el fichero.
const MAX_FILE: u64 = 16 * 1024 * 1024;
/// Bloqueo automático tras este tiempo sin uso.
pub const IDLE_LOCK: Duration = Duration::from_secs(5 * 60);
/// Retraso añadido ante una contraseña incorrecta.
pub const WRONG_PW_DELAY: Duration = Duration::from_millis(500);
const MIN_MASTER_LEN: usize = 8;

/// Costes de Argon2id. Producción: `KdfParams::PROD`; los tests usan costes mínimos.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    pub const PROD: KdfParams = KdfParams { m_kib: 64 * 1024, t: 3, p: 1 };
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub name: String,
    pub user: String,
    pub secret: Zeroizing<String>,
}

impl fmt::Debug for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Entry")
            .field("name", &self.name)
            .field("user", &self.user)
            .field("secret", &"<oculto>")
            .finish()
    }
}

/// Errores con mensajes genéricos: nunca contienen secretos, contraseñas ni rutas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// Fallo de autenticación AEAD: contraseña errónea o fichero manipulado
    /// (indistinguibles a propósito).
    WrongPassword,
    NotFound,
    AlreadyExists,
    Locked,
    NoEntry,
    Duplicate,
    Busy,
    NoHome,
    Format,
    Crypto,
    Io(io::ErrorKind),
    Invalid(&'static str),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::WrongPassword => f.write_str("Contraseña incorrecta"),
            VaultError::NotFound => f.write_str("No hay ninguna caja fuerte creada"),
            VaultError::AlreadyExists => f.write_str("La caja fuerte ya existe"),
            VaultError::Locked => f.write_str("La caja fuerte está bloqueada"),
            VaultError::NoEntry => f.write_str("No existe esa entrada"),
            VaultError::Duplicate => f.write_str("Ya existe una entrada con ese nombre"),
            VaultError::Busy => f.write_str("Operación en curso, espera un momento"),
            VaultError::NoHome => f.write_str("No se encuentra el directorio HOME"),
            VaultError::Format => f.write_str("El fichero de la caja fuerte no es válido"),
            VaultError::Crypto => f.write_str("Error interno de cifrado"),
            VaultError::Io(k) => write!(f, "Error de E/S en la caja fuerte ({k:?})"),
            VaultError::Invalid(m) => f.write_str(m),
        }
    }
}

impl From<io::Error> for VaultError {
    fn from(e: io::Error) -> Self {
        VaultError::Io(e.kind())
    }
}

type Result<T> = std::result::Result<T, VaultError>;

/// Estado desbloqueado: clave derivada + su salt + entradas en claro (todo Zeroizing).
pub struct Unlocked {
    key: Zeroizing<[u8; KEY_LEN]>,
    salt: [u8; SALT_LEN],
    entries: Vec<Entry>,
}

impl fmt::Debug for Unlocked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Unlocked").field("key", &"<oculta>").field("entries", &self.entries).finish()
    }
}

fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    // getrandom = la fuente del SO (lo mismo que envuelve OsRng).
    getrandom::fill(&mut b).map_err(|_| VaultError::Crypto)?;
    Ok(b)
}

pub fn derive_key(pw: &[u8], salt: &[u8; SALT_LEN], kdf: &KdfParams) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    let params = argon2::Params::new(kdf.m_kib, kdf.t, kdf.p, Some(KEY_LEN)).map_err(|_| VaultError::Crypto)?;
    let argon = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    argon.hash_password_into(pw, salt, &mut key[..]).map_err(|_| VaultError::Crypto)?;
    Ok(key)
}

fn aad(salt: &[u8; SALT_LEN]) -> [u8; MAGIC.len() + SALT_LEN] {
    let mut a = [0u8; MAGIC.len() + SALT_LEN];
    a[..MAGIC.len()].copy_from_slice(MAGIC);
    a[MAGIC.len()..].copy_from_slice(salt);
    a
}

/// Serializa y cifra con un nonce nuevo. Devuelve los bytes completos del fichero.
fn encode(key: &[u8; KEY_LEN], salt: &[u8; SALT_LEN], entries: &[Entry]) -> Result<Vec<u8>> {
    // Capacidad suficiente para el peor caso de escapado JSON (\u00XX = 6 B por byte):
    // así el buffer no se realoja y no quedan copias del texto en claro sin borrar.
    let cap = 2 + entries
        .iter()
        .map(|e| 48 + 6 * (e.name.len() + e.user.len() + e.secret.len()))
        .sum::<usize>();
    let mut pt = Zeroizing::new(Vec::with_capacity(cap));
    serde_json::to_writer(&mut *pt, entries).map_err(|_| VaultError::Crypto)?;

    let nonce: [u8; NONCE_LEN] = random()?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| VaultError::Crypto)?;
    let aad = aad(salt);
    let ct = cipher
        .encrypt(&XNonce::from(nonce), Payload { msg: &pt, aad: &aad })
        .map_err(|_| VaultError::Crypto)?;

    let mut out = Vec::with_capacity(HEADER_LEN + ct.len());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(salt);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Descifra los bytes de un fichero con la contraseña dada.
fn decode(bytes: &[u8], pw: &[u8], kdf: &KdfParams) -> Result<Unlocked> {
    if bytes.len() < HEADER_LEN + 16 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(VaultError::Format);
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + SALT_LEN]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[MAGIC.len() + SALT_LEN..HEADER_LEN]);

    let key = derive_key(pw, &salt, kdf)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key[..]).map_err(|_| VaultError::Crypto)?;
    let aad = aad(&salt);
    let pt = Zeroizing::new(
        cipher
            .decrypt(&XNonce::from(nonce), Payload { msg: &bytes[HEADER_LEN..], aad: &aad })
            .map_err(|_| VaultError::WrongPassword)?,
    );
    let entries: Vec<Entry> = serde_json::from_slice(&pt).map_err(|_| VaultError::Format)?;
    Ok(Unlocked { key, salt, entries })
}

/// Escritura atómica: tmp en el mismo directorio (0600) + fsync + rename + fsync del dir.
/// Crea el directorio con 0700 si falta (no cambia permisos de directorios existentes).
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let name = path.file_name().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let _ = fs::remove_file(&tmp); // restos de un guardado interrumpido

    let res = (|| {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)?;
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

pub fn save(path: &Path, u: &Unlocked) -> Result<()> {
    let bytes = encode(&u.key, &u.salt, &u.entries)?;
    write_atomic(path, &bytes)?;
    Ok(())
}

/// Crea una caja fuerte vacía (salt nuevo). Lenta: deriva con Argon2.
pub fn create(path: &Path, pw: &[u8], kdf: &KdfParams) -> Result<Unlocked> {
    if path.exists() {
        return Err(VaultError::AlreadyExists);
    }
    let salt: [u8; SALT_LEN] = random()?;
    let key = derive_key(pw, &salt, kdf)?;
    let u = Unlocked { key, salt, entries: Vec::new() };
    save(path, &u)?;
    Ok(u)
}

/// Abre una caja fuerte existente. Lenta: deriva con Argon2.
pub fn unlock(path: &Path, pw: &[u8], kdf: &KdfParams) -> Result<Unlocked> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(VaultError::NotFound),
        Err(e) => return Err(e.into()),
    };
    if meta.len() > MAX_FILE {
        return Err(VaultError::Format);
    }
    let bytes = fs::read(path)?;
    decode(&bytes, pw, kdf)
}

/// Resultado de una operación lenta (crear/desbloquear) hecha en otro hilo.
pub struct Done {
    generation: u64,
    res: Result<Unlocked>,
}

#[derive(Serialize)]
struct EntryView<'a> {
    name: &'a str,
    user: &'a str,
}

#[derive(Serialize)]
struct Status<'a> {
    state: &'static str,
    entries: Vec<EntryView<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

/// Gestor de estado (Locked/Unlocked) que vive en el hilo del event loop.
pub struct Vault {
    path: Option<PathBuf>,
    kdf: KdfParams,
    idle: Duration,
    open: Option<Unlocked>,
    last_use: Instant,
    busy: bool,
    generation: u64,
}

impl Vault {
    /// Caja fuerte real en `~/.config/mish/vault.bin` con costes de producción.
    pub fn default_location() -> Self {
        let path = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".config/mish/vault.bin"));
        Self::with(path, KdfParams::PROD, IDLE_LOCK)
    }

    pub fn with(path: Option<PathBuf>, kdf: KdfParams, idle: Duration) -> Self {
        Vault { path, kdf, idle, open: None, last_use: Instant::now(), busy: false, generation: 0 }
    }

    fn path(&self) -> Result<&Path> {
        self.path.as_deref().ok_or(VaultError::NoHome)
    }

    pub fn state(&self) -> &'static str {
        if self.open.is_some() {
            "unlocked"
        } else if self.path.as_deref().is_some_and(Path::exists) {
            "locked"
        } else {
            "none"
        }
    }

    fn touch(&mut self) {
        self.last_use = Instant::now();
    }

    /// Instante del bloqueo automático, si está desbloqueada.
    pub fn deadline(&self) -> Option<Instant> {
        self.open.as_ref().map(|_| self.last_use + self.idle)
    }

    /// Bloquea si se ha superado el tiempo sin uso. Devuelve true si acaba de bloquear.
    pub fn expire_if_idle(&mut self) -> bool {
        match self.deadline() {
            Some(d) if Instant::now() >= d => {
                self.lock();
                true
            }
            _ => false,
        }
    }

    /// Bloquea: descarta (y borra de memoria) clave y entradas. Invalida operaciones en curso.
    pub fn lock(&mut self) {
        self.open = None;
        self.generation = self.generation.wrapping_add(1);
    }

    fn validate_master(pw: &str) -> Result<()> {
        if pw.chars().count() < MIN_MASTER_LEN {
            return Err(VaultError::Invalid("La contraseña maestra debe tener al menos 8 caracteres"));
        }
        Ok(())
    }

    /// Lanza la derivación en otro hilo. `send` recibe el resultado (vía EventLoopProxy).
    /// Devuelve Ok(false) si no hay nada que hacer (ya desbloqueada).
    fn spawn<F>(&mut self, pw: Zeroizing<String>, creating: bool, send: F) -> Result<bool>
    where
        F: FnOnce(Done) + Send + 'static,
    {
        if self.open.is_some() {
            return Ok(false);
        }
        if self.busy {
            return Err(VaultError::Busy);
        }
        let path = self.path()?.to_path_buf();
        if creating {
            Self::validate_master(&pw)?;
            if path.exists() {
                return Err(VaultError::AlreadyExists);
            }
        } else if pw.is_empty() {
            return Err(VaultError::WrongPassword);
        }
        let kdf = self.kdf;
        let generation = self.generation;
        self.busy = true;
        std::thread::spawn(move || {
            let res = if creating {
                create(&path, pw.as_bytes(), &kdf)
            } else {
                unlock(&path, pw.as_bytes(), &kdf)
            };
            drop(pw);
            if matches!(res, Err(VaultError::WrongPassword)) {
                // ~500 ms extra (además de Argon2) ante contraseña errónea.
                std::thread::sleep(WRONG_PW_DELAY);
            }
            send(Done { generation, res });
        });
        Ok(true)
    }

    pub fn begin_create<F: FnOnce(Done) + Send + 'static>(&mut self, pw: Zeroizing<String>, send: F) -> Result<bool> {
        self.spawn(pw, true, send)
    }

    pub fn begin_unlock<F: FnOnce(Done) + Send + 'static>(&mut self, pw: Zeroizing<String>, send: F) -> Result<bool> {
        self.spawn(pw, false, send)
    }

    /// Aplica el resultado de un `begin_*`. Ignora resultados obsoletos (se bloqueó entretanto).
    pub fn finish(&mut self, done: Done) -> Result<()> {
        self.busy = false;
        if done.generation != self.generation {
            return Ok(()); // `done.res` se descarta y se borra al soltarse
        }
        let u = done.res?;
        self.open = Some(u);
        self.touch();
        Ok(())
    }

    /// Añade una entrada. Los nombres son únicos (sin distinguir mayúsculas): un nombre
    /// existente es un error
    /// (no se reemplaza; para cambiarla, borrar y volver a añadir).
    pub fn add(&mut self, name: &str, user: &str, secret: Zeroizing<String>) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(VaultError::Invalid("El nombre no puede estar vacío"));
        }
        if secret.is_empty() {
            return Err(VaultError::Invalid("La contraseña no puede estar vacía"));
        }
        let path = self.path()?.to_path_buf();
        let u = self.open.as_mut().ok_or(VaultError::Locked)?;
        if u.entries.iter().any(|e| e.name.to_lowercase() == name.to_lowercase()) {
            return Err(VaultError::Duplicate);
        }
        let entry = Entry { name: name.to_string(), user: user.trim().to_string(), secret };
        let pos = u
            .entries
            .iter()
            .position(|e| e.name.to_lowercase() > entry.name.to_lowercase())
            .unwrap_or(u.entries.len());
        u.entries.insert(pos, entry);
        if let Err(e) = save(&path, u) {
            u.entries.remove(pos);
            return Err(e);
        }
        self.touch();
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<()> {
        let path = self.path()?.to_path_buf();
        let u = self.open.as_mut().ok_or(VaultError::Locked)?;
        let pos = u.entries.iter().position(|e| e.name == name).ok_or(VaultError::NoEntry)?;
        let removed = u.entries.remove(pos);
        if let Err(e) = save(&path, u) {
            u.entries.insert(pos, removed);
            return Err(e);
        }
        self.touch();
        Ok(())
    }

    /// Copia del secreto (Zeroizing) para escribirlo en el PTY o el portapapeles.
    pub fn secret(&mut self, name: &str) -> Result<Zeroizing<String>> {
        let u = self.open.as_ref().ok_or(VaultError::Locked)?;
        let e = u.entries.iter().find(|e| e.name == name).ok_or(VaultError::NoEntry)?;
        let s = e.secret.clone();
        self.touch();
        Ok(s)
    }

    /// Script JS `window.mishVault({...})` con el estado. Solo nombres y usuarios.
    pub fn status_script(&self, error: Option<&str>) -> String {
        let entries = match &self.open {
            Some(u) => u.entries.iter().map(|e| EntryView { name: &e.name, user: &e.user }).collect(),
            None => Vec::new(),
        };
        let status = Status { state: self.state(), entries, error };
        // Si no se pudiera serializar, mishVault(null) cae a "locked".
        crate::js::call("mishVault", &status)
    }
}
