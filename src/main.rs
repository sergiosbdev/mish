mod claude_hooks;
mod complete;
mod config;
mod exec;
mod git;
mod highlight;
mod hook;
mod ls;
mod parse;
mod prompt;
mod servers;
mod sshdest;
mod status;
mod term;
mod theme;
mod update;
mod zdirs;

use nu_ansi_term::Style;
use parse::Parsed;
use reedline::{
    default_emacs_keybindings, default_vi_insert_keybindings, default_vi_normal_keybindings,
    default_vi_visual_keybindings, ColumnarMenu, DefaultHinter, Emacs, ExternalPrinter, FileBackedHistory,
    KeyCode, KeyModifiers, Keybindings, ListMenu, MenuBuilder, Reedline, ReedlineEvent, ReedlineMenu, Signal, Vi,
};
use std::collections::{BTreeMap, HashSet};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use theme::Theme;

/// Estado compartido con el autocompletado y el resaltado.
pub struct State {
    pub cfg: config::Config,
    pub theme: Theme,
    pub servers: servers::Servers,
    pub path_bins: Vec<String>,
    pub bins: HashSet<String>,
    /// Alias creados con `alias x=y` durante la sesión (no se guardan).
    pub session_alias: BTreeMap<String, String>,
}

impl State {
    fn load() -> Self {
        let cfg = config::load();
        let servers = servers::load(&cfg.servidores.extra);
        let path_bins = complete::scan_path();
        Self {
            theme: theme::by_name(&cfg.tema.nombre),
            bins: path_bins.iter().cloned().collect(),
            cfg,
            servers,
            path_bins,
            session_alias: BTreeMap::new(),
        }
    }

    fn reload(&mut self) {
        let mut cfg = config::load();
        cfg.alias.extend(self.session_alias.clone());
        self.servers = servers::load(&cfg.servidores.extra);
        self.theme = theme::by_name(&cfg.tema.nombre);
        self.cfg = cfg;
        self.rescan_path();
    }

    fn rescan_path(&mut self) {
        self.path_bins = complete::scan_path();
        self.bins = self.path_bins.iter().cloned().collect();
    }
}

struct Shell {
    state: Arc<Mutex<State>>,
    exec: exec::Exec,
    ctx: prompt::Ctx,
    cfg_mtime: Option<SystemTime>,
    vpn: status::VpnFlag,
    exit: bool,
}

/// Builtins de shell (zsh/bash) que no tienen sentido como binario: se pasa la línea entera a la
/// shell de delegación en vez de dar «comando no encontrado». Los que sí existen como binario útil
/// (`echo`, `printf`, `test`, `kill`, `true`…) se siguen ejecutando directamente.
const SHELL_BUILTINS: &[&str] = &[
    "source", ".", "set", "shopt", "setopt", "unsetopt", "declare", "typeset", "local", "readonly", "let", "eval",
    "history", "fc", "pushd", "popd", "dirs", "exec", "trap", "type", "command", "builtin", "hash", "rehash",
    "ulimit", "umask", "read", "whence", "where", "autoload", "bindkey", "functions", "unalias", "unfunction",
    "emulate", "zmodload", "print", "shift", "return", "wait", "bg", "disown", "getopts", "times", "enable",
    "compgen", "complete", "mapfile", "readarray", "caller", "suspend",
];

/// ¿Está `cmd` en algún directorio de PATH como fichero ejecutable? (consulta en vivo, no la caché)
fn in_path(cmd: &str) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|d| {
        std::fs::metadata(d.join(cmd)).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    })
}

/// Comando no builtin de mish: ¿se ejecuta directo o se delega la línea en la shell?
fn should_delegate(cmd: &str) -> bool {
    if cmd.contains('/') {
        return false; // ruta explícita: ejecución directa
    }
    SHELL_BUILTINS.contains(&cmd) || !in_path(cmd)
}

/// Programas interactivos: no tiene sentido notificar cuando terminan.
const NO_NOTIFY: &[&str] = &[
    "s", "ssh", "mosh", "vim", "nvim", "vi", "nano", "less", "more", "man", "htop", "top", "btop", "claude", "python",
    "python3", "node", "irb", "psql", "mysql", "sqlite3", "tmux", "screen", "watch",
];

