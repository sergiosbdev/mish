//! Decide qué hacer con una línea: ejecutarla nosotros (argv simple) o pasarla a la shell de
//! delegación (`delegate_shell` en la config; por defecto /bin/zsh).

#[derive(Debug, PartialEq)]
pub enum Parsed {
    Empty,
    /// Comando simple: argv ya sin comillas y con `~` expandido.
    Simple(Vec<String>),
    /// Sintaxis que no manejamos (pipes, redirecciones, $, globs…): va a `<delegate_shell> -c`.
    Shell(String),
}

/// Caracteres que, fuera de comillas simples, obligan a delegar en la shell.
const SHELL_CHARS: &[char] = &[
    '|', '&', ';', '<', '>', '(', ')', '$', '`', '*', '?', '[', ']', '{', '}', '!',
];

pub fn parse(line: &str) -> Parsed {
    let line = line.trim();
    if line.is_empty() {
        return Parsed::Empty;
    }
    match tokenize(line) {
        Some(argv) if !argv.is_empty() => {
            // `VAR=valor comando` → shell de delegación
            if argv[0].contains('=') {
                Parsed::Shell(line.to_string())
            } else {
                Parsed::Simple(argv.into_iter().map(|t| expand_tilde(&t)).collect())
            }
        }
        Some(_) => Parsed::Empty,
        None => Parsed::Shell(line.to_string()),
    }
}

/// Separa en tokens. Devuelve `None` si hay sintaxis de shell que no manejamos.
fn tokenize(line: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_token = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return None, // comilla sin cerrar
                    }
                }
            }
            '"' => {
                in_token = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('$') | Some('`') => return None, // expansión dentro de "…"
                        Some('\\') => match chars.next() {
                            Some(ch @ ('"' | '\\')) => cur.push(ch),
                            Some(ch) => {
                                cur.push('\\');
                                cur.push(ch);
                            }
                            None => return None,
                        },
                        Some(ch) => cur.push(ch),
                        None => return None,
                    }
                }
            }
            '\\' => {
                in_token = true;
                match chars.next() {
                    Some(ch) => cur.push(ch),
                    None => return None,
                }
            }
            '#' if !in_token => return None, // comentario → que lo gestione la shell
            c if c.is_whitespace() => {
                if in_token {
                    out.push(std::mem::take(&mut cur));
                    in_token = false;
                }
            }
            c if SHELL_CHARS.contains(&c) => return None,
            c => {
                in_token = true;
                cur.push(c);
            }
        }
    }
    if in_token {
        out.push(cur);
    }
    Some(out)
}

pub fn expand_tilde(s: &str) -> String {
    if s == "~" {
        return home();
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return format!("{}/{}", home(), rest);
    }
    s.to_string()
}

pub fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

/// Sustituye `~/` por el home real → `~` para mostrar.
pub fn contract_home(p: &str) -> String {
    let h = home();
    if p == h {
        "~".into()
    } else if let Some(rest) = p.strip_prefix(&(h + "/")) {
        format!("~/{rest}")
    } else {
        p.to_string()
    }
}
