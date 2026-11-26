//! Prompt configurable: {cwd} {git} {host} {user} {estado} {tiempo} {vpn}

use crate::theme::Theme;
use reedline::{Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct MishPrompt {
    left: String,
    right: String,
}

pub struct Ctx {
    pub last_status: i32,
    pub last_duration: Duration,
}

impl MishPrompt {
    /// Glifos: sólo caracteres de SF Mono/Menlo sin variante emoji (✗ ∆ ▲ ×), para que
    /// Terminal no los pinte a color ni a doble ancho y el cursor no se desalinee.
    pub fn build(fmt: &str, ctx: &Ctx, t: &Theme, vpn_down: bool) -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("?"));
        let cwd_s = crate::parse::contract_home(&cwd.to_string_lossy());

        let git = crate::git::status(&cwd).map(|g| fmt_git(&g, t)).unwrap_or_default();
        let estado = if ctx.last_status != 0 {
            format!("{} ", t.err.b().paint(format!("✗ {}", ctx.last_status)))
        } else {
            String::new()
        };
        let tiempo = if ctx.last_duration >= Duration::from_secs(2) {
            format!("{} ", t.warn.s().paint(format!("∆ {}", fmt_dur(ctx.last_duration))))
        } else {
            String::new()
        };
        let vpn = if vpn_down { format!("{} ", t.warn.b().paint("▲ sin VPN")) } else { String::new() };

        let mut left = fmt
            .replace("{cwd}", &t.cwd.b().paint(cwd_s).to_string())
            .replace("{git}", &git)
            .replace("{host}", &t.server.s().paint(hostname()).to_string())
            .replace("{user}", &t.muted.s().paint(std::env::var("USER").unwrap_or_default()).to_string())
            .replace("{estado}", &estado)
            .replace("{tiempo}", &tiempo)
            .replace("{vpn}", &vpn);
        if !fmt.contains("{vpn}") {
            left = format!("{vpn}{left}");
        }
        let arrow = if ctx.last_status == 0 { t.accent } else { t.err };
        left = left.replace('❯', &arrow.b().paint("❯").to_string());

        let right = t.muted.s().paint(clock()).to_string();
        Self { left, right }
    }
}

fn fmt_git(g: &crate::git::GitStatus, t: &Theme) -> String {
    let mut s = t.git.s().paint(format!(" {}", g.branch)).to_string();
    let mut marks = Vec::new();
    if g.conflicts > 0 {
        marks.push(t.err.b().paint(format!("×{}", g.conflicts)).to_string());
    }
    if g.staged > 0 {
        marks.push(t.ok.s().paint(format!("+{}", g.staged)).to_string());
    }
    if g.modified > 0 {
        marks.push(t.git_dirty.s().paint(format!("!{}", g.modified)).to_string());
    }
    if g.untracked > 0 {
        marks.push(t.muted.s().paint(format!("?{}", g.untracked)).to_string());
    }
    if g.ahead > 0 {
        marks.push(t.git_sync.s().paint(format!("↑{}", g.ahead)).to_string());
    }
    if g.behind > 0 {
        marks.push(t.git_sync.s().paint(format!("↓{}", g.behind)).to_string());
    }
    if !marks.is_empty() {
        s.push(' ');
        s.push_str(&marks.join(" "));
    }
    s.push(' ');
    s
}

fn clock() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
    }
}

pub fn fmt_dur(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h{}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m{}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f32())
    }
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
    if !ok {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let h = String::from_utf8_lossy(&buf[..end]).to_string();
    h.split('.').next().unwrap_or("").to_string()
}

/// Rama git leyendo .git/HEAD directamente (rápido, sin lanzar git).
pub fn git_branch(start: &Path) -> Option<String> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let dotgit = d.join(".git");
        let gitdir = if dotgit.is_dir() {
            Some(dotgit)
        } else if dotgit.is_file() {
            std::fs::read_to_string(&dotgit)
                .ok()
                .and_then(|s| s.trim().strip_prefix("gitdir: ").map(|p| d.join(p)))
        } else {
            None
        };
        if let Some(g) = gitdir {
            let head = std::fs::read_to_string(g.join("HEAD")).ok()?;
            let head = head.trim();
            return Some(match head.strip_prefix("ref: refs/heads/") {
                Some(b) => b.to_string(),
                None => head.chars().take(7).collect(),
            });
        }
        dir = d.parent();
    }
    None
}

impl Prompt for MishPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.right)
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(&self, s: PromptHistorySearch) -> Cow<'_, str> {
        let prefix = match s.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "sin resultados ",
        };
        Cow::Owned(format!("({prefix}buscar: {}) ", s.term))
    }
}

/// Prompt compacto que queda en la pantalla tras ejecutar.
pub struct TransientPrompt {
    pub left: String,
}

impl TransientPrompt {
    pub fn new(t: &Theme) -> Self {
        Self { left: format!("{} ", t.accent.s().paint("❯")) }
    }
}

impl Prompt for TransientPrompt {
    fn render_prompt_left(&self) -> Cow<'_, str> {
        Cow::Borrowed(&self.left)
    }
    fn render_prompt_right(&self) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_indicator(&self, _mode: PromptEditMode) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
    fn render_prompt_multiline_indicator(&self) -> Cow<'_, str> {
        Cow::Borrowed("… ")
    }
    fn render_prompt_history_search_indicator(&self, _s: PromptHistorySearch) -> Cow<'_, str> {
        Cow::Borrowed("")
    }
}
