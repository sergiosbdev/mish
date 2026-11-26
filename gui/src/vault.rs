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
//! - Touch ID (opcional, touchid.rs): al activarlo se guarda en el llavero de macOS
//!   `b"MISHB1" | salt | clave | estado del dominio biométrico`. Desbloquear con la huella
//!   lee esa clave y descifra el fichero sin Argon2. La clave guardada se invalida (se borra)
//!   si no descifra el fichero (contraseña cambiada o caja fuerte recreada), si el salt no
//!   coincide, si cambian las huellas registradas o al crear una caja fuerte nueva.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

use crate::touchid::{Bio, BioError, NoBio};

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
/// Cabecera de la clave guardada en el llavero para Touch ID.
const BIO_MAGIC: &[u8; 6] = b"MISHB1";
/// Textos del diálogo de Touch ID ("Mish quiere …").
const REASON_UNLOCK: &str = "desbloquear la caja fuerte";
const REASON_ENABLE: &str = "activar Touch ID para la caja fuerte";

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
    /// Touch ID cancelado (o "Usar contraseña maestra").
    BioCancel,
    /// Huella no reconocida o Touch ID bloqueado.
    BioFailed,
    /// Touch ID no disponible o no activado.
    BioUnavailable,
    /// La clave guardada ya no sirve y se ha borrado: hay que usar la contraseña.
    BioStale,
    /// El llavero de macOS no dio acceso a la clave.
    BioKeychain,
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
            VaultError::BioCancel => f.write_str("Touch ID cancelado. Usa la contraseña maestra."),
            VaultError::BioFailed => f.write_str("No se reconoció la huella. Usa la contraseña maestra."),
            VaultError::BioUnavailable => f.write_str("Touch ID no está disponible ahora mismo"),
            VaultError::BioStale => f.write_str(
                "Touch ID se ha desactivado (cambió la caja fuerte o las huellas). Desbloquea con la contraseña maestra para reactivarlo.",
            ),
            VaultError::BioKeychain => {
                f.write_str("El llavero de macOS no dio acceso a la clave. Usa la contraseña maestra.")
            }
        }
    }
}

impl From<BioError> for VaultError {
    fn from(e: BioError) -> Self {
        match e {
            BioError::Cancel => VaultError::BioCancel,
            BioError::Failed => VaultError::BioFailed,
            BioError::Unavailable => VaultError::BioUnavailable,
            BioError::NotFound => VaultError::BioStale,
            BioError::Keychain => VaultError::BioKeychain,
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

/// Salt y nonce de la cabecera de un fichero.
fn header(bytes: &[u8]) -> Result<([u8; SALT_LEN], [u8; NONCE_LEN])> {
    if bytes.len() < HEADER_LEN + 16 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(VaultError::Format);
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&bytes[MAGIC.len()..MAGIC.len() + SALT_LEN]);
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[MAGIC.len() + SALT_LEN..HEADER_LEN]);
    Ok((salt, nonce))
}

/// Descifra los bytes de un fichero con la contraseña dada.
fn decode(bytes: &[u8], pw: &[u8], kdf: &KdfParams) -> Result<Unlocked> {
    let (salt, nonce) = header(bytes)?;
    let key = derive_key(pw, &salt, kdf)?;
    open_with_key(bytes, key, salt, nonce)
}

/// Descifra con una clave ya derivada (contraseña o Touch ID).
fn open_with_key(
    bytes: &[u8],
    key: Zeroizing<[u8; KEY_LEN]>,
    salt: [u8; SALT_LEN],
    nonce: [u8; NONCE_LEN],
) -> Result<Unlocked> {
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

fn read_file(path: &Path) -> Result<Vec<u8>> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(VaultError::NotFound),
        Err(e) => return Err(e.into()),
    };
    if meta.len() > MAX_FILE {
        return Err(VaultError::Format);
    }
    Ok(fs::read(path)?)
}

/// Abre una caja fuerte existente. Lenta: deriva con Argon2.
pub fn unlock(path: &Path, pw: &[u8], kdf: &KdfParams) -> Result<Unlocked> {
    decode(&read_file(path)?, pw, kdf)
}

