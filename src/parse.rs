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

/// Palabras que abren/cierran un bloque compuesto de shell (en posición de comando; `{`/`}`
/// cuentan en cualquier posición, ver `end_word`).
const KW_OPEN: &[&str] = &["if", "case", "for", "while", "until", "select", "repeat"];
const KW_CLOSE: &[&str] = &["fi", "esac", "done"];
/// Tras estas palabras la siguiente sigue en posición de comando (`if cd x`, `then for …`).
const KW_CMD_NEXT: &[&str] = &["if", "then", "else", "elif", "while", "until", "do", "!", "time", "{"];

/// Parte un bloque de varias líneas (p. ej. un pegado) en los comandos que lo forman, en el
/// orden en que se habrían escrito a mano, para ejecutarlos uno tras otro (así `cd` y demás
/// builtins siguen afectando a mish). Solo se parte en saltos de línea de nivel superior: un
/// salto dentro de comillas, `$(…)`, `` `…` ``, un `if`/`for`/`while`/`case`/`{ … }`, tras `|`,
/// `&&`, `||` o `f()`, o el cuerpo de un heredoc (`<<FIN … FIN`) quedan dentro de su comando,
/// que entonces ocupa varias líneas. Las continuaciones `\` + salto se unen; las líneas vacías
/// y las que solo son un comentario se descartan.
///
/// Devuelve `None` si el bloque tiene varias líneas y queda algo sin cerrar al final (comillas,
/// paréntesis, `if` sin `fi`, heredoc sin terminador): entonces va entero a la shell de
/// delegación, que dará el error que toque.
pub fn split_lines(src: &str) -> Option<Vec<String>> {
    let src: Vec<char> = src.replace("\r\n", "\n").replace('\r', "\n").chars().collect();
    let mut segs: Vec<String> = Vec::new();
    let mut cur = String::new();
    let (mut sq, mut dq, mut bt, mut comment) = (false, false, false, false);
    let mut paren = 0usize; // ( … ) y $( … ) fuera de comillas
    let mut kw = 0usize; // if/fi, case/esac, for|while|until…/done, { }
    let mut word = String::new();
    let mut cmd_pos = true;
    let mut heredocs: Vec<(String, bool)> = Vec::new(); // (terminador, `<<-`) pendientes
    let mut multiline = false;
    let mut i = 0;

    // Cierra la palabra en curso y lleva la cuenta de palabras clave en posición de comando.
    fn end_word(word: &mut String, cmd_pos: &mut bool, kw: &mut usize) {
        if word.is_empty() {
            return;
        }
        // `{ … }` (grupo o cuerpo de función: `f() {`, `function f {`) cuenta en cualquier posición.
        if word == "{" {
            *kw += 1;
        } else if word == "}" {
            *kw = kw.saturating_sub(1);
        }
        if *cmd_pos {
            if KW_OPEN.contains(&word.as_str()) {
                *kw += 1;
            } else if KW_CLOSE.contains(&word.as_str()) {
                *kw = kw.saturating_sub(1);
            }
            *cmd_pos = KW_CMD_NEXT.contains(&word.as_str()) || word.contains('=');
        }
        word.clear();
    }

    while i < src.len() {
        let c = src[i];
        i += 1;
        if comment {
            if c != '\n' {
                cur.push(c);
                continue;
            }
            comment = false;
        }
        if sq {
            cur.push(c);
            sq = c != '\'';
            multiline |= c == '\n';
            continue;
        }
        match c {
            '\\' => match src.get(i) {
                Some('\n') => i += 1, // continuación: se quitan los dos (también dentro de "…")
                Some(&n) => {
                    i += 1;
                    cur.push(c);
                    cur.push(n);
                    word.push(c);
                    word.push(n);
                }
                None => cur.push(c),
            },
            '\n' => {
                end_word(&mut word, &mut cmd_pos, &mut kw);
                // Cuerpos de los heredoc de esta línea, tal cual, hasta su terminador.
                for (delim, strip_tabs) in heredocs.drain(..) {
                    cur.push('\n');
                    loop {
                        if i >= src.len() {
                            return None; // heredoc sin terminador
                        }
                        let end = src[i..].iter().position(|&ch| ch == '\n').map_or(src.len(), |p| i + p);
                        let line: String = src[i..end].iter().collect();
                        cur.push_str(&line);
                        i = end;
                        let l = if strip_tabs { line.trim_start_matches('\t') } else { &line };
                        if l == delim {
                            i += 1; // su salto de línea es el que decide abajo
                            break;
                        }
                        cur.push('\n');
                        i += 1; // el salto de la línea del cuerpo
                    }
                    multiline = true;
                }
                let t = cur.trim_end();
                let open = dq || bt || paren > 0 || kw > 0;
                if open || t.ends_with('|') || t.ends_with("&&") || t.ends_with("()") {
                    // El comando sigue en la línea siguiente.
                    cur.push('\n');
                    multiline = true;
                    cmd_pos = true;
                    continue;
                }
                segs.push(std::mem::take(&mut cur));
                cmd_pos = true;
            }
            '"' if !bt => {
                dq = !dq;
                cur.push(c);
                word.push(c);
            }
            _ if dq => {
                cur.push(c);
                word.push(c);
            }
            '\'' => {
                sq = true;
                cur.push(c);
                word.push(c);
            }
            '`' => {
                bt = !bt;
                cur.push(c);
                word.push(c);
            }
            _ if bt => {
                cur.push(c);
                word.push(c);
            }
            '#' if word.is_empty() => {
                comment = true;
                cur.push(c);
            }
            '<' if src.get(i) == Some(&'<') => {
                i += 1;
                end_word(&mut word, &mut cmd_pos, &mut kw);
                cur.push_str("<<");
                if src.get(i) == Some(&'<') {
                    i += 1; // <<< (here-string): una sola línea
                    cur.push('<');
                } else if paren == 0 {
                    let strip_tabs = src.get(i) == Some(&'-');
                    if strip_tabs {
                        i += 1;
                        cur.push('-');
                    }
                    // El terminador (sin comillas ni barras); el texto sigue su curso normal.
                    let mut j = i;
                    while j < src.len() && (src[j] == ' ' || src[j] == '\t') {
                        j += 1;
                    }
                    let mut delim = String::new();
                    while j < src.len() && !src[j].is_whitespace() && !";&|<>()".contains(src[j]) {
                        if !matches!(src[j], '\'' | '"' | '\\') {
                            delim.push(src[j]);
                        }
                        j += 1;
                    }
                    if !delim.is_empty() {
                        heredocs.push((delim, strip_tabs));
                    }
                }
                cmd_pos = false;
            }
            '(' | ')' => {
                if c == '(' {
                    paren += 1;
                } else {
                    paren = paren.saturating_sub(1);
                }
                // `$(` y `<(` forman parte de la palabra; `(` suelto abre una subshell.
                if c == '(' && word.is_empty() {
                    cmd_pos = true;
                } else if c == ')' {
                    end_word(&mut word, &mut cmd_pos, &mut kw);
                } else {
                    word.push(c);
                }
                cur.push(c);
            }
            ';' | '|' | '&' => {
                end_word(&mut word, &mut cmd_pos, &mut kw);
                cmd_pos = true;
                cur.push(c);
            }
            c if c.is_whitespace() => {
                end_word(&mut word, &mut cmd_pos, &mut kw);
                cur.push(c);
            }
            c => {
                cur.push(c);
                word.push(c);
            }
        }
    }
    end_word(&mut word, &mut cmd_pos, &mut kw);
    let unclosed = sq || dq || bt || paren > 0 || kw > 0 || !heredocs.is_empty();
    if unclosed && (multiline || !segs.is_empty()) {
        return None;
    }
    // Sin cerrar en una sola línea: igual que antes (la shell de delegación da el error).
    segs.push(cur);
    Some(
        segs.into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
            .collect(),
    )
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
