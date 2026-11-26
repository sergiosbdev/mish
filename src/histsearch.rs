//! Búsqueda en el historial con Ctrl+R: un menú propio para reedline.
//!
//! El `ListMenu` de reedline solo filtra por una subcadena exacta y sensible a mayúsculas, no
//! tiene en cuenta lo que ya había escrito y al aceptar lo duplicaba (`git` + Ctrl+R + `push`
//! → `gitgit push…`). Este menú lo sustituye con la misma interfaz (`Menu`) y sigue usando el
//! historial del propio reedline (`ReedlineMenu::HistoryMenu`), así que ve también lo que se ha
//! ejecutado en esta sesión:
//!
//! - la consulta es la línea entera: lo que ya hubiera escrito más lo que se teclee después;
//! - varias palabras en cualquier orden y sin distinguir mayúsculas (`push git` → `git push …`);
//!   si nada coincide literalmente, vale con las letras en orden (`dcup` → `docker compose up`);
//! - primero la frase exacta, luego todas las palabras; las difusas solo si no hay literales;
//!   dentro de cada grupo, lo más reciente primero y sin repetidos;
//! - ↑↓ / Ctrl+R / Ctrl+S / Tab para moverse, Enter lo pone en la línea (no lo ejecuta), Esc cierra.

use crate::theme::Theme;
use nu_ansi_term::Style;
use reedline::{Completer, Editor, Menu, MenuBuilder, MenuEvent, MenuSettings, Painter, Span, Suggestion, UndoBehavior};
use std::collections::HashSet;
use unicode_width::UnicodeWidthChar;

/// Filas de resultados como máximo (y nunca más de media pantalla).
const MAX_ROWS: usize = 12;

// ---------- coincidencias (sin estado, probadas abajo) ----------

