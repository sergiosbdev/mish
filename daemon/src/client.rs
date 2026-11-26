//! Conexiones de clientes: comprobación de UID y un hilo lector + un hilo
//! escritor por conexión.
//!
//! El escritor recibe frames ya codificados por un canal acotado
//! ([`CLIENT_QUEUE`]). Si el canal se llena (cliente que no lee), el hub
//! desconecta al cliente; al reconectar, el replay recupera la salida.

use std::io::{self, BufReader, Write};
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};

use mish_proto::{is_recoverable, read_frame};

use crate::hub::Event;

/// Frames en cola por cliente antes de desconectarlo.
pub const CLIENT_QUEUE: usize = 512;

pub type Bytes = Arc<Vec<u8>>;

/// UID del otro extremo del socket.
#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
pub fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: fd válido mientras vive `s`; punteros a variables locales.
    let r = unsafe { libc::getpeereid(s.as_raw_fd(), &mut uid, &mut gid) };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
pub fn peer_uid(s: &UnixStream) -> io::Result<u32> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: fd válido; `cred` y `len` describen un buffer del tamaño correcto.
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if r != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

/// Arranca los hilos de una conexión. Devuelve el emisor hacia su escritor.
pub fn start(
    cid: u64,
    stream: &UnixStream,
    events: SyncSender<Event>,
) -> io::Result<SyncSender<Bytes>> {
    let rstream = stream.try_clone()?;
    let wstream = stream.try_clone()?;
    let (tx, rx) = mpsc::sync_channel::<Bytes>(CLIENT_QUEUE);
    std::thread::Builder::new()
        .name(format!("cli-w-{cid}"))
        .spawn(move || writer(wstream, rx))?;
    std::thread::Builder::new()
        .name(format!("cli-r-{cid}"))
        .spawn(move || reader(cid, rstream, events))?;
    Ok(tx)
}

fn writer(mut s: UnixStream, rx: Receiver<Bytes>) {
    // Termina cuando el hub suelta el emisor (tras vaciar la cola) o al fallar la escritura.
    for buf in rx {
        if s.write_all(&buf).is_err() {
            break;
        }
    }
    let _ = s.shutdown(Shutdown::Both);
}

fn reader(cid: u64, s: UnixStream, events: SyncSender<Event>) {
    let mut r = BufReader::with_capacity(64 * 1024, s);
    loop {
        match read_frame(&mut r) {
            Ok(frame) => {
                if events.send(Event::Frame { cid, frame }).is_err() {
                    return;
                }
            }
            Err(e) if is_recoverable(&e) => {
                if events
                    .send(Event::BadFrame {
                        cid,
                        why: e.to_string(),
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(e) => {
                let _ = events.send(Event::Gone {
                    cid,
                    why: e.to_string(),
                });
                return;
            }
        }
    }
}
