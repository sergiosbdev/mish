//! Framing binario sobre el socket Unix.
//!
//! ```text
//! [u32 BE len][u8 kind][payload]      len = 1 + payload.len() <= MAX_FRAME
//!
//! kind 1 CTL     payload = JSON de Ctl
//! kind 2 OUT     payload = [u32 BE sid][bytes]   daemon → cliente, salida en vivo
//! kind 3 IN      payload = [u32 BE sid][bytes]   cliente → daemon, teclado
//! kind 4 REPLAY  payload = [u32 BE sid][bytes]   daemon → cliente, scrollback
//! ```

use std::fmt;
use std::io::{self, Read, Write};

use crate::ctl::Ctl;

/// Tamaño máximo de `len` (kind + payload): 4 MiB.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;

/// Máximo de bytes de datos en un frame OUT/IN/REPLAY (`MAX_FRAME - kind - sid`).
pub const MAX_DATA: usize = MAX_FRAME - 1 - 4;

pub const KIND_CTL: u8 = 1;
pub const KIND_OUT: u8 = 2;
pub const KIND_IN: u8 = 3;
pub const KIND_REPLAY: u8 = 4;

/// Un frame decodificado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Ctl(Ctl),
    Out { sid: u32, data: Vec<u8> },
    In { sid: u32, data: Vec<u8> },
    Replay { sid: u32, data: Vec<u8> },
}

/// Motivo concreto de un error de protocolo. Va dentro del `io::Error`
/// (`e.get_ref()` + `downcast_ref::<FrameError>()`), o usa [`is_recoverable`].
#[derive(Debug)]
pub enum FrameError {
    /// El peer cerró la conexión limpiamente, en el límite entre frames.
    Closed,
    /// EOF a mitad de frame.
    Truncated,
    /// `len` > [`MAX_FRAME`] (al leer o al escribir). No recuperable al leer.
    TooLarge(usize),
    /// `len` == 0 (no hay byte de kind). No recuperable.
    Empty,
    /// Kind desconocido. Recuperable: el payload ya se consumió.
    UnknownKind(u8),
    /// Payload de OUT/IN/REPLAY con menos de 4 bytes. Recuperable.
    MissingSid,
    /// JSON de CTL inválido, con `t` o campos desconocidos. Recuperable.
    BadCtl(serde_json::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::Closed => write!(f, "conexión cerrada"),
            FrameError::Truncated => write!(f, "EOF a mitad de frame"),
            FrameError::TooLarge(n) => write!(f, "frame de {n} bytes > máximo {MAX_FRAME}"),
            FrameError::Empty => write!(f, "frame vacío (len 0)"),
            FrameError::UnknownKind(k) => write!(f, "kind de frame desconocido: {k}"),
            FrameError::MissingSid => write!(f, "frame de datos sin sid"),
            FrameError::BadCtl(e) => write!(f, "CTL inválido: {e}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FrameError::BadCtl(e) => Some(e),
            _ => None,
        }
    }
}

impl FrameError {
    fn into_io(self) -> io::Error {
        let kind = match self {
            FrameError::Closed | FrameError::Truncated => io::ErrorKind::UnexpectedEof,
            _ => io::ErrorKind::InvalidData,
        };
        io::Error::new(kind, self)
    }
}

/// `true` si tras este error de [`read_frame`] el flujo sigue alineado en un
/// límite de frame y se puede seguir leyendo (kind desconocido, CTL inválido,
/// frame de datos sin sid). Errores de E/S, EOF, frame vacío o demasiado
/// grande no son recuperables: hay que cerrar la conexión.
pub fn is_recoverable(e: &io::Error) -> bool {
    matches!(
        e.get_ref().and_then(|i| i.downcast_ref::<FrameError>()),
        Some(FrameError::UnknownKind(_) | FrameError::MissingSid | FrameError::BadCtl(_))
    )
}

/// `true` si el error indica que el peer cerró limpiamente entre frames.
pub fn is_closed(e: &io::Error) -> bool {
    matches!(
        e.get_ref().and_then(|i| i.downcast_ref::<FrameError>()),
        Some(FrameError::Closed)
    )
}

