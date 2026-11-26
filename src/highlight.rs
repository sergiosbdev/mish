//! Resaltado de la línea mientras se escribe.

use crate::State;
use nu_ansi_term::Style;
use reedline::{Highlighter, StyledText};
use std::sync::{Arc, Mutex};

pub struct MishHighlighter {
    pub state: Arc<Mutex<State>>,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Kind {
    Space,
    Word,
    Str,
    Var,
    Op,
    Comment,
}

/// Separa la línea en trozos (con posiciones) sin perder nada del texto.
fn lex(line: &str) -> Vec<(Kind, &str)> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        let kind = if c == b' ' || c == b'\t' {
            while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
                i += 1;
            }
            Kind::Space
        } else if c == b'\'' || c == b'"' {
            i += 1;
            while i < b.len() && b[i] != c {
                if b[i] == b'\\' && c == b'"' {
                    i += 1;
                }
                i += 1;
            }
            i = (i + 1).min(b.len());
            Kind::Str
        } else if c == b'$' {
            i += 1;
            if i < b.len() && (b[i] == b'(' || b[i] == b'{') {
                let close = if b[i] == b'(' { b')' } else { b'}' };
                while i < b.len() && b[i] != close {
                    i += 1;
                }
                i = (i + 1).min(b.len());
            } else {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                    i += 1;
                }
            }
            Kind::Var
        } else if b"|&;<>".contains(&c) {
            while i < b.len() && b"|&;<>".contains(&b[i]) {
                i += 1;
            }
            Kind::Op
        } else if c == b'#' && (start == 0 || b[start - 1] == b' ') {
            i = b.len();
            Kind::Comment
        } else {
            while i < b.len() && !b" \t'\"$|&;<>".contains(&b[i]) {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i = i.min(b.len());
            Kind::Word
        };
        // Evitar cortar en medio de un carácter UTF-8.
        while i < b.len() && !line.is_char_boundary(i) {
            i += 1;
        }
        out.push((kind, &line[start..i]));
    }
    out
}

impl Highlighter for MishHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let st = self.state.lock().unwrap();
        let t = &st.theme;
        let mut out = StyledText::new();
        let mut expect_cmd = true;
        let mut cmd_name = String::new();
        let mut arg_idx = 0;

        for (kind, text) in lex(line) {
            let style = match kind {
                Kind::Space => Style::new(),
                Kind::Str => t.string.s(),
                Kind::Var => t.var.s(),
                Kind::Comment => t.muted.s().italic(),
                Kind::Op => {
                    if text.contains(['|', ';', '&']) {
                        expect_cmd = true;
                    }
                    t.op.b()
                }
                Kind::Word if expect_cmd => {
                    expect_cmd = false;
                    arg_idx = 0;
                    cmd_name = text.to_string();
                    if text.contains('=') && !text.starts_with('=') {
                        expect_cmd = true; // VAR=valor cmd
                        t.var.s()
                    } else if crate::complete::BUILTINS.iter().any(|(b, _)| *b == text) {
                        t.builtin.b()
                    } else if st.cfg.alias.contains_key(text) || st.cfg.atajos.contains_key(text) {
                        t.alias.b()
                    } else if st.bins.contains(text) || is_exec_path(text) {
                        t.cmd.b()
                    } else {
                        t.cmd_bad.b()
                    }
                }
                Kind::Word => {
                    arg_idx += 1;
                    if text.starts_with('-') {
                        t.flag.s()
                    } else if arg_idx == 1 && cmd_name == "s" && st.servers.contains_key(text) {
                        t.server.b()
                    } else if arg_idx == 1 && cmd_name == "ir" && st.cfg.dirs.contains_key(text) {
                        t.builtin.s()
                    } else if (text.contains('/') || text.starts_with('~'))
                        && std::path::Path::new(&crate::parse::expand_tilde(text)).exists()
                    {
                        t.path.s().underline()
                    } else {
                        Style::new()
                    }
                }
            };
            out.push((style, text.to_string()));
        }
        out
    }
}

fn is_exec_path(s: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !s.contains('/') {
        return false;
    }
    std::fs::metadata(crate::parse::expand_tilde(s))
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
