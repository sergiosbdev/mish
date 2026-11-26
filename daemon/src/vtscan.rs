//! Escáner incremental de la salida del PTY.
//!
//! No es un emulador: solo sigue lo necesario para que un replay deje al
//! terminal del cliente en el estado correcto y para extraer metadatos.
//!
//! - Modos DEC privados (`CSI ? Pm h/l`): pantalla alternativa (1049/47/1047),
//!   seguimiento de ratón (1000/1002/1003, mutuamente excluyentes como en
//!   xterm), codificación SGR del ratón (1006), bracketed paste (2004),
//!   cursor de aplicación (1) y cursor visible (25). `ESC c` (RIS) los resetea.
//! - OSC 0/2 → título; OSC 7 → cwd (`file://host/ruta`, solo si el host es
//!   local: un servidor remoto no puede dirigir rutas locales); OSC 7701 →
//!   tinte (`bg=#rrggbb`; `reset` → ninguno).
//!
//! Es robusto a secuencias partidas entre lecturas: el estado del parser se
//! conserva entre llamadas a [`VtScan::feed`]. Los buffers de CSI y OSC están
//! acotados; una secuencia demasiado larga se descarta entera.

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

const CSI_MAX: usize = 64;
const OSC_MAX: usize = 4096;
const TITLE_MAX: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Esc,
    EscInter,
    Csi,
    Osc,
    OscEsc,
    /// DCS/SOS/PM/APC: se ignora hasta ST.
    Str,
    StrEsc,
}

/// Modos DEC que se restauran al hacer replay.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Modes {
    /// Pantalla alternativa activa y con qué modo se activó (1049, 1047 o 47).
    pub alt: Option<u16>,
    /// Seguimiento de ratón: 1000, 1002 o 1003.
    pub mouse: Option<u16>,
    /// Codificación SGR del ratón (1006).
    pub mouse_sgr: bool,
    /// Bracketed paste (2004).
    pub paste: bool,
    /// Teclas de cursor en modo aplicación (DECCKM, 1).
    pub app_cursor: bool,
    /// Cursor oculto (`?25l`).
    pub cursor_hidden: bool,
}

impl Modes {
    /// Secuencias que llevan un terminal recién creado a estos modos.
    pub fn prefix(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut set = |n: u16, on: bool| {
            out.extend_from_slice(format!("\x1b[?{n}{}", if on { 'h' } else { 'l' }).as_bytes());
        };
        if let Some(n) = self.alt {
            set(n, true);
        }
        if let Some(n) = self.mouse {
            set(n, true);
        }
        if self.mouse_sgr {
            set(1006, true);
        }
        if self.paste {
            set(2004, true);
        }
        if self.app_cursor {
            set(1, true);
        }
        if self.cursor_hidden {
            set(25, false);
        }
        out
    }

    fn set(&mut self, n: u16, on: bool) {
        match n {
            1 => self.app_cursor = on,
            25 => self.cursor_hidden = !on,
            47 | 1047 | 1049 => self.alt = if on { Some(n) } else { None },
            1000 | 1002 | 1003 => {
                // xterm: activar uno sustituye al anterior; desactivar cualquiera los apaga.
                self.mouse = if on { Some(n) } else { None };
            }
            1006 => self.mouse_sgr = on,
            2004 => self.paste = on,
            _ => {}
        }
    }
}

pub struct VtScan {
    state: State,
    csi: Vec<u8>,
    csi_bad: bool,
    osc: Vec<u8>,
    osc_bad: bool,
    modes: Modes,
    title: Option<String>,
    cwd: Option<String>,
    tint: Option<String>,
    /// Hostname local normalizado (para validar OSC 7).
    host: String,
    changed: bool,
    /// En el último `feed` hubo un OSC 7 local válido (aunque no cambiara el cwd).
    osc7: bool,
}

impl VtScan {
    /// `host`: nombre de esta máquina (se normaliza como en app.js).
    pub fn new(host: &str) -> VtScan {
        VtScan {
            state: State::Ground,
            csi: Vec::with_capacity(CSI_MAX),
            csi_bad: false,
            osc: Vec::new(),
            osc_bad: false,
            modes: Modes::default(),
            title: None,
            cwd: None,
            tint: None,
            host: norm_host(host),
            changed: false,
            osc7: false,
        }
    }

