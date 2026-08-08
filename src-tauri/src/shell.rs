//! The macOS shell: the menu bar, the window's life cycle, and QA's invisibility.
//!
//! Everything here is about the app being a *Mac application* rather than a web
//! page in a frame. Three separate problems, one module because they all hang
//! off the same builder.
//!
//! # ⌘W used to be fatal
//!
//! Tauri's default is to destroy the window on close, and the process outlived
//! it: `ps` showed a live `mach` with no window, no Dock route back to it, and
//! no way to reach it short of `kill`. On a mail client you keep open all day
//! that is not a papercut, it is a data-loss-shaped hole — the sync loop is
//! still running, the composer may still hold an unsent draft, and the only
//! visible symptom is that the app is gone.
//!
//! So closing hides instead. The window comes back on a Dock click through
//! [`reopen`], instantly, with the store already warm — which is the behaviour
//! Mail and Messages have had for twenty years, and incidentally the fastest
//! possible "launch". ⌘Q still quits.
//!
//! # The menu is a replay device, not a second implementation
//!
//! `src/lib/keymap.ts` is the single registry for what keys do. A menu that
//! reached into the app another way would be a second, quietly diverging copy
//! of it, and the first time someone changed a binding the menu would lie.
//!
//! Instead each custom item carries a *keymap token* as its id — `"mod+,"`,
//! `"c"`, `"?"` — and choosing it emits [`MENU_EVENT`] with that token. The
//! frontend synthesises the keystroke and hands it to the keymap, so a menu
//! item and its shortcut are the same code path by construction. Adding a menu
//! entry for a binding is one line and cannot drift.
//!
//! Cut, Copy, Paste and Select All stay *predefined*, deliberately. They are
//! already live in the default menu and the app's own ⌘A works alongside them,
//! so replacing them with replay items would risk a text-editing regression to
//! buy nothing.
//!
//! # QA must be able to look without touching
//!
//! Someone is using this machine. A second Tauri window activates on macOS
//! whether you asked it to or not, so an agent opening the app to check its
//! work takes the keyboard out of the owner's hands mid-sentence.
//!
//! [`is_qa_instance`] detects the separate-store instances `scripts/qa` starts
//! and puts them in `Accessory` activation policy: no Dock tile, never the
//! active application, window created unfocused. The window still exists and
//! still renders, so `screencapture -l<id>` can read its buffer — a real app,
//! real Rust, real database, and it cannot steal a keystroke.

use tauri::menu::{AboutMetadata, Menu, MenuItemBuilder, PredefinedMenuItem, SubmenuBuilder};
use tauri::{AppHandle, Emitter, Manager, Runtime, Window};

/// Carries a keymap token to the frontend, which replays it. See the module doc.
pub const MENU_EVENT: &str = "mach://menu";

/// The label Tauri gives the window in `tauri.conf.json`.
const MAIN_WINDOW: &str = "main";

/// True when this process is one of `scripts/qa`'s side instances.
///
/// `MACH_DATA_DIR` is the signal because it is already the thing that makes an
/// instance separate: a process pointed at its own store is by definition not
/// the one the owner is reading, and there is nothing else to configure.
pub fn is_qa_instance() -> bool {
    std::env::var_os("MACH_DATA_DIR").is_some()
}

