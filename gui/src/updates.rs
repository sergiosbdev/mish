//! «Buscar actualizaciones…» y «Instalar comando mish en el terminal…» (menú Mish, IPC
//! `update_check` y `OSC 7703` desde `mish update`).
//!
//! Las operaciones de `mish-updater` son bloqueantes: corren en un hilo que manda
//! [`Ev`] al event loop (`UserEvent::Update`). [`Updater`] guarda el estado y traduce cada
//! paso a un aviso `mishNotice` (funciones `notice_*`, puras y probadas aquí).

use mish_updater::{self as upd, Release, UpdConfig};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Máximo de líneas de las notas de la release en el aviso.
const NOTES_MAX: usize = 8;
/// Intervalo mínimo entre avisos de progreso (~10 por segundo).
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// Mensaje cuando la app no se ejecuta desde un `.app` (p. ej. `target/` en desarrollo).
pub const NOT_INSTALLED: &str = "Solo disponible en la app instalada (Mish.app).";

/// Resultado de un paso del hilo de actualización.
#[derive(Debug)]
pub enum Ev {
    Checked(Result<Option<Box<Release>>, String>),
    Progress { done: u64, total: Option<u64> },
    Installing,
    Installed(Result<(), String>),
}

// ---------- avisos (puros) ----------

pub fn notice_checking() -> Value {
    json!({"kind": "updating", "busy": true, "msg": "Buscando actualizaciones…"})
}

pub fn notice_uptodate(current: &str) -> Value {
    json!({"kind": "uptodate", "msg": format!("Estás al día (v{current})")})
}

/// Primeras líneas no vacías de las notas (sin viñetas de Markdown sueltas).
pub fn notes_list(notes: &str) -> Vec<String> {
    notes
        .lines()
        .map(str::trim)
        .map(|l| l.trim_start_matches(['-', '*', '•']).trim_start())
        .filter(|l| !l.is_empty())
        .take(NOTES_MAX)
        .map(str::to_string)
        .collect()
}

pub fn notice_update(version: &str, notes: &str) -> Value {
    json!({
        "kind": "update",
        "title": format!("Mish v{version} disponible"),
        "list": notes_list(notes),
        "actions": [{"id": "update", "label": "Actualizar"}, {"id": "later", "label": "Más tarde"}],
    })
}

pub fn notice_progress(version: &str, done: u64, total: Option<u64>) -> Value {
    let mb = |b: u64| b as f64 / (1024.0 * 1024.0);
    match total.filter(|t| *t > 0) {
        Some(t) => json!({
            "kind": "updating",
            "title": format!("Descargando Mish v{version}"),
            "msg": format!("{:.1} / {:.1} MB", mb(done.min(t)), mb(t)),
            "progress": done.min(t) * 100 / t,
        }),
        None => json!({
            "kind": "updating",
            "title": format!("Descargando Mish v{version}"),
            "msg": format!("{:.1} MB", mb(done)),
            "busy": true,
        }),
    }
}

pub fn notice_installing(version: &str) -> Value {
    json!({"kind": "updating", "busy": true, "title": format!("Instalando Mish v{version}"), "msg": "Verificando e instalando…"})
}

pub fn notice_updated(version: &str) -> Value {
    json!({
        "kind": "updated",
        "title": format!("Mish v{version} instalado"),
        "msg": "Reinicia Mish para usar la versión nueva.",
        "actions": [{"id": "relaunch", "label": "Reiniciar Mish"}, {"id": "later", "label": "Más tarde"}],
    })
}

pub fn notice_error(msg: &str, retry: bool) -> Value {
    let mut n = json!({"kind": "error", "title": "No se pudo actualizar", "msg": msg});
    if retry {
        n["actions"] = json!([{"id": "retry", "label": "Reintentar"}, {"id": "later", "label": "Cerrar"}]);
    }
    n
}

// ---------- estado ----------