/// Abre con la clave guardada para Touch ID. `BioStale` si esa clave ya no corresponde al
/// fichero (salt distinto o no descifra: contraseña cambiada o caja fuerte recreada).
fn unlock_with_key(path: &Path, key: Zeroizing<[u8; KEY_LEN]>, salt: &[u8; SALT_LEN]) -> Result<Unlocked> {
    let bytes = read_file(path)?;
    let (file_salt, nonce) = header(&bytes)?;
    if &file_salt != salt {
        return Err(VaultError::BioStale);
    }
    open_with_key(&bytes, key, file_salt, nonce).map_err(|e| match e {
        VaultError::WrongPassword => VaultError::BioStale,
        e => e,
    })
}

/// Lo que se guarda en el llavero: `BIO_MAGIC | salt | clave | estado biométrico`.
fn bio_blob(key: &[u8; KEY_LEN], salt: &[u8; SALT_LEN], domain: &[u8]) -> Zeroizing<Vec<u8>> {
    let mut b = Zeroizing::new(Vec::with_capacity(BIO_MAGIC.len() + SALT_LEN + KEY_LEN + domain.len()));
    b.extend_from_slice(BIO_MAGIC);
    b.extend_from_slice(salt);
    b.extend_from_slice(key);
    b.extend_from_slice(domain);
    b
}

/// Clave, salt y estado biométrico guardados para Touch ID.
type BioParts<'a> = (Zeroizing<[u8; KEY_LEN]>, [u8; SALT_LEN], &'a [u8]);

fn parse_bio_blob(b: &[u8]) -> Option<BioParts<'_>> {
    let rest = b.strip_prefix(BIO_MAGIC.as_slice())?;
    if rest.len() < SALT_LEN + KEY_LEN {
        return None;
    }
    let mut salt = [0u8; SALT_LEN];
    salt.copy_from_slice(&rest[..SALT_LEN]);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    key.copy_from_slice(&rest[SALT_LEN..SALT_LEN + KEY_LEN]);
    Some((key, salt, &rest[SALT_LEN + KEY_LEN..]))
}

/// Resultado de un trabajo en segundo plano: estado desbloqueado (o nada) y, si cambió,
/// si queda una clave de Touch ID guardada.
type Work = (Result<Option<Unlocked>>, Option<bool>);

/// Desbloqueo con Touch ID (bloqueante: diálogo de la huella y quizá del llavero).
fn bio_unlock(bio: &dyn Bio, path: &Path, cancel: &AtomicBool) -> Work {
    let domain = match bio.authenticate(REASON_UNLOCK, cancel) {
        Ok(d) => d,
        Err(e) => return (Err(e.into()), None),
    };
    let blob = match bio.load() {
        Ok(b) => b,
        Err(BioError::NotFound) => return (Err(VaultError::BioStale), Some(false)),
        Err(e) => return (Err(e.into()), None),
    };
    let stale = || {
        bio.delete();
        (Err(VaultError::BioStale), Some(false))
    };
    let Some((key, salt, stored_domain)) = parse_bio_blob(&blob) else { return stale() };
    // Huellas añadidas o quitadas desde que se activó (equivale a BiometryCurrentSet).
    if !stored_domain.is_empty() && stored_domain != domain.as_slice() {
        return stale();
    }
    match unlock_with_key(path, key, &salt) {
        Ok(u) => (Ok(Some(u)), None),
        Err(VaultError::BioStale) => stale(),
        Err(e) => (Err(e), None),
    }
}

/// Resultado de una operación lenta (crear/desbloquear/Touch ID) hecha en otro hilo.
pub struct Done {
    op: u64,
    generation: u64,
    res: Result<Option<Unlocked>>,
    bio: Option<bool>,
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
    /// "unavailable" (sin Touch ID) | "off" | "on" (desbloqueo con huella activado).
    bio: &'static str,
    /// Hay un diálogo de Touch ID en curso.
    bio_pending: bool,
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
    /// Operación en segundo plano en curso (su `op`).
    busy: Option<u64>,
    op: u64,
    generation: u64,
    bio: Arc<dyn Bio>,
    /// Caché de `bio.available()` / `bio.exists()` (se refresca con `refresh_bio`).
    bio_avail: bool,
    bio_on: bool,
    /// Operación de Touch ID en curso y su señal de cancelación.
    bio_op: Option<(u64, Arc<AtomicBool>)>,
}