/// Minúsculas carácter a carácter, para que los índices sigan casando con el original.
fn lower(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

fn find(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > hay.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Letras de `needle` en orden dentro de `hay`, en la ventana más corta posible.
fn fuzzy(hay: &[char], needle: &[char]) -> Option<Vec<usize>> {
    let mut best: Option<Vec<usize>> = None;
    for start in 0..hay.len() {
        if hay[start] != needle[0] {
            continue;
        }
        let mut pos = vec![start];
        let mut j = 1;
        for (i, &c) in hay.iter().enumerate().skip(start + 1) {
            if j == needle.len() {
                break;
            }
            if c == needle[j] {
                pos.push(i);
                j += 1;
            }
        }
        if j < needle.len() {
            break; // si desde aquí no caben todas, empezando más adelante tampoco
        }
        let span = |p: &[usize]| p[p.len() - 1] - p[0];
        if best.as_deref().is_none_or(|b| span(&pos) < span(b)) {
            best = Some(pos);
        }
    }
    best
}

/// Un resultado: índice en la lista de entradas (0 = la más reciente) y caracteres que coinciden.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub idx: usize,
    pub positions: Vec<usize>,
}

/// Cómo casa una entrada con la consulta: (grupo, huecos, posiciones). Menor = mejor.
/// Grupo 0: la frase tal cual; 1: todas las palabras como subcadena; 2: alguna difusa.
fn score(entry: &[char], phrase: &[char], terms: &[Vec<char>]) -> Option<(u8, usize, Vec<usize>)> {
    if terms.is_empty() {
        return Some((0, 0, Vec::new()));
    }
    if let Some(p) = find(entry, phrase) {
        return Some((0, 0, (p..p + phrase.len()).collect()));
    }
    let mut tier = 1;
    let mut gaps = 0;
    let mut positions = Vec::new();
    for t in terms {
        if let Some(p) = find(entry, t) {
            positions.extend(p..p + t.len());
            continue;
        }
        // Una sola letra ya se ha buscado como subcadena; difusa solo a partir de dos.
        if t.len() < 2 {
            return None;
        }
        let pos = fuzzy(entry, t)?;
        let gap = pos[pos.len() - 1] - pos[0] + 1 - t.len();
        if gap > t.len() * 2 + 6 {
            return None; // letras demasiado desperdigadas: no es lo que se buscaba
        }
        tier = 2;
        gaps += gap;
        positions.extend(pos);
    }
    positions.sort_unstable();
    positions.dedup();
    Some((tier, gaps, positions))
}

/// Filtra y ordena `entries` (ya en minúsculas, la más reciente primero) para `query`.
pub fn rank(entries: &[Vec<char>], query: &str) -> Vec<Hit> {
    let phrase = lower(query.trim());
    let terms: Vec<Vec<char>> = query.split_whitespace().map(lower).collect();
    let mut scored: Vec<(u8, usize, Hit)> = entries
        .iter()
        .enumerate()
        .filter_map(|(idx, e)| score(e, &phrase, &terms).map(|(t, g, positions)| (t, g, Hit { idx, positions })))
        .collect();
    // Las difusas son el último recurso: con alguna coincidencia literal sobran (y con un
    // historial largo casi cualquier palabra tiene sus letras en orden en cientos de líneas).
    if scored.iter().any(|(t, _, _)| *t < 2) {
        scored.retain(|(t, _, _)| *t < 2);
    }
    // Estable: a igualdad de grupo y huecos queda el orden de recencia.
    scored.sort_by_key(|(t, g, _)| (*t, *g));
    scored.into_iter().map(|(_, _, h)| h).collect()
}

/// Quita repetidos (se queda con la aparición más reciente) y líneas vacías.
pub fn dedup(entries: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    entries.into_iter().filter(|e| !e.trim().is_empty() && seen.insert(e.clone())).collect()
}

/// Una entrada en una sola fila de `width` columnas, con las coincidencias resaltadas.
fn render_entry(entry: &str, positions: &[usize], width: usize, base: Style, hl: Style, ansi: bool) -> String {
    let mut out = String::new();
    let mut used = 0;
    let mut in_hl: Option<bool> = None;
    let chars: Vec<char> = entry.chars().collect();
    let mut pi = 0;
    for (i, &c) in chars.iter().enumerate() {
        let shown = match c {
            '\n' => '↵',
            '\t' => ' ',
            c if c.is_control() => continue,
            c => c,
        };
        let w = shown.width().unwrap_or(0);
        // Deja sitio para «…» si aún queda texto detrás.
        let rest_fits = used + w <= width && (i + 1 == chars.len() || used + w < width);
        if !rest_fits {
            if ansi {
                out.push_str("\x1b[0m");
                out.push_str(&base.prefix().to_string());
            }
            out.push('…');
            break;
        }
        while pi < positions.len() && positions[pi] < i {
            pi += 1;
        }
        let is_hl = pi < positions.len() && positions[pi] == i;
        if ansi && in_hl != Some(is_hl) {
            out.push_str("\x1b[0m");
            out.push_str(&(if is_hl { hl } else { base }).prefix().to_string());
            in_hl = Some(is_hl);
        }
        out.push(shown);
        used += w;
    }
    if ansi {
        out.push_str("\x1b[0m");
    }
    out
}

fn truncate(s: &str, width: usize) -> String {
    let mut used = 0;
    let mut out = String::new();
    for c in s.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            out.pop();
            out.push('…');
            break;
        }
        used += w;
        out.push(c);
    }
    out
}

// ---------- el menú ----------

pub struct HistorySearchMenu {
    settings: MenuSettings,
    active: bool,
    entries: Vec<String>,
    low: Vec<Vec<char>>,
    loaded: bool,
    /// Consulta con la que se calcularon `hits` (None = aún no se ha calculado).
    query: Option<String>,
    hits: Vec<Hit>,
    sel: usize,
    top: usize,
    rows_cap: usize,
    cols: usize,
    /// Lo que se pondrá en la línea al aceptar: siempre un valor mientras está abierto, para que
    /// Enter lo gestione el menú y nunca ejecute la consulta a medio escribir.
    value: Vec<Suggestion>,
    st_text: Style,
    st_sel: Style,
    st_hl: Style,
    st_muted: Style,
    st_accent: Style,
}

