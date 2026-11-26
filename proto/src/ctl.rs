//! Mensajes de control (frames `CTL`, kind 1): JSON con etiqueta `"t"`.
//!
//! Ejemplo: `{"t":"attach","id":3,"cols":120,"rows":40}`.
//!
//! # Estabilidad
//! - [`Ctl::Hello`], [`Ctl::Welcome`] y [`Ctl::Shutdown`] **no cambian nunca**
//!   (ni nombre ni campos): son lo que permite negociar versión y apagar un
//!   daemon de otra versión.
//! - El resto puede evolucionar subiendo [`crate::PROTO`]. Añadir variantes
//!   nuevas (como `hook`/`claude` de la fase 4) es aditivo: el enum es
//!   `#[non_exhaustive]`, así que el código que hace `match` ya tiene un brazo
//!   `_` y no se rompe al compilar.
//! - Todas las variantes rechazan campos desconocidos (`deny_unknown_fields`).
//!   Por eso las variantes sin datos se declaran `List {}` en vez de `List`:
//!   serde ignora campos extra en variantes unitarias de enums con etiqueta
//!   interna. Constrúyelas como `Ctl::List {}` y haz `match` con `Ctl::List { .. }`.
//!   Un `t` desconocido o JSON inválido da un error recuperable de
//!   [`crate::read_frame`] (el flujo sigue sincronizado; ver
//!   [`crate::is_recoverable`]).

use serde::{Deserialize, Serialize};

/// Quién se conecta al daemon (campo `role` de `hello`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// La app Mish (una sola activa; la anterior recibe `taken_over`).
    Gui,
    /// Hooks de Claude Code (fase 4): solo cambian estado, nunca escriben al PTY.
    Hook,
    /// Herramientas de línea de comandos / tests.
    Ctl,
}

/// Descripción de una sesión viva (lista de `sessions`).
///
/// A diferencia de [`Ctl`], **ignora campos desconocidos** a propósito: así un
/// daemon más nuevo puede añadir información sin romper a clientes antiguos.
/// Los `Option` ausentes se leen como `None` y no se serializan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u32,
    /// PID del proceso hijo (la shell) de la sesión.
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Tinte de pestaña (p. ej. sesiones ssh), color CSS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<String>,
    pub cols: u16,
    pub rows: u16,
    /// Arranque de la sesión, segundos desde epoch.
    pub started: u64,
    /// Estado de Claude Code en la sesión (fase 4): uno de [`CLAUDE_STATES`]
    /// salvo `"none"`, que se representa como `None` (campo ausente).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude: Option<String>,
}