impl Vault {
    /// Caja fuerte real en `~/.config/mish/vault.bin` con costes de producción.
    pub fn default_location() -> Self {
        let path = std::env::var_os("HOME")
            .filter(|h| !h.is_empty())
            .map(|h| PathBuf::from(h).join(".config/mish/vault.bin"));
        Self::with(path, KdfParams::PROD, IDLE_LOCK).with_bio(crate::touchid::system())
    }

    pub fn with(path: Option<PathBuf>, kdf: KdfParams, idle: Duration) -> Self {
        Vault {
            path,
            kdf,
            idle,
            open: None,
            last_use: Instant::now(),
            busy: None,
            op: 0,
            generation: 0,
            bio: Arc::new(NoBio),
            bio_avail: false,
            bio_on: false,
            bio_op: None,
        }
    }

    pub fn with_bio(mut self, bio: Arc<dyn Bio>) -> Self {
        self.bio = bio;
        self
    }

    /// Vuelve a consultar si hay Touch ID y si hay clave guardada (rápido, sin diálogos).
    pub fn refresh_bio(&mut self) {
        self.bio_avail = self.bio.available();
        self.bio_on = self.bio.exists();
    }

    /// "unavailable" | "off" | "on".
    pub fn bio_state(&self) -> &'static str {
        if !self.bio_avail {
            "unavailable"
        } else if self.bio_on && self.state() != "none" {
            "on"
        } else {
            "off"
        }
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

    /// Bloquea: descarta (y borra de memoria) clave y entradas. Invalida operaciones en curso
    /// y cierra el diálogo de Touch ID si lo hay.
    pub fn lock(&mut self) {
        self.open = None;
        self.generation = self.generation.wrapping_add(1);
        self.cancel_bio();
    }

    /// Cierra el diálogo de Touch ID en curso (su resultado llegará como cancelado).
    pub fn cancel_bio(&mut self) {
        if let Some((_, c)) = &self.bio_op {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// Lanza `work` en otro hilo; `send` recibe el resultado (vía EventLoopProxy).
    fn start<F, W>(&mut self, send: F, work: W) -> u64
    where
        F: FnOnce(Done) + Send + 'static,
        W: FnOnce() -> Work + Send + 'static,
    {
        self.op = self.op.wrapping_add(1);
        let op = self.op;
        let generation = self.generation;
        self.busy = Some(op);
        std::thread::spawn(move || {
            let (res, bio) = work();
            send(Done { op, generation, res, bio });
        });
        op
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
        let path = self.path()?.to_path_buf();
        if creating {
            Self::validate_master(&pw)?;
            if path.exists() {
                return Err(VaultError::AlreadyExists);
            }
        } else if pw.is_empty() {
            return Err(VaultError::WrongPassword);
        }
        // La contraseña manda sobre un diálogo de Touch ID pendiente: se cierra y su
        // resultado se ignora (generación nueva).
        if let Some((op, c)) = self.bio_op.take() {
            c.store(true, Ordering::SeqCst);
            if self.busy == Some(op) {
                self.busy = None;
                self.generation = self.generation.wrapping_add(1);
            }
        }
        if self.busy.is_some() {
            return Err(VaultError::Busy);
        }
        let kdf = self.kdf;
        let bio = self.bio.clone();
        self.start(send, move || {
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
            // Caja fuerte nueva: una clave de Touch ID que quedara de otra ya no vale.
            let bio_now = if creating && res.is_ok() {
                bio.delete();
                Some(false)
            } else {
                None
            };
            (res.map(Some), bio_now)
        });
        Ok(true)
    }

    pub fn begin_create<F: FnOnce(Done) + Send + 'static>(&mut self, pw: Zeroizing<String>, send: F) -> Result<bool> {
        self.spawn(pw, true, send)
    }

    pub fn begin_unlock<F: FnOnce(Done) + Send + 'static>(&mut self, pw: Zeroizing<String>, send: F) -> Result<bool> {
        self.spawn(pw, false, send)
    }