    pub fn modes(&self) -> &Modes {
        &self.modes
    }
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    pub fn tint(&self) -> Option<&str> {
        self.tint.as_deref()
    }
    /// `true` si el último [`VtScan::feed`] contenía un OSC 7 local válido,
    /// aunque repitiera el mismo cwd. mish lo emite en cada prompt, así que
    /// indica que la shell ha vuelto a tomar el control del terminal.
    pub fn saw_osc7(&self) -> bool {
        self.osc7
    }

    /// Procesa un trozo de salida. Devuelve `true` si cambió título, cwd o tinte.
    pub fn feed(&mut self, data: &[u8]) -> bool {
        self.changed = false;
        self.osc7 = false;
        let mut i = 0;
        while i < data.len() {
            if self.state == State::Ground {
                // Camino rápido: en texto normal solo interesa el próximo ESC.
                match data[i..].iter().position(|&b| b == ESC) {
                    Some(p) => {
                        i += p + 1;
                        self.state = State::Esc;
                        continue;
                    }
                    None => break,
                }
            }
            let b = data[i];
            i += 1;
            self.step(b);
        }
        self.changed
    }

    fn step(&mut self, b: u8) {
        use State::*;
        match self.state {
            Ground => {
                if b == ESC {
                    self.state = Esc;
                }
            }
            Esc => match b {
                b'[' => {
                    self.csi.clear();
                    self.csi_bad = false;
                    self.state = Csi;
                }
                b']' => {
                    self.osc.clear();
                    self.osc_bad = false;
                    self.state = Osc;
                }
                b'P' | b'X' | b'^' | b'_' => self.state = Str,
                b'c' => {
                    self.modes = Modes::default();
                    self.state = Ground;
                }
                ESC => {}
                CAN | SUB => self.state = Ground,
                0x20..=0x2f => self.state = EscInter,
                0x00..=0x1f => {} // C0 dentro de una secuencia: se ejecuta, no la corta
                _ => self.state = Ground,
            },
            EscInter => match b {
                ESC => self.state = Esc,
                CAN | SUB => self.state = Ground,
                0x00..=0x2f => {} // intermedios y C0
                _ => self.state = Ground,
            },
            Csi => match b {
                ESC => self.state = Esc,
                CAN | SUB => self.state = Ground,
                0x40..=0x7e => {
                    self.dispatch_csi(b);
                    self.state = Ground;
                }
                0x20..=0x3f => {
                    if self.csi.len() < CSI_MAX {
                        self.csi.push(b);
                    } else {
                        self.csi_bad = true;
                    }
                }
                _ => {} // C0 se ejecuta; DEL y bytes altos se ignoran
            },
            Osc => match b {
                BEL => {
                    self.dispatch_osc();
                    self.state = Ground;
                }
                ESC => self.state = OscEsc,
                CAN | SUB => self.state = Ground,
                _ => {
                    if self.osc.len() < OSC_MAX {
                        self.osc.push(b);
                    } else {
                        self.osc_bad = true;
                    }
                }
            },
            OscEsc => {
                // ESC termina el OSC (como xterm.js); si no es ST, el byte inicia otra secuencia.
                self.dispatch_osc();
                self.state = Esc;
                if b == b'\\' {
                    self.state = Ground;
                } else {
                    self.step(b);
                }
            }
            Str => match b {
                ESC => self.state = StrEsc,
                CAN | SUB => self.state = Ground,
                _ => {}
            },
            StrEsc => {
                self.state = Esc;
                if b == b'\\' {
                    self.state = Ground;
                } else {
                    self.step(b);
                }
            }
        }
    }

