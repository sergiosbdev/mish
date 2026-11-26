//! `mish-proto`: protocolo entre `mish-daemon` y sus clientes por socket Unix.
//!
//! - [`frame`]: framing `[u32 BE len][u8 kind][payload]`, máximo 4 MiB, con
//!   [`read_frame`]/[`write_frame`] bloqueantes.
//! - [`ctl`]: mensajes de control JSON ([`Ctl`]), etiquetados con `"t"`.
//! - [`paths`]: rutas de `~/.local/state/mish` y verificación del directorio.
//!
//! Secuencia típica de la GUI:
//! `hello` → `welcome`; `list` → `sessions`; `attach`/`new` → `REPLAY`… →
//! `replay_end` → `OUT` en vivo; `IN` para teclado; `resize`; `detach`.
//!
//! Versión: [`PROTO`]. `hello`, `welcome` y `shutdown` no cambian nunca, de
//! modo que un cliente siempre puede detectar un daemon incompatible y
//! apagarlo.

pub mod ctl;
pub mod frame;
pub mod paths;

pub use ctl::{
    CLAUDE_STATES, Ctl, MAX_HOOK_EVENT, MAX_HOOK_MSG, MAX_HOOK_NTYPE, MAX_HOOK_TOOL, Role,
    SessionInfo, clamp_chars,
};
pub use frame::{
    Frame, FrameError, KIND_CTL, KIND_IN, KIND_OUT, KIND_REPLAY, MAX_DATA, MAX_FRAME, encode_frame,
    is_closed, is_recoverable, read_frame, write_frame,
};
pub use paths::{
    STATE_DIR_ENV, ensure_private_dir, ensure_state_dir, lock_path, log_path, socket_path,
    state_dir,
};

/// Versión del protocolo. Se sube al hacer cambios incompatibles en algo que
/// no sea `hello`/`welcome`/`shutdown`.
pub const PROTO: u32 = 1;
