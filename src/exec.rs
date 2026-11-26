//! Ejecución de procesos con control del terminal (para que claude, vim, ssh, htop funcionen igual que en zsh).

use std::io;
use std::os::unix::process::CommandExt;
use std::process::Command;

pub struct Job {
    pub pgid: i32,
    pub cmd: String,
}

pub struct Exec {
    /// stdin es un TTY: hacemos control de terminal y grupos de procesos.
    pub interactive: bool,
    shell_pgid: i32,
    termios: Option<libc::termios>,
    pub jobs: Vec<Job>,
}

const JOB_SIGNALS: [libc::c_int; 5] = [libc::SIGINT, libc::SIGQUIT, libc::SIGTSTP, libc::SIGTTIN, libc::SIGTTOU];

impl Exec {
    pub fn init(interactive: bool) -> Self {
        let mut shell_pgid = unsafe { libc::getpgrp() };
        if interactive {
            unsafe {
                // Esperar a estar en primer plano.
                loop {
                    let fg = libc::tcgetpgrp(0);
                    if fg == -1 || fg == shell_pgid {
                        break;
                    }
                    libc::kill(-shell_pgid, libc::SIGTTIN);
                    shell_pgid = libc::getpgrp();
                }
                // mish no debe morir con Ctrl+C / Ctrl+Z: eso es para el proceso hijo.
                for s in JOB_SIGNALS {
                    libc::signal(s, libc::SIG_IGN);
                }
                let pid = libc::getpid();
                libc::setpgid(pid, pid); // falla si somos líder de sesión: no pasa nada
                shell_pgid = libc::getpgrp();
                libc::tcsetpgrp(0, shell_pgid);
            }
        }
        Self { interactive, shell_pgid, termios: None, jobs: Vec::new() }
    }

    /// Lanza el comando en primer plano y espera. Devuelve el código de salida.
    pub fn run(&mut self, mut cmd: Command, label: &str) -> i32 {
        let interactive = self.interactive;
        if interactive {
            self.termios = save_termios();
            unsafe {
                cmd.pre_exec(|| {
                    let pid = libc::getpid();
                    libc::setpgid(0, 0);
                    libc::tcsetpgrp(0, pid);
                    for s in JOB_SIGNALS {
                        libc::signal(s, libc::SIG_DFL);
                    }
                    Ok(())
                });
            }
        }
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                // El hijo puede haber hecho setpgid+tcsetpgrp en pre_exec antes de que execvp
                // fallara: el terminal quedaría con un grupo muerto en primer plano y mish en
                // segundo plano (read() → EIO → bucle infinito en crossterm). Recuperarlo.
                if interactive {
                    self.reclaim_terminal();
                }
                let prog = cmd.get_program().to_string_lossy().to_string();
                return match e.kind() {
                    io::ErrorKind::NotFound => {
                        eprintln!("mish: comando no encontrado: {prog}");
                        127
                    }
                    io::ErrorKind::PermissionDenied => {
                        eprintln!("mish: permiso denegado: {prog}");
                        126
                    }
                    _ => {
                        eprintln!("mish: {prog}: {e}");
                        126
                    }
                };
            }
        };
        let pid = child.id() as i32;
        if interactive {
            unsafe {
                libc::setpgid(pid, pid); // también desde el padre, por si gana la carrera
                libc::tcsetpgrp(0, pid);
            }
        }
        drop(child); // esperamos con waitpid para detectar Ctrl+Z
        self.wait_fg(pid, label)
    }

    fn wait_fg(&mut self, pid: i32, label: &str) -> i32 {
        let mut status: libc::c_int = 0;
        let code = loop {
            let r = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED) };
            if r == -1 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break 1;
            }
            if libc::WIFSTOPPED(status) {
                self.jobs.push(Job { pgid: pid, cmd: label.to_string() });
                eprintln!("\n[{}] detenido  {}   (fg para volver)", self.jobs.len(), label);
                break 148;
            }
            if libc::WIFEXITED(status) {
                break libc::WEXITSTATUS(status);
            }
            if libc::WIFSIGNALED(status) {
                let sig = libc::WTERMSIG(status);
                if sig == libc::SIGINT {
                    eprintln!();
                }
                break 128 + sig;
            }
        };
        if self.interactive {
            self.reclaim_terminal();
        }
        code
    }

    /// Devuelve el terminal a mish: su grupo en primer plano y los termios guardados.
    fn reclaim_terminal(&self) {
        unsafe {
            libc::tcsetpgrp(0, self.shell_pgid);
        }
        if let Some(t) = &self.termios {
            unsafe {
                libc::tcsetattr(0, libc::TCSADRAIN, t);
            }
        }
    }

    /// Salvaguarda antes de leer la siguiente línea: si por lo que sea mish no está en primer
    /// plano, reclamar el terminal (si no, read() da EIO y el editor se queda en bucle).
    pub fn ensure_foreground(&self) {
        if self.interactive && unsafe { libc::tcgetpgrp(0) } != self.shell_pgid {
            self.reclaim_terminal();
        }
    }

    /// `fg [n]`: reanuda un trabajo detenido.
    pub fn fg(&mut self, n: Option<usize>) -> i32 {
        if self.jobs.is_empty() {
            eprintln!("fg: no hay trabajos detenidos");
            return 1;
        }
        let idx = n.map(|n| n.saturating_sub(1)).unwrap_or(self.jobs.len() - 1);
        if idx >= self.jobs.len() {
            eprintln!("fg: no existe el trabajo {}", idx + 1);
            return 1;
        }
        let job = self.jobs.remove(idx);
        eprintln!("{}", job.cmd);
        self.termios = save_termios();
        unsafe {
            libc::tcsetpgrp(0, job.pgid);
            libc::kill(-job.pgid, libc::SIGCONT);
        }
        self.wait_fg(job.pgid, &job.cmd)
    }

    /// Recoge trabajos que hayan terminado por su cuenta.
    pub fn reap(&mut self) {
        self.jobs.retain(|j| {
            let mut st = 0;
            let r = unsafe { libc::waitpid(j.pgid, &mut st, libc::WNOHANG) };
            r == 0
        });
    }

    /// Al salir, matar trabajos detenidos (si no, quedarían huérfanos parados).
    pub fn kill_jobs(&mut self) {
        for j in self.jobs.drain(..) {
            unsafe {
                libc::kill(-j.pgid, libc::SIGHUP);
                libc::kill(-j.pgid, libc::SIGCONT);
            }
        }
    }
}

fn save_termios() -> Option<libc::termios> {
    unsafe {
        let mut t: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(0, &mut t) == 0).then_some(t)
    }
}
