//! Anillo de salida por sesión (scrollback que se reenvía al hacer `attach`).
//!
//! Guarda como mucho `cap` bytes. Al recortar por delante, el corte se lleva
//! hasta justo después del siguiente `\n` para que el replay empiece en una
//! línea completa (y no a mitad de una secuencia de escape o de un carácter
//! UTF-8). Si no hay `\n` cerca (salida sin saltos de línea), se corta en el
//! mínimo necesario ajustado a un límite de carácter UTF-8.
//!
//! [`Ring::push`] devuelve los bytes recortados para que el llamante pueda
//! pasárselos al escáner de "estado base" (modos activos al inicio del anillo).

use std::collections::VecDeque;

/// Capacidad por sesión: 2 MiB.
pub const RING_CAP: usize = 2 * 1024 * 1024;

/// Distancia máxima que se busca un `\n` tras el punto de corte mínimo.
/// Acota el coste de recortar con salida que no tiene saltos de línea.
const NL_SCAN: usize = 64 * 1024;

pub struct Ring {
    buf: VecDeque<u8>,
    cap: usize,
}

impl Ring {
    pub fn new(cap: usize) -> Ring {
        Ring {
            buf: VecDeque::with_capacity(cap.min(64 * 1024)),
            cap: cap.max(1),
        }
    }

    /// Añade `data` y recorta por delante si se supera la capacidad.
    /// Devuelve los bytes descartados, en orden (vacío si no se recortó).
    pub fn push(&mut self, data: &[u8]) -> Vec<u8> {
        self.buf.extend(data);
        if self.buf.len() <= self.cap {
            return Vec::new();
        }
        let excess = self.buf.len() - self.cap;
        // Se busca desde excess-1: si justo ahí hay un '\n', basta con cortar en `excess`.
        let from = excess - 1;
        let limit = (excess + NL_SCAN).min(self.buf.len());
        let cut = match self.buf.range(from..limit).position(|&b| b == b'\n') {
            Some(p) => from + p + 1,
            None => {
                // Sin '\n' cerca: corte mínimo, avanzando hasta un inicio de carácter UTF-8.
                let mut c = excess;
                while c < self.buf.len() && (self.buf[c] & 0xC0) == 0x80 {
                    c += 1;
                }
                c
            }
        };
        self.buf.drain(..cut).collect()
    }

    /// Copia contigua del contenido actual.
    pub fn contents(&self) -> Vec<u8> {
        let (a, b) = self.buf.as_slices();
        let mut v = Vec::with_capacity(a.len() + b.len());
        v.extend_from_slice(a);
        v.extend_from_slice(b);
        v
    }
}