#[derive(Default)]
pub struct Updater {
    /// Hay un hilo trabajando (check, descarga o instalación).
    busy: bool,
    cfg: Option<UpdConfig>,
    /// Release encontrada y pendiente de «Actualizar».
    pending: Option<Release>,
    /// Versión que se está descargando/instalando.
    target: Option<String>,
    /// Última vez que se mandó progreso al webview.
    last_progress: Option<Instant>,
}

impl Updater {
    /// Bundle de la última comprobación (el que se actualiza y se reabre).
    pub fn bundle(&self) -> Option<PathBuf> {
        self.cfg.as_ref().map(|c| c.bundle.clone())
    }

    /// Comprueba en un hilo. Devuelve el aviso a mostrar ya (o `None` si ya hay algo en curso).
    /// Nunca instala: si hay versión nueva se muestra el aviso y solo el botón «Actualizar»
    /// ([`Updater::action`] "update") arranca la descarga.
    pub fn check(&mut self, send: impl Fn(Ev) + Send + 'static) -> Option<Value> {
        if self.busy {
            return None;
        }
        let Some(bundle) = upd::current_bundle() else {
            return Some(notice_error(NOT_INSTALLED, false));
        };
        let cfg = upd::default_config(bundle);
        self.cfg = Some(cfg.clone());
        self.pending = None;
        self.busy = true;
        std::thread::spawn(move || {
            let res = upd::check(&cfg).map(|r| r.map(Box::new)).map_err(|e| e.to_string());
            send(Ev::Checked(res));
        });
        Some(notice_checking())
    }

    /// Descarga e instala `pending` en un hilo.
    fn start_download(&mut self, send: impl Fn(Ev) + Send + 'static) -> Option<Value> {
        let (Some(cfg), Some(rel)) = (self.cfg.clone(), self.pending.take()) else { return None };
        if self.busy {
            self.pending = Some(rel);
            return None;
        }
        self.busy = true;
        let version = rel.version.to_string();
        let size = rel.zip.size;
        self.target = Some(version.clone());
        self.last_progress = None;
        std::thread::spawn(move || {
            let mut last: Option<Instant> = None;
            let mut progress = |done: u64, total: Option<u64>| {
                let finished = total.is_some_and(|t| done >= t);
                if finished || last.is_none_or(|l| l.elapsed() >= PROGRESS_EVERY) {
                    last = Some(Instant::now());
                    send(Ev::Progress { done, total });
                }
            };
            let dl = match upd::download(&cfg, &rel, &mut progress) {
                Ok(d) => d,
                Err(e) => return send(Ev::Installed(Err(e.to_string()))),
            };
            send(Ev::Installing);
            send(Ev::Installed(upd::install(&cfg, &dl).map_err(|e| e.to_string())));
        });
        Some(notice_progress(&version, 0, Some(size)))
    }

    /// Botón de un aviso de actualización. `None` si el id no es de este módulo.
    pub fn action(&mut self, id: &str, send: impl Fn(Ev) + Send + 'static) -> Option<Option<Value>> {
        match id {
            "update" => Some(self.start_download(send)),
            "retry" => Some(self.check(send)),
            "later" => Some(None),
            _ => None,
        }
    }

    /// Paso terminado en el hilo → aviso a mostrar (`None`: nada que pintar).
    pub fn on_event(&mut self, ev: Ev) -> Option<Value> {
        match ev {
            Ev::Checked(res) => {
                self.busy = false;
                match res {
                    Ok(None) => Some(notice_uptodate(&upd::current_version().to_string())),
                    Ok(Some(rel)) => {
                        let n = notice_update(&rel.version.to_string(), &rel.notes);
                        self.pending = Some(*rel);
                        Some(n)
                    }
                    Err(e) => Some(notice_error(&e, true)),
                }
            }
            Ev::Progress { done, total } => {
                // El hilo ya limita; esto evita saturar el webview si llegan en ráfaga.
                let now = Instant::now();
                let finished = total.is_some_and(|t| done >= t);
                if !finished && self.last_progress.is_some_and(|l| now - l < PROGRESS_EVERY / 2) {
                    return None;
                }
                self.last_progress = Some(now);
                Some(notice_progress(self.target.as_deref().unwrap_or("?"), done, total))
            }
            Ev::Installing => Some(notice_installing(self.target.as_deref().unwrap_or("?"))),
            Ev::Installed(res) => {
                self.busy = false;
                let v = self.target.take().unwrap_or_default();
                Some(match res {
                    Ok(()) => notice_updated(&v),
                    Err(e) => notice_error(&e, true),
                })
            }
        }
    }
}