fn main() {
    // mish hook <evento>: hook de Claude Code. LO PRIMERO, antes de leer config, PATH o stdin:
    // `mish` sin argumentos y con stdin que no es TTY ejecutaría stdin como script. Nunca
    // escribe en stdout y sale siempre con 0.
    {
        let mut a = std::env::args_os().skip(1);
        if a.next().is_some_and(|x| x == "hook") {
            hook::run(a.next().as_deref());
        }
    }

    let args: Vec<String> = std::env::args().collect();

    // mish claude-hooks install|uninstall|status [--dry-run]
    if args.get(1).is_some_and(|a| a == "claude-hooks") {
        std::process::exit(claude_hooks::run(&args[2..]));
    }

    // mish update [--check]: actualizar Mish.app (también builtin `update`).
    if args.get(1).is_some_and(|a| a == "update") {
        std::process::exit(update::run(&args[2..]));
    }

    // mish -c "comando": ejecuta una línea y sale.
    if args.len() >= 3 && args[1] == "-c" {
        let mut sh = Shell::new(false);
        let code = sh.run_line(&args[2]);
        std::process::exit(code);
    }

    let interactive = unsafe { libc::isatty(0) } == 1;
    let mut sh = Shell::new(interactive);

    if !interactive {
        use std::io::BufRead;
        let mut code = 0;
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            code = sh.run_line(&line);
            if sh.exit {
                break;
            }
        }
        std::process::exit(code);
    }

    unsafe { std::env::set_var("SHELL_NAME", "mish") };
    let printer: ExternalPrinter<String> = ExternalPrinter::default();

    // Bienvenida + comprobaciones en segundo plano.
    {
        let st = sh.state.lock().unwrap();
        let bienvenida = st.cfg.tema.bienvenida;
        if bienvenida {
            let dirs: Vec<String> = st.cfg.dirs.keys().cloned().collect();
            status::welcome_header(&st.theme, &dirs, 0);
        }
        if bienvenida || st.cfg.avisos.vpn {
            status::spawn(
                st.servers.clone(),
                st.cfg.servidores.prefer.clone(),
                st.theme.clone(),
                bienvenida.then(|| printer.sender()),
                sh.vpn.clone(),
            );
        }
        if bienvenida {
            println!();
        }
    }

    let mut editor = build_editor(&sh.state, printer);

    loop {
        sh.maybe_reload();
        sh.exec.reap();
        sh.exec.ensure_foreground();
        let in_app = term::is_mish_app();
        if in_app || term::is_apple_terminal() {
            if let Ok(p) = std::env::current_dir() {
                if in_app {
                    term::set_title(&parse::contract_home(&p.to_string_lossy()));
                }
                term::report_cwd(&p);
            }
        }
        let p = {
            let st = sh.state.lock().unwrap();
            let vpn_down = st.cfg.avisos.vpn && status::vpn_down(&sh.vpn);
            prompt::MishPrompt::build(&st.cfg.prompt.formato, &sh.ctx, &st.theme, vpn_down)
        };
        match editor.read_line(&p) {
            Ok(Signal::Success(line)) => {
                let code = sh.run_line(&line);
                sh.ctx.last_status = code;
                sh.after_command(&line);
                if sh.exit {
                    break;
                }
            }
            Ok(Signal::CtrlC(_)) => {
                sh.ctx.last_status = 0;
                sh.ctx.last_duration = Duration::ZERO;
            }
            Ok(Signal::CtrlD) => break,
            Ok(_) => {}
            Err(e) => {
                eprintln!("mish: error del editor: {e}");
                break;
            }
        }
    }
    sh.exec.kill_jobs();
}