/// Mensaje de control. Se serializa como objeto JSON con `"t"` en snake_case.
///
/// Los `Option` ausentes se leen como `None`; al serializar, los `None` se
/// omiten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Ctl {
    // ---------------- cliente → daemon ----------------
    /// Primer mensaje del cliente. **Estable para siempre.**
    Hello {
        proto: u32,
        role: Role,
        /// Identificador de compilación (`MISH_BUILD`); distinto = aviso.
        build: String,
    },
    /// Pide la lista de sesiones; el daemon responde `sessions`.
    List {},
    /// Se engancha a una sesión: el daemon manda `REPLAY`…, `replay_end` y
    /// después `OUT` en vivo.
    Attach {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Deja de recibir salida de la sesión (sigue viva).
    Detach {
        id: u32,
    },
    /// Crea una sesión con el id dado (lo elige el cliente) y se engancha.
    New {
        id: u32,
        cols: u16,
        rows: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        argv: Option<Vec<String>>,
    },
    Resize {
        id: u32,
        cols: u16,
        rows: u16,
    },
    /// Mata la sesión (el daemon emite `exit`).
    Close {
        id: u32,
    },
    /// Mata todas las sesiones.
    KillAll {},
    /// Apaga el daemon (mata todas las sesiones). **Estable para siempre.**
    Shutdown {},
    /// Evento de un hook de Claude Code (solo con `role: "hook"`). Cambia el
    /// estado de Claude de la sesión; nunca escribe al PTY. El daemon ignora
    /// en silencio los `session` que no existen.
    ///
    /// Límites (en caracteres Unicode, no bytes), aplicados al deserializar:
    /// - `event` ≤ [`MAX_HOOK_EVENT`] (32), `ntype` ≤ [`MAX_HOOK_NTYPE`] (64) y
    ///   `tool` ≤ [`MAX_HOOK_TOOL`] (64): si se pasan, el mensaje se **rechaza**
    ///   (error recuperable, el daemon lo descarta).
    /// - `msg` ≤ [`MAX_HOOK_MSG`] (200): si se pasa, se **trunca** a 200
    ///   caracteres (el cliente debería truncarlo antes con [`clamp_chars`]).
    Hook {
        /// `MISH_SESSION` del proceso que lanzó el hook.
        session: u32,
        /// `hook_event_name` de Claude Code: `SessionStart`, `Stop`…
        #[serde(deserialize_with = "de_event")]
        event: String,
        /// `notification_type` (solo en `Notification`).
        #[serde(
            default,
            deserialize_with = "de_ntype",
            skip_serializing_if = "Option::is_none"
        )]
        ntype: Option<String>,
        /// `tool_name` (en `PreToolUse`, `PostToolUse`, `PermissionRequest`).
        #[serde(
            default,
            deserialize_with = "de_tool",
            skip_serializing_if = "Option::is_none"
        )]
        tool: Option<String>,
        /// Mensaje legible (p. ej. el de `Notification`), truncado.
        #[serde(
            default,
            deserialize_with = "de_msg",
            skip_serializing_if = "Option::is_none"
        )]
        msg: Option<String>,
    },

    // ---------------- daemon → cliente ----------------
    /// Respuesta a `hello`. **Estable para siempre.**
    Welcome {
        proto: u32,
        build: String,
        pid: u32,
        hostname: String,
    },
    Sessions {
        list: Vec<SessionInfo>,
    },
    /// Error; `id` es la sesión afectada si la hay.
    Error {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<u32>,
        msg: String,
    },
    /// La sesión terminó (su proceso salió o se cerró).
    Exit {
        id: u32,
    },
    /// Metadatos extraídos de la salida (OSC 0/2/7/7701).
    Meta {
        id: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tint: Option<String>,
    },
    /// Fin del replay de la sesión: a partir de aquí llegan `OUT`.
    ReplayEnd {
        id: u32,
    },
    /// Otra GUI se ha conectado; esta conexión se cerrará.
    TakenOver {},
    /// Cambio del estado de Claude Code en una sesión (solo a la GUI).
    /// `state` es uno de [`CLAUDE_STATES`]; `detail` es informativo
    /// (herramienta o mensaje de la notificación, ≤ 200 caracteres).
    Claude {
        id: u32,
        state: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

/// Estados posibles de Claude en `Ctl::Claude.state`.
pub const CLAUDE_STATES: [&str; 6] = ["none", "idle", "working", "waiting", "done", "error"];

/// Máximo de caracteres de `Ctl::Hook.event` (se rechaza si se pasa).
pub const MAX_HOOK_EVENT: usize = 32;
/// Máximo de caracteres de `Ctl::Hook.ntype` (se rechaza si se pasa).
pub const MAX_HOOK_NTYPE: usize = 64;
/// Máximo de caracteres de `Ctl::Hook.tool` (se rechaza si se pasa).
pub const MAX_HOOK_TOOL: usize = 64;
/// Máximo de caracteres de `Ctl::Hook.msg` (se trunca si se pasa).
pub const MAX_HOOK_MSG: usize = 200;

/// Corta `s` a como mucho `max` caracteres (Unicode), sin partir ninguno.
pub fn clamp_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_owned(),
        None => s.to_owned(),
    }
}

fn de_bounded<'de, D: serde::Deserializer<'de>>(
    d: D,
    max: usize,
    what: &str,
) -> Result<String, D::Error> {
    let s = String::deserialize(d)?;
    if s.chars().count() > max {
        return Err(serde::de::Error::custom(format!(
            "{what}: más de {max} caracteres"
        )));
    }
    Ok(s)
}

fn de_event<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    de_bounded(d, MAX_HOOK_EVENT, "event")
}

fn de_opt_bounded<'de, D: serde::Deserializer<'de>>(
    d: D,
    max: usize,
    what: &str,
) -> Result<Option<String>, D::Error> {
    match Option::<String>::deserialize(d)? {
        Some(s) if s.chars().count() > max => Err(serde::de::Error::custom(format!(
            "{what}: más de {max} caracteres"
        ))),
        o => Ok(o),
    }
}

fn de_ntype<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    de_opt_bounded(d, MAX_HOOK_NTYPE, "ntype")
}

fn de_tool<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    de_opt_bounded(d, MAX_HOOK_TOOL, "tool")
}

fn de_msg<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(Option::<String>::deserialize(d)?.map(|s| clamp_chars(&s, MAX_HOOK_MSG)))
}

impl Ctl {
    /// Serializa a JSON (sin el framing).
    pub fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(self)
    }

    /// Deserializa desde JSON (sin el framing). Rechaza campos y `t`
    /// desconocidos.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Ctl> {
        serde_json::from_slice(bytes)
    }
}
