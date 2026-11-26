//! Lanzamiento de sesiones: PTY + proceso hijo + hilos de E/S.
//!
//! Por sesión hay tres hilos:
//! - lector del PTY → `Event::PtyOut`/`Event::PtyEof` al hub;
//! - escritor del PTY (teclado), para que un hijo que no lee su entrada no
//!   bloquee el hub;
//! - "reaper": `wait()` del hijo → `Event::ChildExit` (no deja zombis).
//!
//! Los eventos llevan una `key` única por sesión (no el id), para que un hilo
//! rezagado de una sesión ya cerrada no se confunda con otra que reutilice el id.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender, SyncSender};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::hub::Event;

pub struct Spawned {
    pub pid: u32,
    pub master: Box<dyn MasterPty + Send>,
    pub input: Sender<Vec<u8>>,
}

/// Programa y argumentos a ejecutar en la sesión.
///
/// - `app_cmd` (`MISH_APP_CMD`, tests/desarrollo): `/bin/sh -c "$MISH_APP_CMD"`.
/// - `argv` no vacío: `/bin/zsh -il -c 'exec "$@"' zsh argv…` (sin pasar por
///   el parser de la shell: cada argumento llega tal cual).
/// - si no: `/bin/zsh -il -c "exec '<mish>'"`.
pub fn command_line(
    app_cmd: Option<&str>,
    argv: Option<&[String]>,
    mish: &Path,
) -> (String, Vec<String>) {
    if let Some(c) = app_cmd.filter(|c| !c.is_empty()) {
        return ("/bin/sh".into(), vec!["-c".into(), c.into()]);
    }
    if let Some(argv) = argv.filter(|a| !a.is_empty()) {
        let mut args: Vec<String> = vec![
            "-il".into(),
            "-c".into(),
            "exec \"$@\"".into(),
            "zsh".into(),
        ];
        args.extend(argv.iter().cloned());
        return ("/bin/zsh".into(), args);
    }
    let exec = format!("exec {}", sh_quote(&mish.to_string_lossy()));
    ("/bin/zsh".into(), vec!["-il".into(), "-c".into(), exec])
}

/// Comillas simples de shell: `a'b` → `'a'\''b'`.
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Binario `mish`: hermano del ejecutable del daemon, si no `~/.cargo/bin/mish`,
/// y si tampoco existe, `mish` a secas (lo resolverá el PATH de la shell de login).
pub fn mish_path(home: &str) -> PathBuf {
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(Path::to_path_buf))
    {
        let p = dir.join("mish");
        if p.is_file() {
            return p;
        }
    }
    let p = PathBuf::from(home).join(".cargo/bin/mish");
    if p.is_file() {
        p
    } else {
        PathBuf::from("mish")
    }
}

/// Bundle `.app` si el ejecutable del daemon vive en `<X.app>/Contents/MacOS`.
pub fn bundle_of_exe(exe: &Path) -> Option<PathBuf> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let ok = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    ok.then(|| app.to_path_buf())
}

fn current_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    bundle_of_exe(&exe)
}

/// `dir` delante de `path`, sin repetirlo: las sesiones encuentran el `mish` del bundle
/// (`mish update`) sin cargo. zsh -il puede reordenar el PATH; por eso también se exporta
/// `MISH_BUNDLE`.
pub fn path_with_first(dir: &Path, path: Option<&std::ffi::OsStr>) -> std::ffi::OsString {
    let rest: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(p).filter(|d| d != dir).collect())
        .unwrap_or_default();
    std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(rest)).unwrap_or_else(|_| {
        let mut s = dir.as_os_str().to_owned();
        if let Some(p) = path.filter(|p| !p.is_empty()) {
            s.push(":");
            s.push(p);
        }
        s
    })
}

/// cwd pedido si es absoluto y es un directorio; si no, `$HOME`.
pub fn start_dir(cwd: Option<&str>, home: &str) -> PathBuf {
    match cwd.map(Path::new) {
        Some(p) if p.is_absolute() && p.is_dir() => p.to_path_buf(),
        _ => PathBuf::from(home),
    }
}

pub fn home() -> String {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "/".into())
}

pub struct SpawnReq<'a> {
    pub id: u32,
    pub key: u64,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<&'a str>,
    pub argv: Option<&'a [String]>,
    pub sock: &'a Path,
}

pub fn spawn(
    req: SpawnReq<'_>,
    events: SyncSender<Event>,
) -> Result<Spawned, Box<dyn std::error::Error>> {
    let pty = native_pty_system();
    let pair = pty.openpty(PtySize {
        rows: req.rows,
        cols: req.cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let home = home();
    let app_cmd = std::env::var("MISH_APP_CMD").ok();
    let (prog, args) = command_line(app_cmd.as_deref(), req.argv, &mish_path(&home));
    let mut cmd = CommandBuilder::new(prog);
    cmd.args(args);
    cmd.cwd(start_dir(req.cwd, &home));
    // Mismo entorno que la GUI en modo local (gui/src/main.rs spawn_session).
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "mish-app");
    cmd.env("MISH_SESSION", req.id.to_string());
    cmd.env("MISH_SOCK", req.sock.as_os_str());
    if let Some(bundle) = current_bundle() {
        let path = std::env::var_os("PATH");
        cmd.env(
            "PATH",
            path_with_first(&bundle.join("Contents/MacOS"), path.as_deref()),
        );
        cmd.env("MISH_BUNDLE", bundle);
    }
    if std::env::var("LANG").is_err() {
        cmd.env("LANG", "es_ES.UTF-8");
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    drop(pair.slave);
    let pid = child.process_id().unwrap_or(0);

    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let key = req.key;

    let ev = events.clone();
    std::thread::Builder::new()
        .name(format!("pty-r-{}", req.id))
        .spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if ev
                            .send(Event::PtyOut {
                                key,
                                data: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = ev.send(Event::PtyEof { key });
        })?;

    let (input, input_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name(format!("pty-w-{}", req.id))
        .spawn(move || {
            for data in input_rx {
                if writer.write_all(&data).is_err() {
                    break;
                }
            }
        })?;

    let ev = events;
    std::thread::Builder::new()
        .name(format!("pty-wait-{}", req.id))
        .spawn(move || {
            let _ = child.wait();
            let _ = ev.send(Event::ChildExit { key });
        })?;

    Ok(Spawned {
        pid,
        master: pair.master,
        input,
    })
}