    /// Desbloqueo con Touch ID en otro hilo. Ok(false) si ya está desbloqueada.
    pub fn begin_unlock_bio<F: FnOnce(Done) + Send + 'static>(&mut self, send: F) -> Result<bool> {
        if self.open.is_some() {
            return Ok(false);
        }
        if self.busy.is_some() {
            return Err(VaultError::Busy);
        }
        let path = self.path()?.to_path_buf();
        if !path.exists() {
            return Err(VaultError::NotFound);
        }
        if self.bio_state() != "on" {
            return Err(VaultError::BioUnavailable);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let (bio, c) = (self.bio.clone(), cancel.clone());
        let op = self.start(send, move || bio_unlock(&*bio, &path, &c));
        self.bio_op = Some((op, cancel));
        Ok(true)
    }

    /// Activa Touch ID: pide la huella y guarda en el llavero la clave que hay en memoria.
    /// Requiere la caja fuerte desbloqueada (con la contraseña o con la huella).
    pub fn begin_enable_bio<F: FnOnce(Done) + Send + 'static>(&mut self, send: F) -> Result<()> {
        let u = self.open.as_ref().ok_or(VaultError::Locked)?;
        if self.busy.is_some() {
            return Err(VaultError::Busy);
        }
        if !self.bio_avail {
            return Err(VaultError::BioUnavailable);
        }
        let (key, salt) = (u.key.clone(), u.salt);
        let cancel = Arc::new(AtomicBool::new(false));
        let (bio, c) = (self.bio.clone(), cancel.clone());
        let op = self.start(send, move || {
            let domain = match bio.authenticate(REASON_ENABLE, &c) {
                Ok(d) => d,
                Err(BioError::Unavailable) => return (Err(VaultError::BioUnavailable), None),
                Err(_) => return (Err(VaultError::Invalid("No se ha activado Touch ID")), None),
            };
            let blob = bio_blob(&key, &salt, &domain);
            drop(key);
            match bio.store(&blob) {
                Ok(()) => (Ok(None), Some(true)),
                Err(_) => (
                    Err(VaultError::Invalid("No se pudo guardar la clave en el llavero de macOS")),
                    Some(bio.exists()),
                ),
            }
        });
        self.bio_op = Some((op, cancel));
        self.touch();
        Ok(())
    }

    /// Desactiva Touch ID: borra la clave del llavero (en otro hilo: puede haber diálogo).
    pub fn begin_disable_bio<F: FnOnce(Done) + Send + 'static>(&mut self, send: F) -> Result<()> {
        if self.busy.is_some() {
            return Err(VaultError::Busy);
        }
        let bio = self.bio.clone();
        self.start(send, move || {
            bio.delete();
            (Ok(None), Some(bio.exists()))
        });
        Ok(())
    }

    /// Aplica el resultado de un `begin_*`. Ignora resultados obsoletos (se bloqueó entretanto).
    pub fn finish(&mut self, done: Done) -> Result<()> {
        if self.busy == Some(done.op) {
            self.busy = None;
        }
        if self.bio_op.as_ref().is_some_and(|(op, _)| *op == done.op) {
            self.bio_op = None;
        }
        // El estado del llavero es real aunque el resultado llegue obsoleto.
        if let Some(on) = done.bio {
            self.bio_on = on;
        }
        if done.generation != self.generation {
            return Ok(()); // `done.res` se descarta y se borra al soltarse
        }
        if let Some(u) = done.res? {
            self.open = Some(u);
        }
        if self.open.is_some() {
            self.touch();
        }
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
        let status = Status {
            state: self.state(),
            entries,
            bio: self.bio_state(),
            bio_pending: self.bio_op.is_some(),
            error,
        };
        // Si no se pudiera serializar, mishVault(null) cae a "locked".
        crate::js::call("mishVault", &status)
    }
}
