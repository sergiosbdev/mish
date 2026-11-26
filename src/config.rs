//! Configuración en ~/.config/mish/config.toml (se crea con valores por defecto si no existe).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct Config {
    /// Shell en la que se delega lo que mish no ejecuta: "zsh", "bash" o una ruta absoluta.
    pub delegate_shell: String,
    pub prompt: PromptCfg,
    pub editor: EditorCfg,
    pub alias: BTreeMap<String, String>,
    pub atajos: BTreeMap<String, String>,
    pub dirs: BTreeMap<String, String>,
    pub servidores: ServCfg,
    pub tema: TemaCfg,
    pub avisos: AvisosCfg,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            delegate_shell: DEFAULT_DELEGATE_SHELL.into(),
            prompt: PromptCfg::default(),
            editor: EditorCfg::default(),
            alias: BTreeMap::new(),
            atajos: BTreeMap::new(),
            dirs: BTreeMap::new(),
            servidores: ServCfg::default(),
            tema: TemaCfg::default(),
            avisos: AvisosCfg::default(),
        }
    }
}

pub const DEFAULT_DELEGATE_SHELL: &str = "/bin/zsh";

impl Config {
    /// Programa de la shell de delegación ya resuelto ("zsh" → /bin/zsh, "bash" → /bin/bash).
    pub fn delegate_shell(&self) -> String {
        resolve_shell(&self.delegate_shell)
    }
}

fn resolve_shell(v: &str) -> String {
    match v.trim() {
        "" | "zsh" => DEFAULT_DELEGATE_SHELL.into(),
        "bash" => "/bin/bash".into(),
        other => other.into(), // ruta absoluta (u otro nombre: se busca en PATH)
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct TemaCfg {
    pub nombre: String,
    /// Pantalla de bienvenida al abrir.
    pub bienvenida: bool,
    /// Prompt compacto (❯ comando) para los comandos ya ejecutados.
    pub prompt_compacto: bool,
}

impl Default for TemaCfg {
    fn default() -> Self {
        Self { nombre: "cobre".into(), bienvenida: true, prompt_compacto: true }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct AvisosCfg {
    /// Notificar al terminar comandos más largos que esto (segundos) si no estás mirando. 0 = nunca.
    pub notificar_seg: u64,
    /// Avisar en el prompt cuando la VPN no responde.
    pub vpn: bool,
}

impl Default for AvisosCfg {
    fn default() -> Self {
        Self { notificar_seg: 30, vpn: true }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct PromptCfg {
    pub formato: String,
}

impl Default for PromptCfg {
    fn default() -> Self {
        Self { formato: "{cwd} {git}{tiempo}{estado}❯ ".into() }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct EditorCfg {
    /// "emacs" o "vi"
    pub modo: String,
}

impl Default for EditorCfg {
    fn default() -> Self {
        Self { modo: "emacs".into() }
    }
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct ServCfg {
    /// "vpn" o "publico": qué variante prueba primero `s`.
    pub prefer: String,
    /// Servidores extra que no están en ~/.ssh/config: nombre → destino ssh (user@host).
    pub extra: BTreeMap<String, String>,
    /// Color de fondo de la pestaña al conectar: nombre → "#rrggbb". Sin entrada: uno automático.
    pub colores: BTreeMap<String, String>,
    /// Teñir la pestaña al conectar con `s`.
    pub tenir: bool,
}

impl Default for ServCfg {
    fn default() -> Self {
        Self { prefer: "vpn".into(), extra: BTreeMap::new(), colores: BTreeMap::new(), tenir: true }
    }
}

pub const DEFAULT_TOML: &str = r##"# Configuración de mish. Se recarga sola al guardar (o con `reload`).

# Shell para lo que mish no hace por sí mismo (pipes, $VAR, globs, builtins como source/set/pushd,
# comandos que no están en PATH…): "zsh", "bash" o una ruta absoluta.
delegate_shell = "zsh"

[prompt]
# Variables: {cwd} {git} {host} {user} {estado} {tiempo} {vpn}
formato = "{cwd} {git}{tiempo}{estado}❯ "

[tema]
nombre = "cobre"
bienvenida = true        # panel al abrir: servidores, dirs
prompt_compacto = true   # los comandos ya ejecutados quedan como "❯ comando"

[avisos]
notificar_seg = 30       # notificación de macOS si un comando tarda más (y no estás mirando). 0 = nunca
vpn = true               # "⚠ sin VPN" en el prompt si los servidores -vpn no responden

[editor]
modo = "emacs"   # "emacs" o "vi" (requiere reiniciar mish)

[alias]
ll = "ls -la"
la = "ls -A"
gs = "git status"
gd = "git diff"
gl = "git log --oneline -20"
c  = "claude"

[atajos]
# Comandos con nombre propio. Se ejecutan escribiendo el nombre.
# logs-web = "ssh web 'tail -n 100 -f /var/log/syslog'"
# ip = "curl -s ifconfig.me; echo"

[dirs]
# Saltos rápidos: `ir code`
code     = "~/code"
# proyecto = "~/code/proyecto"

[servidores]
# `s web` prueba primero web-vpn y, si no conecta, web-publico.
prefer = "vpn"
tenir = true             # al conectar, la pestaña cambia de color y de título

[servidores.colores]
# Fondo de la pestaña por servidor (si no, uno automático por nombre):
# web = "#2a1418"

[servidores.extra]
# Servidores que no están en ~/.ssh/config: nombre = "usuario@host"
# db = "usuario@192.0.2.10"
"##;

pub fn path() -> PathBuf {
    PathBuf::from(crate::parse::home()).join(".config/mish/config.toml")
}

pub fn mtime() -> Option<SystemTime> {
    std::fs::metadata(path()).and_then(|m| m.modified()).ok()
}

/// Carga la config. Si no existe la crea. Si tiene errores, avisa y usa la de por defecto.
pub fn load() -> Config {
    let p = path();
    if !p.exists() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(&p, DEFAULT_TOML).is_ok() {
            eprintln!("mish: config creada en {}", p.display());
        }
    }
    let text = std::fs::read_to_string(&p).unwrap_or_else(|_| DEFAULT_TOML.to_string());
    match toml::from_str(&text) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mish: error en {}:\n{e}\nUsando configuración por defecto.", p.display());
            toml::from_str(DEFAULT_TOML).unwrap_or_default()
        }
    }
}