/// Makes a QA instance invisible to the person using the machine.
///
/// Two things are needed, and one alone is not enough. `Accessory` keeps the
/// process out of the Dock and stops it ever becoming the active application;
/// building the window unfocused stops the window itself pulling the keyboard
/// across as it appears. The window is still *visible*, which is the point —
/// `screencapture -l<id>` reads a real rendered window, so an agent can look at
/// the actual app with the actual database instead of at fixtures.
///
/// The caller must also have cleared `create` on the configured window (see
/// [`suppress_configured_window`]), because a window created from config exists
/// before `setup` runs and would already have taken focus.
pub fn apply_qa_policy<R: Runtime>(app: &mut tauri::App<R>) -> tauri::Result<()> {
    if !is_qa_instance() {
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    app.set_activation_policy(tauri::ActivationPolicy::Accessory);

    // Labelled "main" so `capabilities/default.json` applies — capabilities are
    // keyed on the label, and a differently-named window silently loses its
    // permissions.
    tauri::WebviewWindowBuilder::new(app, MAIN_WINDOW, tauri::WebviewUrl::default())
        .title("Mach (QA)")
        .inner_size(1440.0, 900.0)
        .visible(true)
        .focused(false)
        .build()?;

    Ok(())
}

/// Stops Tauri creating the configured window, so QA can build its own.
pub fn suppress_configured_window(context: &mut tauri::Context) {
    if !is_qa_instance() {
        return;
    }
    for window in context.config_mut().app.windows.iter_mut() {
        window.create = false;
    }
}

/// One menu entry that replays `token` through the keymap when chosen.
struct Replay {
    token: &'static str,
    label: &'static str,
    accelerator: Option<&'static str>,
}

const fn replay(token: &'static str, label: &'static str, accelerator: &'static str) -> Replay {
    Replay { token, label, accelerator: Some(accelerator) }
}

/// Builds the application menu.
///
/// Only bindings that actually exist in the frontend registry appear here. A
/// menu item for a key nothing answers to is worse than no menu item: it is
/// discoverable, and it does nothing.
pub fn build_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let item = |r: &Replay| {
        let mut b = MenuItemBuilder::with_id(r.token, r.label);
        if let Some(accel) = r.accelerator {
            b = b.accelerator(accel);
        }
        b.build(app)
    };

    // Hard-coded rather than read from the package info: run straight out of
    // `target/debug` there is no Info.plist, so macOS falls back to the
    // executable name and the menu reads "About mach" in lowercase.
    let name = "Mach";

    let about = AboutMetadata {
        name: Some(name.into()),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        comments: Some("A fast Google-only mail and calendar client.".into()),
        ..Default::default()
    };

    let app_menu = SubmenuBuilder::new(app, name)
        .item(&PredefinedMenuItem::about(app, Some(&format!("About {name}")), Some(about))?)
        .separator()
        .item(&item(&replay("mod+,", "Settings…", "CmdOrCtrl+,"))?)
        .separator()
        .services()
        .separator()
        .item(&PredefinedMenuItem::hide(app, Some(&format!("Hide {name}")))?)
        .item(&PredefinedMenuItem::hide_others(app, Some("Hide Others"))?)
        .item(&PredefinedMenuItem::show_all(app, Some("Show All"))?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some(&format!("Quit {name}")))?)
        .build()?;

    let file_menu = SubmenuBuilder::new(app, "File")
        // "New", not "New Message": `c` is compose in mail and new-event in the
        // calendar, and a ⌘N that reads "New Message" while making a calendar
        // entry would be a lie. Contextual ⌘N is the Mac convention anyway.
        .item(&item(&replay("c", "New", "CmdOrCtrl+N"))?)
        .separator()
        .item(&item(&replay("mod+k", "Command Palette…", "CmdOrCtrl+K"))?)
        .separator()
        // Hides rather than destroys — see the module doc.
        .item(&PredefinedMenuItem::close_window(app, Some("Close Window"))?)
        .build()?;

    // Left exactly as the platform default. These already coexist with the
    // app's own ⌘A, and swapping them for replay items would risk text editing
    // to gain nothing.
    let edit_menu = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let view_menu = SubmenuBuilder::new(app, "View")
        .item(&item(&replay("mod+1", "Mail", "CmdOrCtrl+1"))?)
        .item(&item(&replay("mod+2", "Calendar", "CmdOrCtrl+2"))?)
        .separator()
        // No accelerator: the binding is a bare `[`, and a bare key as a menu
        // accelerator would swallow every `[` typed into the composer. The
        // shortcut is discoverable from `?` instead.
        .item(&item(&Replay { token: "[", label: "Toggle Sidebar", accelerator: None })?)
        .separator()
        .item(&PredefinedMenuItem::fullscreen(app, Some("Toggle Full Screen"))?)
        .build()?;

    let window_menu = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, Some("Minimize"))?)
        .item(&PredefinedMenuItem::maximize(app, Some("Zoom"))?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, Some("Close Window"))?)
        .build()?;

    let help_menu = SubmenuBuilder::new(app, "Help")
        .item(&item(&Replay {
            token: "?",
            label: "Keyboard Shortcuts",
            accelerator: None,
        })?)
        .build()?;

    Menu::with_items(
        app,
        &[&app_menu, &file_menu, &edit_menu, &view_menu, &window_menu, &help_menu],
    )
}

/// Forwards a chosen menu item to the frontend as a keymap token.
///
/// The id *is* the token, so there is no table to keep in step. Predefined
/// items never arrive here — Tauri handles those itself.
pub fn on_menu_event<R: Runtime>(app: &AppHandle<R>, id: &str) {
    let _ = app.emit(MENU_EVENT, id);
}

/// Hide on close instead of destroying, so the window can come back.
///
/// Returns true when the close was intercepted, which is every time on macOS
/// and never elsewhere: on Windows and Linux closing the last window is how you
/// quit, and a hidden unquittable process there would be the bug rather than
/// the fix.
pub fn intercept_close<R: Runtime>(window: &Window<R>) -> bool {
    if cfg!(target_os = "macos") {
        let _ = window.hide();
        true
    } else {
        false
    }
}

/// Bring the window back for a Dock click or `open -a Mach`.
///
/// Also covers the case where the window was never hidden but is buried behind
/// other applications, which is what a Dock click means the rest of the time.
pub fn reopen<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env var is the whole contract with `scripts/qa`; if this flips, QA
    /// instances silently start stealing focus again.
    #[test]
    fn qa_instances_are_detected_by_their_data_dir() {
        // Serialised by running in one test fn: these mutate process state.
        std::env::remove_var("MACH_DATA_DIR");
        assert!(!is_qa_instance(), "the owner's instance must stay a normal app");

        std::env::set_var("MACH_DATA_DIR", "/tmp/mach-qa");
        assert!(is_qa_instance(), "a separate store means a separate, silent instance");

        std::env::remove_var("MACH_DATA_DIR");
    }
}