fn build_editor(state: &Arc<Mutex<State>>, printer: ExternalPrinter<String>) -> Reedline {
    let (modo, theme, compacto) = {
        let st = state.lock().unwrap();
        (st.cfg.editor.modo.clone(), st.theme.clone(), st.cfg.tema.prompt_compacto)
    };

    let hist_path = std::path::PathBuf::from(parse::home()).join(".local/share/mish/history");
    if let Some(d) = hist_path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let history = FileBackedHistory::with_file(20_000, hist_path).expect("no se pudo abrir el historial");

    let completion_menu = ColumnarMenu::default().with_name("completion_menu");
    let history_menu = ListMenu::default().with_name("history_menu").with_page_size(12);

    let add_keys = |kb: &mut Keybindings| {
        kb.add_binding(
            KeyModifiers::NONE,
            KeyCode::Tab,
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("completion_menu".to_string()),
                ReedlineEvent::MenuNext,
            ]),
        );
        kb.add_binding(KeyModifiers::SHIFT, KeyCode::BackTab, ReedlineEvent::MenuPrevious);
        kb.add_binding(
            KeyModifiers::CONTROL,
            KeyCode::Char('r'),
            ReedlineEvent::UntilFound(vec![
                ReedlineEvent::Menu("history_menu".to_string()),
                ReedlineEvent::MenuPageNext,
            ]),
        );
    };

    let edit_mode: Box<dyn reedline::EditMode> = if modo == "vi" {
        let mut ins = default_vi_insert_keybindings();
        add_keys(&mut ins);
        Box::new(Vi::new(ins, default_vi_normal_keybindings(), default_vi_visual_keybindings()))
    } else {
        let mut kb = default_emacs_keybindings();
        add_keys(&mut kb);
        Box::new(Emacs::new(kb))
    };

    let mut ed = Reedline::create()
        .with_history(Box::new(history))
        .with_history_exclusion_prefix(Some(" ".into()))
        .with_hinter(Box::new(DefaultHinter::default().with_style(Style::new().fg(theme.muted.c()))))
        .with_highlighter(Box::new(highlight::MishHighlighter { state: state.clone() }))
        .with_completer(Box::new(complete::MishCompleter { state: state.clone() }))
        .with_quick_completions(true)
        .with_partial_completions(true)
        .with_menu(ReedlineMenu::EngineCompleter(Box::new(completion_menu)))
        .with_menu(ReedlineMenu::HistoryMenu(Box::new(history_menu)))
        .with_edit_mode(edit_mode)
        .with_external_printer(printer)
        // Un pegado multilínea queda en el buffer (no se ejecuta línea a línea) y llega entero, UTF-8 incluido.
        .use_bracketed_paste(true);
    if compacto {
        ed = ed.with_transient_prompt(Box::new(prompt::TransientPrompt::new(&theme)));
    }
    ed
}

