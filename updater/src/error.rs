//! Errores del actualizador. Los mensajes (`Display`) están en español y
//! pensados para mostrarse tal cual al usuario. Nunca incluyen claves ni
//! contenido descargado; como mucho, una ruta local cuando hace falta para
//! que el usuario pueda actuar (permisos, rollback fallido).

use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
pub enum UpdError {
    /// La app se compiló sin clave pública (placeholder de ceros).
    SinClave,
    /// owner/repo con caracteres no válidos.
    Config(String),
    /// Fallo de red/transporte (DNS, conexión, TLS, timeout, corte).
    Red(String),
    /// GitHub respondió con un estado HTTP inesperado.
    Http(u16),
    /// 403/429: límite de peticiones de la API de GitHub.
    Limite,
    /// 404 en releases/latest: no hay releases públicas (o el repo no es público).
    SinReleases,
    /// JSON o fichero auxiliar con formato inesperado.
    Formato(String),
    /// La release no trae zip para esta arquitectura.
    SinBinario { arch: &'static str },
    /// Falta un fichero obligatorio (.sig o .sha256) en la release.
    FaltaAsset(String),
    /// La descarga supera el tamaño anunciado o el máximo permitido.
    Tamano,
    /// La descarga terminó antes de tiempo.
    Incompleta,
    /// El sha256 no coincide.
    Sha,
    /// La firma Ed25519 no es válida (o no tiene formato válido).
    Firma,
    /// El zip no contiene una Mish.app válida con la versión esperada.
    Paquete(String),
    /// `codesign --verify` rechazó la app nueva.
    Codesign,
    /// La app está en App Translocation (abierta desde Descargas sin mover).
    Translocada,
    /// No se puede escribir en la carpeta que contiene la app.
    SinPermiso(PathBuf),
    /// No se encontró el bundle instalado.
    SinBundle,
    /// Error de E/S local.
    Io(String),
    /// El cambio de versión falló, pero la app anterior quedó restaurada.
    Instalacion(String),
    /// El cambio falló y tampoco se pudo restaurar: la app vieja está en `old`.
    RollbackFallido { old: PathBuf },
}

impl fmt::Display for UpdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use UpdError::*;
        match self {
            SinClave => write!(
                f,
                "esta copia de Mish no tiene clave de actualizaciones; actualiza manualmente"
            ),
            Config(m) => write!(f, "configuración de actualizaciones no válida: {m}"),
            Red(m) => write!(f, "no se pudo conectar con GitHub: {m}"),
            Http(s) => write!(f, "GitHub respondió con un error inesperado (HTTP {s})"),
            Limite => write!(
                f,
                "GitHub ha limitado las peticiones; vuelve a intentarlo dentro de un rato"
            ),
            SinReleases => write!(
                f,
                "no hay ninguna versión publicada (o el repositorio no es público)"
            ),
            Formato(m) => write!(f, "la información de la versión no es válida: {m}"),
            SinBinario { arch } => write!(
                f,
                "la versión nueva no incluye un binario para este Mac ({arch})"
            ),
            FaltaAsset(n) => write!(f, "a la versión publicada le falta el fichero {n}"),
            Tamano => write!(
                f,
                "la descarga es más grande de lo esperado; se ha descartado"
            ),
            Incompleta => write!(
                f,
                "la descarga se cortó antes de terminar; vuelve a intentarlo"
            ),
            Sha => write!(
                f,
                "la suma sha256 de la descarga no coincide; se ha descartado"
            ),
            Firma => write!(
                f,
                "la firma digital de la descarga no es válida; se ha descartado"
            ),
            Paquete(m) => write!(f, "el paquete descargado no es válido: {m}"),
            Codesign => write!(
                f,
                "la firma de código de la app nueva no es válida; no se ha instalado"
            ),
            Translocada => write!(
                f,
                "macOS está ejecutando Mish desde una ubicación temporal; mueve Mish.app a Aplicaciones, ábrela de nuevo y vuelve a intentarlo"
            ),
            SinPermiso(d) => write!(
                f,
                "no hay permiso de escritura en {}; mueve Mish.app a una carpeta tuya o actualiza manualmente",
                d.display()
            ),
            SinBundle => write!(f, "no se encontró la app instalada (Mish.app)"),
            Io(m) => write!(f, "error de disco: {m}"),
            Instalacion(m) => write!(
                f,
                "no se pudo instalar la versión nueva ({m}); se ha mantenido la versión actual"
            ),
            RollbackFallido { old } => write!(
                f,
                "la instalación falló y no se pudo restaurar la app; la versión anterior está en {}",
                old.display()
            ),
        }
    }
}

impl std::error::Error for UpdError {}

impl UpdError {
    pub(crate) fn io(op: &str, e: std::io::Error) -> Self {
        UpdError::Io(format!("{op}: {}", e.kind()))
    }
}
