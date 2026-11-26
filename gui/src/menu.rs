//! Barra de menús nativa de macOS (muda): menú de la app (con «Buscar actualizaciones…» e
//! «Instalar comando mish en el terminal…»), Ventana y Ayuda.
//!
//! Sin menú Edición: app.js ya gestiona ⌘C/⌘V/⌘A/⌘F y los aceleradores nativos se
//! duplicarían. Tampoco se registran ⌘Enter ni ⌃⌘F (pantalla completa, también en app.js).

use muda::{
    accelerator::{Accelerator, Code, Modifiers, CMD_OR_CTRL},
    AboutMetadata, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};

const QUIT: &str = "mish.quit";
const QUIT_ALL: &str = "mish.quit_all";
const FULLSCREEN: &str = "mish.fullscreen";
const SHORTCUTS: &str = "mish.help.shortcuts";
const CHECK_UPDATES: &str = "mish.check_updates";
const INSTALL_CLI: &str = "mish.install_cli";

/// Acciones propias de la barra de menús (las predefinidas las resuelve AppKit).
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// «Salir de Mish» (⌘Q): mismo camino que `Msg::Quit` (con daemon, desconecta y las
    /// sesiones siguen vivas).
    Quit,
    /// «Salir y cerrar sesiones» (⌥⌘Q): mismo camino que `Msg::QuitAll`.
    QuitAll,
    /// «Pantalla completa»: mismo camino que `Msg::Fullscreen`.
    Fullscreen,
    /// «Atajos de teclado» (⌘/): abre el popup de atajos en el webview.
    Shortcuts,
    /// «Buscar actualizaciones…»: mismo camino que IPC `update_check` (sin instalar).
    CheckUpdates,
    /// «Instalar comando mish en el terminal…»: enlace en `~/.local/bin`.
    InstallCli,
}

pub fn action(id: &MenuId) -> Option<Action> {
    match id.as_ref() {
        QUIT => Some(Action::Quit),
        QUIT_ALL => Some(Action::QuitAll),
        FULLSCREEN => Some(Action::Fullscreen),
        SHORTCUTS => Some(Action::Shortcuts),
        CHECK_UPDATES => Some(Action::CheckUpdates),
        INSTALL_CLI => Some(Action::InstallCli),
        _ => None,
    }
}

/// Crea la barra de menús y la instala en NSApp. Debe llamarse en el hilo principal con
/// NSApp ya inicializado (tras crear el event loop). El `Menu` devuelto debe mantenerse vivo.
pub fn install() -> muda::Result<Menu> {
    let about = AboutMetadata {
        name: Some("Mish".into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        ..Default::default()
    };
    let quit = MenuItem::with_id(QUIT, "Salir de Mish", true, Some(Accelerator::new(CMD_OR_CTRL, Code::KeyQ)));
    let quit_all = MenuItem::with_id(
        QUIT_ALL,
        "Salir y cerrar sesiones",
        true,
        Some(Accelerator::new(CMD_OR_CTRL | Modifiers::ALT, Code::KeyQ)),
    );
    // Sin aceleradores: se usan poco y no deben chocar con los de app.js.
    let check_updates = MenuItem::with_id(CHECK_UPDATES, "Buscar actualizaciones…", true, None);
    let install_cli = MenuItem::with_id(INSTALL_CLI, "Instalar comando mish en el terminal…", true, None);
    let app_menu = Submenu::with_items(
        "Mish",
        true,
        &[
            &PredefinedMenuItem::about(Some("Acerca de Mish"), Some(about)),
            &check_updates,
            &PredefinedMenuItem::separator(),
            &install_cli,
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Ocultar Mish")),
            &PredefinedMenuItem::hide_others(Some("Ocultar otros")),
            &PredefinedMenuItem::show_all(Some("Mostrar todo")),
            &PredefinedMenuItem::separator(),
            &quit,
            &quit_all,
        ],
    )?;

    // Pantalla completa sin acelerador: app.js ya usa ⌘Enter y ⌃⌘F.
    let window_menu = Submenu::with_items(
        "Ventana",
        true,
        &[
            &PredefinedMenuItem::minimize(Some("Minimizar")),
            &MenuItem::with_id(FULLSCREEN, "Pantalla completa", true, None),
        ],
    )?;

    let shortcuts =
        MenuItem::with_id(SHORTCUTS, "Atajos de teclado", true, Some(Accelerator::new(CMD_OR_CTRL, Code::Slash)));
    let help_menu = Submenu::with_items("Ayuda", true, &[&shortcuts])?;

    let menu = Menu::with_items(&[&app_menu, &window_menu, &help_menu])?;
    #[cfg(target_os = "macos")]
    {
        menu.init_for_nsapp();
        window_menu.set_as_windows_menu_for_nsapp();
        help_menu.set_as_help_menu_for_nsapp();
    }
    Ok(menu)
}
