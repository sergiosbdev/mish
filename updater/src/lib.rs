//! `mish-updater`: actualiza Mish.app desde GitHub Releases.
//!
//! Flujo: [`check`] consulta `releases/latest` y devuelve la release si es más
//! nueva; [`download`] baja el zip a la caché y verifica sha256 **y** firma
//! Ed25519; [`install`] descomprime con `ditto`, valida el bundle
//! (`Info.plist` + `codesign`) y hace el cambio atómico con rollback.
//! Ver `README.md` para los formatos y el comando de firma de CI.

mod error;
pub mod format;
mod http;
mod install;

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub use error::UpdError;
pub use install::install;
pub use semver;

/// Tamaño máximo aceptado para el zip.
pub const MAX_ZIP: u64 = 200 * 1024 * 1024;
const MAX_API: u64 = 2 * 1024 * 1024;
const MAX_SMALL: u64 = 4 * 1024;

/// Repo por defecto (`owner/repo`), fijado al compilar con `MISH_UPDATE_REPO`.
pub const DEFAULT_REPO: &str = match option_env!("MISH_UPDATE_REPO") {
    Some(r) => r,
    None => "sergiosbdev/mish",
};

/// Clave pública embebida (hex en `update-key.pub`, decodificada al compilar).
/// Todo ceros = sin configurar: toda operación devuelve [`UpdError::SinClave`].
pub const EMBEDDED_PUBKEY: [u8; 32] = format::decode_pubkey_file(include_str!("../update-key.pub"));

#[derive(Debug, Clone)]
pub struct UpdConfig {
    pub owner: String,
    pub repo: String,
    /// `https://api.github.com` (sobreescribible en tests).
    pub api_base: String,
    pub current: Version,
    /// Ruta del `.app` instalado.
    pub bundle: PathBuf,
    pub pubkey: [u8; 32],
    /// `~/Library/Caches/mish/update`.
    pub cache_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    pub tag: String,
    pub notes: String,
    pub published: String,
    pub zip: Asset,
    pub sig: Asset,
    pub sha256: Asset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

/// Zip descargado y ya verificado (sha256 + firma). Solo lo crea [`download`].
#[derive(Debug, Clone)]
pub struct Downloaded {
    pub release: Release,
    /// Ruta del zip en `cache_dir`.
    pub zip: PathBuf,
    sig: [u8; format::SIG_LEN],
}

/// Versión de este build (versión única de mish/gui/daemon/updater).
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION es semver")
}

/// Configuración por defecto para el bundle indicado.
pub fn default_config(current_bundle: PathBuf) -> UpdConfig {
    let (owner, repo) = DEFAULT_REPO
        .split_once('/')
        .filter(|(o, r)| valid_name(o) && valid_name(r))
        .unwrap_or(("sergiosbdev", "mish"));
    UpdConfig {
        owner: owner.to_string(),
        repo: repo.to_string(),
        api_base: "https://api.github.com".to_string(),
        current: current_version(),
        bundle: current_bundle,
        pubkey: EMBEDDED_PUBKEY,
        cache_dir: default_cache_dir(),
    }
}

/// `~/Library/Caches/mish/update` (o el temporal del sistema sin `HOME`).
pub fn default_cache_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => PathBuf::from(h).join("Library/Caches/mish/update"),
        _ => std::env::temp_dir().join("mish-update"),
    }
}

/// Bundle `.app` que contiene a `exe` (`…/X.app/Contents/MacOS/bin`), si lo hay.
pub fn bundle_of(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let ok = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    ok.then(|| app.to_path_buf())
}

/// Bundle del ejecutable actual (`current_exe`, con symlinks resueltos).
pub fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    bundle_of(&exe)
}

fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn ensure_key(cfg: &UpdConfig) -> Result<(), UpdError> {
    if cfg.pubkey == [0u8; 32] {
        return Err(UpdError::SinClave);
    }
    Ok(())
}

fn agent_for(cfg: &UpdConfig) -> ureq::Agent {
    http::agent(cfg.api_base.starts_with("https://"))
}

