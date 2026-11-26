//! Estado de git para el prompt.

use std::path::Path;
use std::process::{Command, Stdio};

#[derive(Default, Debug, PartialEq)]
pub struct GitStatus {
    pub branch: String,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicts: u32,
    pub ahead: u32,
    pub behind: u32,
}

pub fn status(dir: &Path) -> Option<GitStatus> {
    let branch = crate::prompt::git_branch(dir)?;
    let out = Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok();
    let mut st = out
        .filter(|o| o.status.success())
        .map(|o| parse(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default();
    if st.branch.is_empty() || st.branch == "(detached)" {
        st.branch = branch;
    }
    Some(st)
}

pub fn parse(text: &str) -> GitStatus {
    let mut s = GitStatus::default();
    for l in text.lines() {
        if let Some(h) = l.strip_prefix("# branch.head ") {
            s.branch = h.to_string();
        } else if let Some(ab) = l.strip_prefix("# branch.ab ") {
            for p in ab.split_whitespace() {
                if let Some(n) = p.strip_prefix('+') {
                    s.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = p.strip_prefix('-') {
                    s.behind = n.parse().unwrap_or(0);
                }
            }
        } else if l.starts_with("1 ") || l.starts_with("2 ") {
            let xy = l.as_bytes();
            if xy.len() > 3 {
                if xy[2] != b'.' {
                    s.staged += 1;
                }
                if xy[3] != b'.' {
                    s.modified += 1;
                }
            }
        } else if l.starts_with("u ") {
            s.conflicts += 1;
        } else if l.starts_with("? ") {
            s.untracked += 1;
        }
    }
    s
}