impl HistorySearchMenu {
    pub fn new(theme: &Theme) -> Self {
        Self {
            settings: MenuSettings::default().with_name("history_menu").with_marker("buscar: "),
            active: false,
            entries: Vec::new(),
            low: Vec::new(),
            loaded: false,
            query: None,
            hits: Vec::new(),
            sel: 0,
            top: 0,
            rows_cap: MAX_ROWS,
            cols: 80,
            value: Vec::new(),
            st_text: theme.text.s(),
            st_sel: theme.text.b(),
            st_hl: theme.accent.b().underline(),
            st_muted: theme.muted.s(),
            st_accent: theme.accent.b(),
        }
    }

    fn load(&mut self, completer: &mut dyn Completer) {
        // Con `HistoryMenu`, el completer es el del historial de reedline: "" = todo, lo más
        // reciente primero y ya sin repetidos (los volvemos a quitar por si acaso).
        let all = completer.complete("", 0);
        self.entries = dedup(all.suggestions().iter().map(|s| s.value.clone()));
        self.low = self.entries.iter().map(|e| lower(e)).collect();
        self.loaded = true;
    }

    fn refresh(&mut self, buffer: &str) {
        self.hits = rank(&self.low, buffer);
        self.query = Some(buffer.to_string());
        self.sel = 0;
        self.top = 0;
        self.update_value();
    }

    fn update_value(&mut self) {
        let q = self.query.clone().unwrap_or_default();
        let v = self.hits.get(self.sel).map_or(q.clone(), |h| self.entries[h.idx].clone());
        self.value = vec![Suggestion { value: v, span: Span::new(0, q.len()), ..Default::default() }];
    }

    fn visible_rows(&self) -> usize {
        self.hits.len().min(self.rows_cap)
    }

    /// Mueve la selección `delta` filas; `wrap` da la vuelta (↑↓), si no se queda en el borde
    /// y desplaza la vista lo mismo (página).
    fn step(&mut self, delta: isize, wrap: bool) {
        let n = self.hits.len() as isize;
        if n == 0 {
            return;
        }
        let rows = self.visible_rows().max(1);
        let mut s = self.sel as isize + delta;
        if wrap {
            s = s.rem_euclid(n);
        } else {
            s = s.clamp(0, n - 1);
            let max_top = (n - rows as isize).max(0);
            self.top = (self.top as isize + delta).clamp(0, max_top) as usize;
        }
        self.sel = s as usize;
        if self.sel < self.top {
            self.top = self.sel;
        } else if self.sel >= self.top + rows {
            self.top = self.sel + 1 - rows;
        }
        self.update_value();
    }
}

impl MenuBuilder for HistorySearchMenu {
    fn settings_mut(&mut self) -> &mut MenuSettings {
        &mut self.settings
    }
}

impl Menu for HistorySearchMenu {
    fn settings(&self) -> &MenuSettings {
        &self.settings
    }

    fn is_active(&self) -> bool {
        self.active
    }

    fn set_active(&mut self, active: bool) {
        self.active = active;
    }

    fn clear_input(&mut self) {}

    fn on_activate(&mut self) {
        self.loaded = false;
        self.query = None;
        self.hits.clear();
        self.sel = 0;
        self.top = 0;
        // Valor provisional hasta el primer repintado: Enter no hará nada raro.
        self.value = vec![Suggestion::default()];
    }

    fn on_deactivate(&mut self) {
        self.entries = Vec::new();
        self.low = Vec::new();
        self.hits = Vec::new();
        self.value.clear();
        self.loaded = false;
        self.query = None;
    }

    /// La navegación se resuelve aquí mismo (no necesita el editor), así un ↓ seguido de Enter
    /// en la misma ráfaga de teclas acepta la entrada correcta.
    fn menu_event(&mut self, event: MenuEvent) {
        self.handle_menu_event(&event);
        let page = self.visible_rows().max(1) as isize;
        match event {
            MenuEvent::NextElement | MenuEvent::MoveDown => self.step(1, true),
            MenuEvent::PreviousElement | MenuEvent::MoveUp => self.step(-1, true),
            MenuEvent::NextPage => self.step(page, false),
            MenuEvent::PreviousPage => self.step(-page, false),
            // ←/→ llegan aquí (las comparte el menú de autocompletar) y no se pueden pasar al
            // editor a tiempo para este repintado: se ignoran. Ctrl+A/Ctrl+E/⌫ sí editan.
            MenuEvent::MoveLeft | MenuEvent::MoveRight => {}
            // Abrir y editar necesitan el editor: se atienden en `update_working_details`,
            // que compara la línea con la última consulta.
            MenuEvent::Activate(_) | MenuEvent::Edit(_) | MenuEvent::Deactivate => {}
        }
    }

