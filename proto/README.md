# mish-proto

Protocolo del socket Unix entre `mish-daemon` y sus clientes (GUI, hooks, ctl).
Crate independiente (sin workspace); se usa como `mish-proto = { path = "../proto" }`.

## Framing

```
[u32 BE len][u8 kind][payload]      len = 1 + payload.len() <= 4 MiB (MAX_FRAME)

1 CTL     JSON de Ctl, etiqueta "t" en snake_case
2 OUT     [u32 BE sid][bytes]   daemon → cliente (salida en vivo)
3 IN      [u32 BE sid][bytes]   cliente → daemon (teclado)
4 REPLAY  [u32 BE sid][bytes]   daemon → cliente (scrollback al hacer attach)
```

Un `len` > 4 MiB o 0 es fatal (cerrar la conexión). Kind desconocido, sid ausente
o CTL inválido dejan el flujo alineado: `is_recoverable(&err)`.

## Mensajes CTL

Cliente → daemon: `hello{proto,role:"gui"|"hook"|"ctl",build}`, `list`,
`attach{id,cols,rows}`, `detach{id}`, `new{id,cols,rows,cwd?,argv?}`,
`resize{id,cols,rows}`, `close{id}`, `kill_all`, `shutdown`,
`hook{session,event,ntype?,tool?,msg?}` (solo rol `hook`).

Daemon → cliente: `welcome{proto,build,pid,hostname}`, `sessions{list:[SessionInfo]}`,
`error{id?,msg}`, `exit{id}`, `meta{id,title?,cwd?,tint?}`, `replay_end{id}`,
`taken_over`, `claude{id,state,detail?}` (solo a la GUI).

`SessionInfo{id,pid,cwd?,title?,tint?,cols,rows,started,claude?}` (`started` en
segundos epoch; `claude` es el estado actual de Claude, ausente si es `none`).

### Hooks de Claude Code

Un cliente con `role:"hook"` solo puede mandar `list` y `hook`; cualquier otra
cosa recibe `error` y **nunca** escribe al PTY (los frames `IN` se descartan).

`{"t":"hook","session":3,"event":"Notification","ntype":"permission_prompt","tool":"Bash","msg":"…"}`

- `session`: `MISH_SESSION`. Si no existe, el daemon lo ignora sin responder.
- `event`: `hook_event_name` (≤ 32 caracteres). `ntype`: `notification_type`
  (≤ 64). `tool`: `tool_name` (≤ 64). Si alguno se pasa, el mensaje entero se
  **rechaza** al deserializar (error recuperable; el daemon lo descarta).
- `msg`: ≤ 200 caracteres; si se pasa se **trunca** a 200 al deserializar (el
  cliente debe truncarlo antes con `clamp_chars(s, MAX_HOOK_MSG)`).
- Los límites cuentan caracteres Unicode, no bytes. Constantes:
  `MAX_HOOK_EVENT`, `MAX_HOOK_NTYPE`, `MAX_HOOK_TOOL`, `MAX_HOOK_MSG`.
- Nunca se envían `tool_input`, el prompt ni el transcript (`deny_unknown_fields`
  rechaza campos extra).

`{"t":"claude","id":3,"state":"waiting","detail":"Bash"}`: el daemon lo manda a
la GUI conectada cuando cambia el estado. `state` ∈ `CLAUDE_STATES` =
`none|idle|working|waiting|done|error`.

| Evento | Estado |
|---|---|
| `SessionStart` | `idle` |
| `UserPromptSubmit`, `PreToolUse`, `PostToolUse` | `working` |
| `PermissionRequest`; `Notification` con `permission_prompt`, `elicitation_dialog` o `agent_needs_input` | `waiting` |
| `Stop`; `Notification` con `idle_prompt` | `done` |
| `StopFailure` | `error` |
| `SessionEnd` | `none` |
| otro | sin cambio |

Además, un OSC 7 nuevo de la shell (vuelve el prompt de mish) pone `none`.

Ejemplo: `{"t":"attach","id":3,"cols":120,"rows":40}`.

Reglas:
- `hello`, `welcome` y `shutdown` **no cambian nunca**; el resto se versiona con `PROTO` (hoy 1).
- `Ctl` rechaza campos y `t` desconocidos. `SessionInfo` ignora campos
  desconocidos (para poder añadirlos sin romper).
- Los opcionales ausentes son `None` y no se serializan.
- En Rust, las variantes sin datos son `Ctl::List {}`, `Ctl::KillAll {}`,
  `Ctl::Shutdown {}`, `Ctl::TakenOver {}` (así `deny_unknown_fields` también
  se aplica a ellas). `Ctl` es `#[non_exhaustive]`.

## Rutas

`state_dir()` = `$MISH_STATE_DIR` o `~/.local/state/mish`, con `daemon.sock`
(0600), `daemon.lock` (flock) y `daemon.log`. `ensure_state_dir()` lo crea
con 0700 y exige que no sea symlink, que el dueño sea `getuid()` y que
`mode & 0o077 == 0`.