// ---------- reinicio tras actualizar ----------

/// Lanza, desacoplado de la app, `sh -c 'sleep 1; open -n "$0"' <bundle>` (el bundle va como
/// argumento, nunca interpolado en el script).
pub fn spawn_relaunch(bundle: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("sleep 1; exec /usr/bin/open -n \"$0\"")
        .arg(bundle)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .current_dir("/");
    // SAFETY: setsid es async-signal-safe; el cierre no reserva memoria ni toma locks.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map(drop)
}

// ---------- «Instalar comando mish en el terminal…» ----------

#[derive(Debug, PartialEq, Eq)]
pub enum CliInstalled {
    Created,
    Replaced,
    Already,
}

/// Crea `<home>/.local/bin/mish → <bundle>/Contents/MacOS/mish`. Un enlace existente se
/// reemplaza; un fichero o directorio que no sea enlace no se toca.
pub fn install_cli(bundle: &Path, home: &Path) -> Result<(PathBuf, CliInstalled), String> {
    let target = bundle.join("Contents/MacOS/mish");
    if !target.is_file() {
        return Err(format!("{} no existe", target.display()));
    }
    let dir = home.join(".local/bin");
    std::fs::create_dir_all(&dir).map_err(|e| format!("no se pudo crear {}: {e}", dir.display()))?;
    let link = dir.join("mish");
    let mut kind = CliInstalled::Created;
    match std::fs::symlink_metadata(&link) {
        Ok(m) if m.file_type().is_symlink() => {
            if std::fs::read_link(&link).is_ok_and(|t| t == target) {
                return Ok((link, CliInstalled::Already));
            }
            std::fs::remove_file(&link).map_err(|e| format!("no se pudo sustituir {}: {e}", link.display()))?;
            kind = CliInstalled::Replaced;
        }
        Ok(_) => {
            return Err(format!("ya existe {} y no es un enlace; no se ha tocado", link.display()));
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(&target, &link).map_err(|e| format!("no se pudo crear {}: {e}", link.display()))?;
    Ok((link, kind))
}

/// ¿Parece que `~/.local/bin` está en el PATH de la shell? La app lanzada desde Finder no
/// hereda el PATH del terminal, así que también se miran los ficheros de arranque de zsh.
pub fn local_bin_in_path(home: &Path, path_env: Option<&str>) -> bool {
    let dir = home.join(".local/bin");
    if path_env.is_some_and(|p| std::env::split_paths(p).any(|d| d == dir)) {
        return true;
    }
    [".zshenv", ".zprofile", ".zshrc", ".profile", ".bash_profile"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(home.join(f)).ok())
        .any(|s| s.lines().any(|l| !l.trim_start().starts_with('#') && l.contains(".local/bin")))
}

/// Aviso con el resultado de «Instalar comando mish…».
pub fn notice_install_cli(res: Result<(PathBuf, CliInstalled), String>, in_path: bool) -> Value {
    match res {
        Ok((link, kind)) => {
            let what = match kind {
                CliInstalled::Created => "creado",
                CliInstalled::Replaced => "actualizado",
                CliInstalled::Already => "ya estaba instalado",
            };
            let mut msg = format!("Enlace {} {what}.", link.display());
            if !in_path {
                msg.push_str(" Añade ~/.local/bin al PATH (p. ej. en ~/.zprofile: export PATH=\"$HOME/.local/bin:$PATH\").");
            }
            json!({"kind": "uptodate", "title": "Comando mish instalado", "msg": msg})
        }
        Err(e) => json!({"kind": "error", "title": "No se pudo instalar el comando mish", "msg": e}),
    }
}
