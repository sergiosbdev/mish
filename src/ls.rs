//! `l [ruta]`: listado visual (carpetas primero, iconos por tipo, tamaño y antigüedad).

use crate::theme::Theme;
use std::os::unix::fs::PermissionsExt;
use std::time::SystemTime;

struct Item {
    name: String,
    kind: Kind,
    size: u64,
    age: Option<u64>,
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
enum Kind {
    Dir,
    Link,
    Exec,
    File,
}

/// Glifo por tipo. Todos existen en SF Mono y en Menlo, ocupan 1 columna y ninguno
/// tiene presentación emoji (nada de ⚙ ↪ ▶ ⚠ …, que Terminal puede pintar a color y anchos).
fn icon(name: &str, kind: Kind) -> &'static str {
    match kind {
        Kind::Dir => "▸",
        Kind::Link => "→",
        Kind::Exec => "⌘",
        Kind::File => {
            let lower = name.to_lowercase();
            let base = lower.split('.').next().unwrap_or("");
            match base {
                "readme" | "license" | "licence" | "changelog" | "authors" | "notice" => return "¶",
                "makefile" | "dockerfile" | "justfile" | "procfile" | "gemfile" | "brewfile" | "containerfile" => {
                    return "⌥";
                }
                _ => {}
            }
            let ext = match lower.rsplit_once('.') {
                Some(("", _)) => return "⌥", // .gitignore, .zshrc, .env…: configuración
                Some((_, e)) => e,
                None => "",
            };
            match ext {
                "rs" | "py" | "js" | "mjs" | "cjs" | "ts" | "tsx" | "jsx" | "go" | "c" | "h" | "cc" | "cpp" | "hpp"
                | "m" | "swift" | "kt" | "java" | "php" | "rb" | "lua" | "zig" | "sh" | "zsh" | "bash" | "fish"
                | "pl" | "ex" | "exs" | "hs" | "ml" | "scala" | "dart" | "r" | "ino" => "λ",
                "md" | "markdown" | "txt" | "rst" | "org" | "adoc" | "tex" | "log" => "¶",
                "pdf" | "doc" | "docx" | "pages" | "odt" | "rtf" | "epub" | "ppt" | "pptx" => "§",
                "toml" | "json" | "jsonc" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" | "plist" | "xml"
                | "terminal" | "service" | "nix" => "⌥",
                "lock" | "sum" => "◦",
                "csv" | "tsv" | "sql" | "db" | "sqlite" | "sqlite3" | "parquet" | "xls" | "xlsx" | "numbers" => "∑",
                "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "icns" | "ico" | "heic" | "tiff" | "bmp" | "psd" => "▚",
                "zip" | "tgz" | "gz" | "tar" | "bz2" | "xz" | "zst" | "7z" | "rar" | "dmg" | "img" | "iso" | "pkg"
                | "deb" | "rpm" => "■",
                "html" | "htm" | "css" | "scss" | "sass" | "vue" | "svelte" | "astro" => "◊",
                "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "aiff" => "≈",
                "mp4" | "mov" | "mkv" | "avi" | "webm" | "m4v" => "□",
                "pem" | "crt" | "cer" | "p12" | "pfx" | "key" | "pub" | "gpg" | "asc" | "keychain" => "±",
                "ttf" | "otf" | "woff" | "woff2" | "ttc" => "ƒ",
                "o" | "a" | "so" | "dylib" | "wasm" | "class" | "pyc" | "bin" | "elf" | "rlib" => "▒",
                _ => "·",
            }
        }
    }
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n}B") } else if v < 10.0 { format!("{v:.1}{}", U[i]) } else { format!("{v:.0}{}", U[i]) }
}

fn ago(secs: u64) -> String {
    match secs {
        s if s < 60 => "ahora".into(),
        s if s < 3600 => format!("{}min", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s if s < 86_400 * 30 => format!("{}d", s / 86_400),
        s if s < 86_400 * 365 => format!("{}mes", s / (86_400 * 30)),
        s => format!("{}a", s / (86_400 * 365)),
    }
}

pub fn run(args: &[String], t: &Theme) -> i32 {
    let all = args.iter().any(|a| a == "-a");
    let target = args.iter().find(|a| !a.starts_with('-')).map(|s| crate::parse::expand_tilde(s));
    let dir = target.unwrap_or_else(|| ".".into());
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("l: {dir}: {e}");
            return 1;
        }
    };
    let now = SystemTime::now();
    let mut items: Vec<Item> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !all && name.starts_with('.') {
                return None;
            }
            let lmeta = e.path().symlink_metadata().ok()?;
            let meta = std::fs::metadata(e.path()).unwrap_or(lmeta.clone());
            let kind = if lmeta.file_type().is_symlink() {
                Kind::Link
            } else if meta.is_dir() {
                Kind::Dir
            } else if meta.permissions().mode() & 0o111 != 0 {
                Kind::Exec
            } else {
                Kind::File
            };
            let age = meta.modified().ok().and_then(|m| now.duration_since(m).ok()).map(|d| d.as_secs());
            Some(Item { name, kind, size: if meta.is_dir() { 0 } else { meta.len() }, age })
        })
        .collect();
    items.sort_by(|a, b| {
        (a.kind != Kind::Dir).cmp(&(b.kind != Kind::Dir)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });

    if items.is_empty() {
        println!("{}", t.muted.s().paint("  (vacío)"));
        return 0;
    }
    let name_w = items.iter().map(|i| i.name.chars().count()).max().unwrap_or(0).min(48);
    for it in &items {
        let hidden = it.name.starts_with('.');
        let color = match it.kind {
            Kind::Dir => t.dir.b(),
            Kind::Link => t.link.s(),
            Kind::Exec => t.exec.b(),
            Kind::File if hidden => t.muted.s(),
            Kind::File => t.file.s(),
        };
        let ic = icon(&it.name, it.kind);
        // El glifo lleva el color de su familia; el nombre, el de su tipo.
        let ic_style = match ic {
            _ if it.kind != Kind::File => color,
            "λ" => t.accent.s(),
            "¶" | "§" => t.text.s(),
            "⌥" => t.var.s(),
            "∑" => t.string.s(),
            "▚" => t.alias.s(),
            "■" => t.warn.s(),
            "◊" => t.flag.s(),
            "≈" | "□" => t.server.s(),
            "±" => t.err.s(),
            _ => t.muted.s(),
        };
        let truncated = it.name.chars().count() + usize::from(it.kind == Kind::Dir) > 48;
        let shown: String = if truncated { it.name.chars().take(47).collect::<String>() + "…" } else { it.name.clone() };
        let slash = if it.kind == Kind::Dir && !truncated { t.muted.s().paint("/").to_string() } else { String::new() };
        let pad = (name_w + 1).saturating_sub(shown.chars().count() + usize::from(!slash.is_empty()));
        let size = if it.kind == Kind::Dir { String::new() } else { human(it.size) };
        let size_style = if it.size >= 100 * 1024 * 1024 { t.warn.s() } else { t.muted.s() };
        let age = it.age.map(ago).unwrap_or_default();
        let age_style = match it.age {
            Some(s) if s < 3600 => t.accent.b(),
            Some(s) if s < 86_400 => t.accent.s(),
            Some(s) if s < 86_400 * 7 => t.file.s(),
            _ => t.muted.s(),
        };
        println!(
            "  {} {}{}{}  {}  {}",
            ic_style.paint(ic),
            color.paint(&shown),
            slash,
            " ".repeat(pad),
            size_style.paint(format!("{size:>6}")),
            age_style.paint(format!("{age:>5}")),
        );
    }
    0
}