/// Sufijos de arquitectura aceptables para este Mac, por preferencia: el nativo
/// y, si no hay, `universal`. Nunca se elige el de otra arquitectura.
pub fn arch_candidates() -> &'static [&'static str] {
    if cfg!(target_arch = "aarch64") {
        &["arm64", "universal"]
    } else {
        &["x86_64", "universal"]
    }
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Comprueba si hay una versión estable más nueva que `cfg.current`.
pub fn check(cfg: &UpdConfig) -> Result<Option<Release>, UpdError> {
    ensure_key(cfg)?;
    if !valid_name(&cfg.owner) || !valid_name(&cfg.repo) {
        return Err(UpdError::Config("owner/repo no válidos".into()));
    }
    let url = format!(
        "{}/repos/{}/{}/releases/latest",
        cfg.api_base.trim_end_matches('/'),
        cfg.owner,
        cfg.repo
    );
    let body = http::get_small(
        &agent_for(cfg),
        &url,
        "application/vnd.github+json",
        MAX_API,
    )?;
    parse_release(&body, &cfg.current, arch_candidates())
}

fn parse_release(
    json: &[u8],
    current: &Version,
    archs: &[&'static str],
) -> Result<Option<Release>, UpdError> {
    let gh: GhRelease = serde_json::from_slice(json)
        .map_err(|_| UpdError::Formato("respuesta de GitHub inesperada".into()))?;
    if gh.draft || gh.prerelease {
        return Ok(None);
    }
    let tag = gh.tag_name;
    let version = Version::parse(tag.strip_prefix('v').unwrap_or(&tag))
        .map_err(|_| UpdError::Formato("la etiqueta no es una versión semver".into()))?;
    if !version.pre.is_empty() || version <= *current {
        return Ok(None);
    }
    let assets: Vec<Asset> = gh
        .assets
        .into_iter()
        .map(|a| Asset {
            name: a.name,
            url: a.browser_download_url,
            size: a.size,
        })
        .collect();
    let zip = pick_zip(&assets, &version, archs)?;
    let find = |name: String| {
        assets
            .iter()
            .find(|a| a.name == name)
            .cloned()
            .ok_or(UpdError::FaltaAsset(name))
    };
    let sig = find(format!("{}.sig", zip.name))?;
    let sha256 = find(format!("{}.sha256", zip.name))?;
    for a in [&zip, &sig, &sha256] {
        if !(a.url.starts_with("https://") || a.url.starts_with("http://")) {
            return Err(UpdError::Formato("URL de descarga no válida".into()));
        }
    }
    Ok(Some(Release {
        version,
        tag,
        notes: gh.body.unwrap_or_default(),
        published: gh.published_at.unwrap_or_default(),
        zip,
        sig,
        sha256,
    }))
}

fn pick_zip(
    assets: &[Asset],
    version: &Version,
    archs: &[&'static str],
) -> Result<Asset, UpdError> {
    for arch in archs {
        let name = format!("Mish-{version}-macos-{arch}.zip");
        if let Some(a) = assets.iter().find(|a| a.name == name) {
            return Ok(a.clone());
        }
    }
    Err(UpdError::SinBinario {
        arch: archs.first().copied().unwrap_or("?"),
    })
}

fn safe_file_name(name: &str) -> bool {
    valid_name(name) && name.ends_with(".zip")
}

/// Borra el fichero al salir salvo que se desarme.
struct RmOnDrop(Option<PathBuf>);
impl Drop for RmOnDrop {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}

/// Descarga el zip de `rel` a `cfg.cache_dir` y lo verifica (tamaño, sha256 y
/// firma Ed25519). Si algo falla, borra lo descargado y no toca la app.
/// `progress(bytes_descargados, total)` se llama tras cada bloque.
pub fn download(
    cfg: &UpdConfig,
    rel: &Release,
    progress: &mut dyn FnMut(u64, Option<u64>),
) -> Result<Downloaded, UpdError> {
    ensure_key(cfg)?;
    if !safe_file_name(&rel.zip.name) {
        return Err(UpdError::Formato("nombre de fichero no válido".into()));
    }
    if rel.zip.size == 0 || rel.zip.size > MAX_ZIP {
        return Err(UpdError::Tamano);
    }
    let agent = agent_for(cfg);
    let sha_raw = http::get_small(
        &agent,
        &rel.sha256.url,
        "application/octet-stream",
        MAX_SMALL,
    )?;
    let expected = format::parse_sha256(&sha_raw, &rel.zip.name)?;
    let sig_raw = http::get_small(&agent, &rel.sig.url, "application/octet-stream", MAX_SMALL)?;
    let sig = format::parse_sig(&sig_raw)?;

    fs::create_dir_all(&cfg.cache_dir).map_err(|e| UpdError::io("crear la caché", e))?;
    let _ = fs::set_permissions(&cfg.cache_dir, fs::Permissions::from_mode(0o700));
    let final_path = cfg.cache_dir.join(&rel.zip.name);
    let part = cfg.cache_dir.join(format!("{}.part", rel.zip.name));
    let _ = fs::remove_file(&final_path);
    let _ = fs::remove_file(&part);
    let mut guard = RmOnDrop(Some(part.clone()));

    let total = rel.zip.size;
    let (len, mut reader) = http::open(&agent, &rel.zip.url, "application/octet-stream")?;
    if len.is_some_and(|l| l > total) {
        return Err(UpdError::Tamano);
    }
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&part)
        .map_err(|e| UpdError::io("crear la descarga", e))?;
    let mut hasher = Sha256::new();
    let mut done: u64 = 0;
    let mut buf = vec![0u8; 64 * 1024];
    progress(0, Some(total));
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(UpdError::Incompleta);
            }
            Err(e) => return Err(UpdError::Red(e.kind().to_string())),
        };
        done += n as u64;
        if done > total {
            return Err(UpdError::Tamano);
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| UpdError::io("escribir la descarga", e))?;
        progress(done, Some(total));
    }
    if done != total {
        return Err(UpdError::Incompleta);
    }
    file.sync_all()
        .map_err(|e| UpdError::io("escribir la descarga", e))?;
    drop(file);
    let got: [u8; 32] = hasher.finalize().into();
    if got != expected {
        return Err(UpdError::Sha);
    }
    verify_file(&part, &sig, &cfg.pubkey)?;
    fs::rename(&part, &final_path).map_err(|e| UpdError::io("guardar la descarga", e))?;
    guard.0 = None;
    Ok(Downloaded {
        release: rel.clone(),
        zip: final_path,
        sig,
    })
}

/// Lee el fichero (con tope) y verifica la firma.
fn verify_file(
    path: &Path,
    sig: &[u8; format::SIG_LEN],
    pubkey: &[u8; 32],
) -> Result<(), UpdError> {
    let f = fs::File::open(path).map_err(|e| UpdError::io("leer la descarga", e))?;
    let mut bytes = Vec::new();
    f.take(MAX_ZIP + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| UpdError::io("leer la descarga", e))?;
    if bytes.len() as u64 > MAX_ZIP {
        return Err(UpdError::Tamano);
    }
    format::verify(&bytes, sig, pubkey)
}
