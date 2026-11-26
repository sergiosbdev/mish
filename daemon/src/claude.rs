//! Estado de Claude Code por sesión, alimentado por los hooks (`Ctl::Hook`).
//!
//! | Evento | Estado |
//! |---|---|
//! | `SessionStart` | idle |
//! | `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | working |
//! | `PermissionRequest`; `Notification` con `permission_prompt`, `elicitation_dialog` o `agent_needs_input` | waiting |
//! | `Stop`; `Notification` con `idle_prompt` | done |
//! | `StopFailure` | error |
//! | `SessionEnd` | none |
//!
//! Cualquier otro evento (o `Notification` de otro tipo) no cambia nada.
//! Además, el hub vuelve a `none` cuando la shell emite un OSC 7 nuevo (ha
//! vuelto el prompt de mish, así que Claude ya no está en primer plano).

use mish_proto::{MAX_HOOK_MSG, clamp_chars};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    None,
    Idle,
    Working,
    Waiting,
    Done,
    Error,
}

impl State {
    /// Nombre en el protocolo (`Ctl::Claude.state`, `SessionInfo.claude`).
    pub fn as_str(self) -> &'static str {
        match self {
            State::None => "none",
            State::Idle => "idle",
            State::Working => "working",
            State::Waiting => "waiting",
            State::Done => "done",
            State::Error => "error",
        }
    }

    /// Para `SessionInfo.claude`: `none` se omite.
    pub fn info(self) -> Option<String> {
        (self != State::None).then(|| self.as_str().to_string())
    }

    /// Estados que merecen una notificación si no hay GUI.
    pub fn notifies(self) -> bool {
        matches!(self, State::Waiting | State::Done)
    }
}

/// Estado al que lleva un evento de hook, o `None` si no cambia nada.
pub fn next(event: &str, ntype: Option<&str>) -> Option<State> {
    Some(match event {
        "SessionStart" => State::Idle,
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => State::Working,
        "PermissionRequest" => State::Waiting,
        "Notification" => match ntype? {
            "permission_prompt" | "elicitation_dialog" | "agent_needs_input" => State::Waiting,
            "idle_prompt" => State::Done,
            _ => return None,
        },
        "Stop" => State::Done,
        "StopFailure" => State::Error,
        "SessionEnd" => State::None,
        _ => return None,
    })
}

/// Texto informativo para la GUI según el estado: la herramienta mientras
/// trabaja; el mensaje (o la herramienta) al esperar o fallar.
pub fn detail(state: State, tool: Option<&str>, msg: Option<&str>) -> Option<String> {
    let d = match state {
        State::Working => tool,
        State::Waiting | State::Error => msg.or(tool),
        State::Done => msg,
        State::None | State::Idle => None,
    }?;
    let d = d.trim();
    (!d.is_empty()).then(|| clamp_chars(d, MAX_HOOK_MSG))
}

/// Estado de Claude de una sesión.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Claude {
    pub state: State,
    pub detail: Option<String>,
}

impl Claude {
    /// Aplica un evento. Devuelve `true` si cambió el estado o el detalle.
    pub fn apply(
        &mut self,
        event: &str,
        ntype: Option<&str>,
        tool: Option<&str>,
        msg: Option<&str>,
    ) -> bool {
        let Some(state) = next(event, ntype) else {
            return false;
        };
        self.set(state, detail(state, tool, msg))
    }

    /// Reset por OSC 7 (vuelve el prompt). Devuelve `true` si cambió.
    pub fn reset(&mut self) -> bool {
        self.set(State::None, None)
    }

    fn set(&mut self, state: State, detail: Option<String>) -> bool {
        if self.state == state && self.detail == detail {
            return false;
        }
        self.state = state;
        self.detail = detail;
        true
    }
}