/// Codifica un frame completo (cabecera incluida) en un único buffer. Útil
/// para codificar una vez y enviar a varios clientes.
///
/// Error `InvalidData` si `len` superaría [`MAX_FRAME`] o si falla el JSON.
pub fn encode_frame(frame: &Frame) -> io::Result<Vec<u8>> {
    let (kind, sid, body): (u8, Option<u32>, std::borrow::Cow<'_, [u8]>) = match frame {
        Frame::Ctl(c) => {
            let json = c
                .to_json()
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            (KIND_CTL, None, json.into())
        }
        Frame::Out { sid, data } => (KIND_OUT, Some(*sid), data.as_slice().into()),
        Frame::In { sid, data } => (KIND_IN, Some(*sid), data.as_slice().into()),
        Frame::Replay { sid, data } => (KIND_REPLAY, Some(*sid), data.as_slice().into()),
    };
    let len = 1 + sid.map_or(0, |_| 4) + body.len();
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len).into_io());
    }
    let mut buf = Vec::with_capacity(4 + len);
    buf.extend_from_slice(&(len as u32).to_be_bytes());
    buf.push(kind);
    if let Some(sid) = sid {
        buf.extend_from_slice(&sid.to_be_bytes());
    }
    buf.extend_from_slice(&body);
    Ok(buf)
}

/// Escribe un frame (bloqueante) con un solo `write_all`. No hace `flush`
/// (innecesario sobre `UnixStream`; hazlo tú si usas un `BufWriter`).
///
/// Error `InvalidData` si el frame supera [`MAX_FRAME`]; en ese caso no se
/// escribe nada.
pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> io::Result<()> {
    let buf = encode_frame(frame)?;
    w.write_all(&buf)
}

/// Lee un frame completo (bloqueante).
///
/// Errores (con un [`FrameError`] dentro):
/// - `UnexpectedEof`: cierre limpio entre frames ([`FrameError::Closed`],
///   ver [`is_closed`]) o a mitad de frame ([`FrameError::Truncated`]).
/// - `InvalidData`: `len` > [`MAX_FRAME`] o 0 (sin leer el payload: cierra la
///   conexión), kind desconocido, sid ausente o CTL inválido (el payload se
///   consumió entero: ver [`is_recoverable`]).
/// - Cualquier otro error de E/S del lector.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Frame> {
    let mut hdr = [0u8; 4];
    read_header(r, &mut hdr)?;
    let len = u32::from_be_bytes(hdr) as usize;
    if len > MAX_FRAME {
        return Err(FrameError::TooLarge(len).into_io());
    }
    if len == 0 {
        return Err(FrameError::Empty.into_io());
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).map_err(eof_to_truncated)?;
    decode_body(buf[0], &buf[1..])
}

/// Lee la cabecera distinguiendo EOF limpio (0 bytes) de EOF a mitad.
fn read_header<R: Read>(r: &mut R, hdr: &mut [u8; 4]) -> io::Result<()> {
    let mut got = 0;
    while got < hdr.len() {
        match r.read(&mut hdr[got..]) {
            Ok(0) if got == 0 => return Err(FrameError::Closed.into_io()),
            Ok(0) => return Err(FrameError::Truncated.into_io()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

fn eof_to_truncated(e: io::Error) -> io::Error {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        FrameError::Truncated.into_io()
    } else {
        e
    }
}

fn decode_body(kind: u8, payload: &[u8]) -> io::Result<Frame> {
    let data_frame = |payload: &[u8]| -> io::Result<(u32, Vec<u8>)> {
        if payload.len() < 4 {
            return Err(FrameError::MissingSid.into_io());
        }
        let sid = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]);
        Ok((sid, payload[4..].to_vec()))
    };
    match kind {
        KIND_CTL => Ctl::from_json(payload)
            .map(Frame::Ctl)
            .map_err(|e| FrameError::BadCtl(e).into_io()),
        KIND_OUT => data_frame(payload).map(|(sid, data)| Frame::Out { sid, data }),
        KIND_IN => data_frame(payload).map(|(sid, data)| Frame::In { sid, data }),
        KIND_REPLAY => data_frame(payload).map(|(sid, data)| Frame::Replay { sid, data }),
        k => Err(FrameError::UnknownKind(k).into_io()),
    }
}