    fn dispatch_csi(&mut self, fin: u8) {
        if self.csi_bad || !(fin == b'h' || fin == b'l') {
            return;
        }
        let Some(params) = self.csi.strip_prefix(b"?") else {
            return;
        };
        // Solo parámetros numéricos y ';' (sin intermedios ni otros marcadores).
        if !params.iter().all(|&c| c.is_ascii_digit() || c == b';') {
            return;
        }
        let on = fin == b'h';
        for p in params.split(|&c| c == b';') {
            if p.is_empty() || p.len() > 5 {
                continue;
            }
            if let Ok(n) = std::str::from_utf8(p).unwrap_or("").parse::<u16>() {
                self.modes.set(n, on);
            }
        }
    }

    fn dispatch_osc(&mut self) {
        if self.osc_bad {
            return;
        }
        let osc = std::mem::take(&mut self.osc);
        let (ps, pt) = match osc.iter().position(|&c| c == b';') {
            Some(p) => (&osc[..p], &osc[p + 1..]),
            None => (&osc[..], &b""[..]),
        };
        match ps {
            b"0" | b"2" => {
                let t = sanitize_title(pt);
                self.set_title(t);
            }
            b"7" => {
                if let Some(cwd) = parse_osc7(pt, &self.host) {
                    self.osc7 = true;
                    if self.cwd.as_deref() != Some(cwd.as_str()) {
                        self.cwd = Some(cwd);
                        self.changed = true;
                    }
                }
            }
            b"7701" => {
                let new = if pt == b"reset" {
                    None
                } else {
                    match parse_tint(pt) {
                        Some(t) => Some(t),
                        None => return,
                    }
                };
                if self.tint != new {
                    self.tint = new;
                    self.changed = true;
                }
            }
            _ => {}
        }
        // Reutiliza la capacidad del buffer.
        self.osc = osc;
        self.osc.clear();
    }

    fn set_title(&mut self, t: Option<String>) {
        if self.title != t {
            self.title = t;
            self.changed = true;
        }
    }
}

/// Quita del replay las *consultas* al terminal (DSR `CSI 6n`, DA `CSI c`,
/// DECRQM, XTVERSION, informes de ventana `CSI 14t`…, consultas de color
/// `OSC 10;?`, lectura del portapapeles `OSC 52;c;?`, DECRQSS/XTGETTCAP).
///
/// Si se reenviaran, el terminal del cliente las respondería al reproducir el
/// scrollback y esas respuestas llegarían como teclado a la shell (basura del
/// tipo `^[[12;1R` en el prompt). Una secuencia incompleta al final se deja tal
/// cual: la salida en vivo la completará.
pub fn strip_queries(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let Some(p) = data[i..].iter().position(|&b| b == ESC) else {
            out.extend_from_slice(&data[i..]);
            break;
        };
        out.extend_from_slice(&data[i..i + p]);
        let start = i + p;
        match seq_end(data, start) {
            Some((end, drop)) => {
                if !drop {
                    out.extend_from_slice(&data[start..end]);
                }
                i = end;
            }
            None => {
                out.extend_from_slice(&data[start..]);
                break;
            }
        }
    }
    out
}

/// Fin (exclusivo) de la secuencia que empieza en `d[s] == ESC` y si es una
/// consulta. `None` si está incompleta.
fn seq_end(d: &[u8], s: usize) -> Option<(usize, bool)> {
    match *d.get(s + 1)? {
        b'[' => {
            let mut j = s + 2;
            loop {
                let c = *d.get(j)?;
                match c {
                    0x40..=0x7e => {
                        let drop = is_query_csi(&d[s + 2..j], c);
                        return Some((j + 1, drop));
                    }
                    ESC | CAN | SUB => return Some((j, false)), // abortada: se copia tal cual
                    _ => j += 1,
                }
            }
        }
        b']' => {
            let (body_end, end) = string_end(d, s + 2, true)?;
            Some((end, is_query_osc(&d[s + 2..body_end])))
        }
        b'P' => {
            let (body_end, end) = string_end(d, s + 2, false)?;
            let body = &d[s + 2..body_end];
            Some((end, body.starts_with(b"$q") || body.starts_with(b"+q")))
        }
        _ => Some((s + 1, false)),
    }
}