impl Shell {
    fn new(interactive: bool) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::load())),
            exec: exec::Exec::init(interactive),
            ctx: prompt::Ctx { last_status: 0, last_duration: Duration::ZERO },
            cfg_mtime: config::mtime(),
            vpn: Arc::new(std::sync::atomic::AtomicU8::new(0)),
            exit: false,
        }
    }

    fn theme(&self) -> Theme {
        self.state.lock().unwrap().theme.clone()
    }

    fn maybe_reload(&mut self) {
        let m = config::mtime();
        if m != self.cfg_mtime {
            self.cfg_mtime = m;
            self.state.lock().unwrap().reload();
            eprintln!("{}", self.theme().muted.s().paint("mish: config recargada"));
        }
    }

    /// Notificación de macOS si el comando ha sido largo y no estás mirando esta pestaña.
    fn after_command(&self, line: &str) {
        let limit = self.state.lock().unwrap().cfg.avisos.notificar_seg;
        if limit == 0 || self.ctx.last_duration < Duration::from_secs(limit) {
            return;
        }
        let first = line.split_whitespace().next().unwrap_or("");
        if NO_NOTIFY.contains(&first) || (!term::is_mish_app() && term::tab_focused()) {
            return;
        }
        let dur = prompt::fmt_dur(self.ctx.last_duration);
        let (title, msg) = if self.ctx.last_status == 0 {
            ("✓ mish: terminado".to_string(), format!("{line}  ({dur})"))
        } else {
            (format!("✘ mish: error {}", self.ctx.last_status), format!("{line}  ({dur})"))
        };
        if term::is_mish_app() {
            term::app_notify(&title, &msg);
        } else {
            term::notify(&title, &msg);
        }
    }

    /// Ejecuta una línea completa. Devuelve el código de salida.
    fn run_line(&mut self, line: &str) -> i32 {
        let line = self.expand_alias(line.trim());
        let t = Instant::now();
        let code = match parse::parse(&line) {
            Parsed::Empty => return self.ctx.last_status,
            Parsed::Shell(l) => self.run_delegated(&l),
            Parsed::Simple(argv) => self.run_argv(argv, &line),
        };
        self.ctx.last_duration = t.elapsed();
        code
    }

    /// Ejecuta la línea entera con la shell de delegación (`<shell> -c línea`), en primer plano.
    fn run_delegated(&mut self, line: &str) -> i32 {
        let sh = self.state.lock().unwrap().cfg.delegate_shell();
        let mut c = Command::new(sh);
        c.arg("-c").arg(line);
        self.exec.run(c, line)
    }

    /// Sustituye la primera palabra si es alias o atajo (con límite para evitar bucles).
    fn expand_alias(&self, line: &str) -> String {
        let st = self.state.lock().unwrap();
        let mut line = line.to_string();
        let mut seen = Vec::new();
        for _ in 0..8 {
            let first = line.split_whitespace().next().unwrap_or("").to_string();
            let Some(exp) = st.cfg.alias.get(&first).or_else(|| st.cfg.atajos.get(&first)) else { break };
            if seen.contains(&first) {
                break;
            }
            let new_first = exp.split_whitespace().next().unwrap_or("");
            let rest = line[line.find(&first).unwrap() + first.len()..].to_string();
            let same = new_first == first;
            seen.push(first);
            line = format!("{exp}{rest}");
            if same {
                break; // ls = "ls -G"
            }
        }
        line
    }

    fn run_argv(&mut self, argv: Vec<String>, line: &str) -> i32 {
        let cmd = argv[0].as_str();
        let rest = &argv[1..];
        match cmd {
            "exit" | "logout" => {
                self.exit = true;
                rest.first().and_then(|s| s.parse().ok()).unwrap_or(0)
            }
            "cd" => self.cd(rest.first().map(String::as_str)),
            "ir" => self.ir(rest.first().map(String::as_str)),
            "z" => self.z(rest),
            "l" => ls::run(rest, &self.theme()),
            "s" => self.s(rest),
            "servers" => self.servers(rest.iter().any(|a| a == "-p")),
            "reload" => {
                self.state.lock().unwrap().reload();
                self.cfg_mtime = config::mtime();
                println!("config recargada: {}", config::path().display());
                0
            }
            "alias" => self.alias(rest),
            "export" => {
                for kv in rest {
                    match kv.split_once('=') {
                        Some((k, v)) => unsafe { std::env::set_var(k, v) },
                        None => eprintln!("export: usa NOMBRE=valor"),
                    }
                }
                if rest.iter().any(|kv| kv.starts_with("PATH=")) {
                    self.state.lock().unwrap().rescan_path();
                }
                0
            }
            "unset" => {
                for k in rest {
                    unsafe { std::env::remove_var(k) };
                }
                0
            }
            "jobs" => {
                for (i, j) in self.exec.jobs.iter().enumerate() {
                    println!("[{}] detenido  {}", i + 1, j.cmd);
                }
                0
            }
            "fg" => {
                let n = rest.first().and_then(|s| s.trim_start_matches('%').parse().ok());
                self.exec.fg(n)
            }
            "update" => update::run(rest),
            "help" => {
                help(&self.theme());
                0
            }
            "ssh" => self.ssh_manual(&argv, line),
            _ => self.run_external(&argv, line),
        }
    }

    /// Comando externo: delegado en la shell o ejecutado directamente.
    fn run_external(&mut self, argv: &[String], line: &str) -> i32 {
        let cmd = argv[0].as_str();
        if should_delegate(cmd) {
            return self.run_delegated(line);
        }
        let mut c = Command::new(cmd);
        c.args(&argv[1..]);
        self.exec.run(c, line)
    }

    /// `ssh …` escrito a mano: se ejecuta igual que cualquier comando externo, pero si es una
    /// sesión interactiva (sin comando remoto) tiñe la pestaña como `s`.
    fn ssh_manual(&mut self, argv: &[String], line: &str) -> i32 {
        let tint = {
            let st = self.state.lock().unwrap();
            match sshdest::destination(&argv[1..]) {
                Some((host, false)) if st.cfg.servidores.tenir && self.exec.interactive => {
                    let known = st.servers.iter().find(|(_, sv)| {
                        [&sv.plain, &sv.vpn, &sv.publico]
                            .into_iter()
                            .flatten()
                            .any(|h| sshdest::clean_host(h) == host)
                    });
                    Some(match known {
                        Some((name, _)) => {
                            let variant = host.strip_prefix(&format!("{name}-")).unwrap_or("");
                            let title = if variant.is_empty() { name.clone() } else { format!("{name} · {variant}") };
                            (theme::server_bg(&st.theme, name, st.cfg.servidores.colores.get(name)), title)
                        }
                        None => (theme::server_bg(&st.theme, &host, st.cfg.servidores.colores.get(&host)), host),
                    })
                }
                _ => None,
            }
        };
        match tint {
            Some((bg, title)) => self.with_tint(bg, &title, |sh| sh.run_external(argv, line)),
            None => self.run_external(argv, line),
        }
    }

    /// Tiñe la pestaña (Mish.app o Terminal.app) mientras corre `f` y luego la restaura.
    fn with_tint(&mut self, bg: theme::Rgb, title: &str, f: impl FnOnce(&mut Self) -> i32) -> i32 {
        let in_app = term::is_mish_app();
        let saved = if in_app {
            term::app_tint(bg, title);
            None
        } else {
            term::tint_tab(bg, title)
        };
        let code = f(self);
        if let Some(s) = &saved {
            term::restore_tab(s);
        }
        if in_app {
            term::app_untint();
        }
        code
    }

    fn cd(&mut self, arg: Option<&str>) -> i32 {
        let target = match arg {
            None => parse::home(),
            Some("-") => match std::env::var("OLDPWD") {
                Ok(p) => {
                    println!("{}", parse::contract_home(&p));
                    p
                }
                Err(_) => {
                    eprintln!("cd: no hay directorio anterior");
                    return 1;
                }
            },
            Some(p) => p.to_string(),
        };
        self.chdir(&target, "cd")
    }

    fn chdir(&mut self, target: &str, who: &str) -> i32 {
        let old = std::env::current_dir().ok();
        if let Err(e) = std::env::set_current_dir(target) {
            eprintln!("{who}: {target}: {e}");
            return 1;
        }
        unsafe {
            if let Some(o) = old {
                std::env::set_var("OLDPWD", o);
            }
            if let Ok(n) = std::env::current_dir() {
                std::env::set_var("PWD", &n);
                zdirs::visit(&n.to_string_lossy());
            }
        }
        0
    }

    fn ir(&mut self, name: Option<&str>) -> i32 {
        let (dirs, t) = {
            let st = self.state.lock().unwrap();
            (st.cfg.dirs.clone(), st.theme.clone())
        };
        let Some(name) = name else {
            for (k, v) in &dirs {
                println!("  {} {}", t.cwd.s().paint(format!("{k:<12}")), t.muted.s().paint(v));
            }
            return 0;
        };
        match dirs.get(name) {
            Some(p) => self.chdir(&parse::expand_tilde(p), "ir"),
            None => {
                eprintln!("ir: '{name}' no está en [dirs] de {}", config::path().display());
                1
            }
        }
    }

    /// `z consulta…`: salta al directorio más usado que coincida. Sin argumentos lista los más usados.
    fn z(&mut self, q: &[String]) -> i32 {
        if q.is_empty() {
            let t = self.theme();
            for p in zdirs::ranked(&[]).into_iter().take(15) {
                println!("  {}", t.cwd.s().paint(parse::contract_home(&p)));
            }
            return 0;
        }
        // Si aún no lo ha aprendido, buscar en [dirs] (por nombre o por ruta).
        let from_cfg = || {
            let st = self.state.lock().unwrap();
            let ql = q.join(" ").to_lowercase();
            st.cfg
                .dirs
                .iter()
                .find(|(k, v)| k.to_lowercase() == ql || v.to_lowercase().rsplit('/').next().unwrap_or("").contains(&ql))
                .map(|(_, v)| parse::expand_tilde(v))
        };
        match zdirs::best(q).or_else(from_cfg) {
            Some(p) => {
                println!("{}", self.theme().muted.s().paint(format!("→ {}", parse::contract_home(&p))));
                self.chdir(&p, "z")
            }
            None => {
                eprintln!("z: nada coincide con '{}' (se aprende al usar cd / ir)", q.join(" "));
                1
            }
        }
    }

    /// `s servidor [args ssh…]`: prueba las variantes en orden de preferencia y tiñe la pestaña.
    fn s(&mut self, rest: &[String]) -> i32 {
        let Some(name) = rest.first() else {
            return self.servers(false);
        };
        let (hosts, bg, tenir, t) = {
            let st = self.state.lock().unwrap();
            let hosts = match st.servers.get(name) {
                Some(sv) => sv.order(&st.cfg.servidores.prefer),
                None => vec![name.clone()],
            };
            let bg = theme::server_bg(&st.theme, name, st.cfg.servidores.colores.get(name));
            (hosts, bg, st.cfg.servidores.tenir && self.exec.interactive, st.theme.clone())
        };
        let extra = &rest[1..];
        let mut code = 255;
        for (i, host) in hosts.iter().enumerate() {
            if i > 0 {
                eprintln!("{}", t.warn.s().paint(format!("mish: probando {host}…")));
            }
            let variant = host.strip_prefix(&format!("{name}-")).unwrap_or("");
            let title = if variant.is_empty() { name.clone() } else { format!("{name} · {variant}") };
            let t0 = Instant::now();
            let run = |sh: &mut Self| {
                let mut c = Command::new("ssh");
                c.arg(host).args(extra);
                sh.exec.run(c, &format!("ssh {host}"))
            };
            code = if tenir { self.with_tint(bg, &title, run) } else { run(self) };
            // 255 = fallo de ssh. Si fue rápido, es que no conectó: probar la siguiente variante.
            if code != 255 || t0.elapsed() > Duration::from_secs(20) {
                break;
            }
        }
        code
    }

    fn servers(&mut self, probe: bool) -> i32 {
        let (servers, prefer, t) = {
            let st = self.state.lock().unwrap();
            (st.servers.clone(), st.cfg.servidores.prefer.clone(), st.theme.clone())
        };
        if servers.is_empty() {
            println!("No hay servidores en ~/.ssh/config ni en [servidores.extra].");
            return 0;
        }
        let results: BTreeMap<String, Option<Duration>> = if probe {
            let hosts: Vec<String> = servers.values().flat_map(|s| s.order(&prefer)).collect();
            let handles: Vec<_> = hosts
                .into_iter()
                .map(|h| std::thread::spawn(move || (h.clone(), servers::probe(&h))))
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect()
        } else {
            BTreeMap::new()
        };
        for (name, sv) in &servers {
            let mut line = format!("  {}", t.server.b().paint(format!("{name:<12}")));
            for host in sv.order(&prefer) {
                let label = if Some(&host) == sv.vpn.as_ref() {
                    "vpn".to_string()
                } else if Some(&host) == sv.publico.as_ref() {
                    "publico".to_string()
                } else {
                    host.clone()
                };
                let mark = match results.get(&host) {
                    Some(Some(d)) => t.ok.s().paint(format!("● {}ms", d.as_millis())).to_string(),
                    Some(None) => t.err.s().paint("●").to_string(),
                    None => String::new(),
                };
                line.push_str(&format!("  {}{mark}", t.muted.s().paint(format!("{label:<8}"))));
            }
            println!("{line}");
        }
        if !probe {
            println!("{}", t.muted.s().paint("  (servers -p para comprobar conexión)"));
        }
        0
    }

    fn alias(&mut self, rest: &[String]) -> i32 {
        let mut st = self.state.lock().unwrap();
        if rest.is_empty() {
            let t = st.theme.clone();
            for (k, v) in &st.cfg.alias {
                println!("  {} = {}", t.alias.s().paint(format!("{k:<10}")), v);
            }
            return 0;
        }
        for kv in rest {
            match kv.split_once('=') {
                Some((k, v)) => {
                    st.session_alias.insert(k.into(), v.into());
                    st.cfg.alias.insert(k.into(), v.into());
                }
                None => match st.cfg.alias.get(kv) {
                    Some(v) => println!("{kv} = {v}"),
                    None => eprintln!("alias: {kv} no existe"),
                },
            }
        }
        0
    }
}

fn help(t: &Theme) {
    println!("{}", t.accent.b().paint("mish — comandos propios"));
    for (b, d) in complete::BUILTINS {
        println!("  {} {}", t.builtin.b().paint(format!("{b:<10}")), d);
    }
    println!();
    println!("  Config: {}  (se recarga al guardar)", config::path().display());
    println!("  Pipes, redirecciones, $VAR, globs, builtins de shell y comandos que mish no conoce");
    println!("  se ejecutan con la shell de delegación (delegate_shell en la config; por defecto zsh).");
    println!("  Tab: autocompletar · →: aceptar sugerencia · Ctrl+R: historial");
    println!("  Empieza un comando con espacio para no guardarlo en el historial.");
    println!();
    println!("  Subcomandos de `mish`:");
    println!("    mish update [--check]                                  actualizar Mish.app");
    println!("    mish claude-hooks install|uninstall|status [--dry-run] hooks de Claude Code en ~/.claude/settings.json");
    println!("    mish hook <evento>                                     (lo ejecuta Claude Code; envía el estado al daemon)");
}