    fn can_quick_complete(&self) -> bool {
        false
    }

    fn can_partially_complete(&mut self, _: bool, _: &mut Editor, _: &mut dyn Completer) -> bool {
        false
    }

    fn update_values(&mut self, editor: &mut Editor, completer: &mut dyn Completer) {
        if !self.loaded {
            self.load(completer);
        }
        self.refresh(editor.get_buffer());
    }

    fn reset_position(&mut self) {
        self.sel = 0;
        self.top = 0;
    }

    fn update_working_details(&mut self, editor: &mut Editor, completer: &mut dyn Completer, painter: &Painter) {
        self.cols = painter.screen_width().max(10) as usize;
        self.rows_cap = MAX_ROWS.min((painter.screen_height() as usize / 2).saturating_sub(1)).max(3);
        if !self.loaded {
            self.load(completer);
        }
        // Cualquier cambio de la línea (teclear, borrar, deshacer…) es una consulta nueva.
        if self.query.as_deref() != Some(editor.get_buffer()) {
            self.refresh(editor.get_buffer());
        }
        // El tamaño de pantalla puede haber cambiado: que la selección siga a la vista.
        self.step(0, false);
    }

    fn replace_in_buffer(&self, editor: &mut Editor) {
        if !self.loaded {
            return; // abierto y aceptado sin llegar a pintar: se deja la línea como está
        }
        let buffer = editor.get_buffer();
        // Si la línea cambió después del último repintado (ráfaga de teclas), se recalcula.
        let value = if self.query.as_deref() == Some(buffer) {
            self.value.first().map(|s| s.value.clone())
        } else {
            rank(&self.low, buffer).first().map(|h| self.entries[h.idx].clone())
        };
        if let Some(v) = value {
            editor.edit_buffer(
                |lb| {
                    lb.set_buffer(v);
                    lb.move_to_end();
                },
                UndoBehavior::CreateUndoPoint,
            );
        }
    }

    fn menu_required_lines(&self, _terminal_columns: u16) -> u16 {
        (1 + self.visible_rows()) as u16
    }

    fn menu_string(&self, available_lines: u16, use_ansi_coloring: bool) -> String {
        let ansi = use_ansi_coloring;
        let paint = |st: Style, s: &str| if ansi { st.paint(s).to_string() } else { s.to_string() };
        let width = self.cols.saturating_sub(1);
        let q = self.query.as_deref().unwrap_or("").trim();
        let header = if !self.loaded {
            "  historial…".to_string()
        } else if self.entries.is_empty() {
            "  historial vacío   Esc cerrar".to_string()
        } else if self.hits.is_empty() {
            format!("  historial: nada coincide con «{q}»   Esc cerrar")
        } else {
            let n = if q.is_empty() {
                format!("{} comandos", self.hits.len())
            } else {
                format!("{}/{}", self.hits.len(), self.entries.len())
            };
            format!("  historial {n}   ↑↓ elegir · Enter a la línea · Esc cerrar")
        };
        let mut lines = vec![paint(self.st_muted, &truncate(&header, width))];
        let rows = self.visible_rows().min((available_lines as usize).saturating_sub(1));
        for (i, hit) in self.hits.iter().enumerate().skip(self.top).take(rows) {
            let selected = i == self.sel;
            let mark = if selected { paint(self.st_accent, "❯ ") } else { "  ".to_string() };
            let base = if selected { self.st_sel } else { self.st_text };
            let text = render_entry(&self.entries[hit.idx], &hit.positions, width.saturating_sub(2), base, self.st_hl, ansi);
            lines.push(format!("{mark}{text}"));
        }
        lines.join("\r\n")
    }

    fn min_rows(&self) -> u16 {
        2
    }

    fn get_values(&self) -> &[Suggestion] {
        &self.value
    }
}
