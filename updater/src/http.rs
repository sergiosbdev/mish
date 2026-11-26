//! Cliente HTTP mínimo sobre ureq + rustls (sin OpenSSL).

use std::io::Read;
use std::time::Duration;

use crate::UpdError;

pub(crate) const USER_AGENT: &str = concat!("mish-updater/", env!("CARGO_PKG_VERSION"));

pub(crate) fn agent(https_only: bool) -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(USER_AGENT)
        .https_only(https_only)
        .http_status_as_error(false)
        .max_redirects(5)
        .timeout_resolve(Some(Duration::from_secs(15)))
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_send_request(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
}

pub(crate) fn red(e: ureq::Error) -> UpdError {
    match e {
        ureq::Error::Timeout(_) => UpdError::Red("tiempo de espera agotado".into()),
        ureq::Error::HostNotFound => UpdError::Red("no se encontró el servidor".into()),
        ureq::Error::ConnectionFailed => UpdError::Red("conexión rechazada".into()),
        ureq::Error::BodyExceedsLimit(_) => UpdError::Tamano,
        ureq::Error::TooManyRedirects | ureq::Error::RedirectFailed => {
            UpdError::Red("demasiadas redirecciones".into())
        }
        ureq::Error::RequireHttpsOnly(_) => {
            UpdError::Red("se intentó una descarga sin HTTPS".into())
        }
        ureq::Error::Io(e) => UpdError::Red(e.kind().to_string()),
        other => UpdError::Red(other.to_string()),
    }
}

fn status_err(s: u16) -> UpdError {
    match s {
        403 | 429 => UpdError::Limite,
        404 => UpdError::SinReleases,
        s => UpdError::Http(s),
    }
}

/// Abre un GET y devuelve el lector del cuerpo (sin límite: lo controla el llamador).
pub(crate) fn open(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
) -> Result<(Option<u64>, impl Read + use<>), UpdError> {
    let mut req = agent.get(url).header("Accept", accept);
    if url.starts_with("https://api.github.com/") {
        req = req.header("X-GitHub-Api-Version", "2022-11-28");
    }
    let resp = req.call().map_err(red)?;
    let status = resp.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(status_err(status));
    }
    let len = resp
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    Ok((len, resp.into_body().into_reader()))
}

/// GET de un recurso pequeño, con límite estricto de tamaño.
pub(crate) fn get_small(
    agent: &ureq::Agent,
    url: &str,
    accept: &str,
    limit: u64,
) -> Result<Vec<u8>, UpdError> {
    let (_, reader) = open(agent, url, accept)?;
    let mut buf = Vec::new();
    reader
        .take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|e| UpdError::Red(e.kind().to_string()))?;
    if buf.len() as u64 > limit {
        return Err(UpdError::Tamano);
    }
    Ok(buf)
}