/// Busca el terminador de un OSC/DCS desde `from`: BEL (solo OSC) o `ESC \`.
/// Devuelve (fin del cuerpo, fin de la secuencia).
fn string_end(d: &[u8], from: usize, bel: bool) -> Option<(usize, usize)> {
    let mut j = from;
    loop {
        match *d.get(j)? {
            BEL if bel => return Some((j, j + 1)),
            CAN | SUB => return Some((j, j)),
            ESC => {
                let next = *d.get(j + 1)?;
                return Some((j, if next == b'\\' { j + 2 } else { j }));
            }
            _ => j += 1,
        }
    }
}

fn is_query_csi(params: &[u8], fin: u8) -> bool {
    let first = |p: &[u8]| -> Option<u16> {
        let n = p.split(|&c| c == b';').next()?;
        std::str::from_utf8(n).ok()?.parse().ok()
    };
    match fin {
        b'n' | b'c' => true,                     // DSR (5n/6n/?6n…) y DA (c, >c, =c)
        b'p' => params.contains(&b'$'),          // DECRQM
        b'q' => params.starts_with(b">"),        // XTVERSION
        b'u' => params.starts_with(b"?"),        // consulta del protocolo de teclado kitty
        b'S' | b'm' => params.starts_with(b"?"), // XTSMGRAPHICS / XTQMODKEYS
        b't' => matches!(
            first(params),
            Some(11 | 13 | 14 | 15 | 16 | 18 | 19 | 20 | 21)
        ),
        _ => false,
    }
}

fn is_query_osc(body: &[u8]) -> bool {
    let mut parts = body.split(|&c| c == b';');
    let ps = parts.next().unwrap_or_default();
    let rest: Vec<&[u8]> = parts.collect();
    match ps {
        b"52" => rest.get(1).is_some_and(|p| *p == b"?"),
        b"4" | b"5" | b"10" | b"11" | b"12" | b"13" | b"14" | b"15" | b"16" | b"17" | b"18"
        | b"19" => rest.iter().any(|p| *p == b"?"),
        _ => false,
    }
}

/// Normaliza un hostname como `normHost` de app.js: minúsculas, sin punto
/// final y sin sufijo `.local`.
pub fn norm_host(h: &str) -> String {
    let h = h.to_ascii_lowercase();
    let h = h.strip_suffix('.').unwrap_or(&h);
    h.strip_suffix(".local").unwrap_or(h).to_string()
}

fn sanitize_title(pt: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(pt);
    let t: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(TITLE_MAX)
        .collect();
    let t = t.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// `bg=#rrggbb[;title=…]` → `#rrggbb` en minúsculas.
fn parse_tint(pt: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(pt).ok()?;
    s.split(';').find_map(|kv| {
        let v = kv.strip_prefix("bg=")?;
        let hex = v.strip_prefix('#')?;
        (hex.len() == 6 && hex.bytes().all(|c| c.is_ascii_hexdigit()))
            .then(|| format!("#{}", hex.to_ascii_lowercase()))
    })
}

fn pct_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            let h = std::str::from_utf8(h).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Some(out)
}

/// `file://host/ruta%20codificada` → `/ruta codificada` si el host es local
/// (vacío, `localhost` o el hostname de esta máquina). Si no, `None`.
pub fn parse_osc7(pt: &[u8], local_host: &str) -> Option<String> {
    let rest = if pt.len() >= 7 && pt[..7].eq_ignore_ascii_case(b"file://") {
        &pt[7..]
    } else {
        return None;
    };
    let slash = rest.iter().position(|&c| c == b'/')?;
    let host = String::from_utf8(pct_decode(&rest[..slash])?).ok()?;
    let host = norm_host(&host);
    // El host puede llevar puerto en una URL; en OSC 7 no tiene sentido: se compara entero.
    if !(host.is_empty() || host == "localhost" || host == local_host) {
        return None;
    }
    let path = String::from_utf8(pct_decode(&rest[slash..])?).ok()?;
    if !path.starts_with('/') || path.contains('\0') || path.chars().any(|c| c.is_control()) {
        return None;
    }
    Some(path)
}
