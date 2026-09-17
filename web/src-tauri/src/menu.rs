//! The native menu bar.
//!
//! Every item is a plain id: clicking one emits the Tauri event `menu` with
//! that id as its payload, and `web/src/lib/menu.ts` is the only thing that
//! interprets it. A menu click therefore ends up in exactly the commands and
//! store actions a click inside the SPA would use — there is no second code
//! path for "the same thing, from the menu", and no menu-only permission.
//!
//! Two decisions here are load-bearing:
//!
//! * **The Edit submenu's predefined items are not optional.** Installing a
//!   custom menu *replaces* Tauri's default one, and the default one is where
//!   `Ctrl+C`, `Ctrl+V` and `Ctrl+A` come from on Linux. Without
//!   `PredefinedMenuItem::{cut, copy, paste, select_all}` the clipboard stops
//!   working in the what-if and price-range inputs — silently, and only in
//!   the shipped application. (GTK does not implement predefined `undo`/
//!   `redo` at all, so they are deliberately not here: `muda` accepts them
//!   and then does not add them.)
//! * **The ids are a contract with `menu.ts`.** [`MENU_IDS`] is the list, and
//!   `tests/menu.rs` pins it against what this function actually builds.
//!
//! Emitting an event needs no capability of its own — the SPA already holds
//! `core:event:allow-listen`. There is deliberately **no** `core:menu:*`
//! permission: an XSS in the webview must not be able to replace the menu.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::Runtime;

/// Every custom item id this menu emits, in menu order.
///
/// `web/src/lib/menu.ts` switches on exactly these strings; a rename on
/// either side is a menu item that silently does nothing, which is why both
/// `tests/menu.rs` and `web/src/lib/menu.test.ts` assert over the list rather
/// than over one item each.
pub const MENU_IDS: &[&str] = &[
    "trades.import",
    "trades.reload",
    "trades.open_sheet",
    "trades.reveal",
    "trades.clear",
    "trades.demo",
    "trades.leave_demo",
    "view.dashboard",
    "view.risk_graph",
    "view.closed",
    "view.health",
    "view.theme",
    "help.diagnostics",
    "help.logs",
    "help.updates",
];

/// The four submenu titles, in bar order. `Edit` is second because that is
/// where a user looks for the clipboard.
pub const SUBMENUS: &[&str] = &["Trades", "Edit", "View", "Help"];

/// Build the application menu. Handed to `tauri::Builder::menu`, which calls
/// it on the main thread while the application is being built.
pub fn build<R: Runtime>(app: &tauri::AppHandle<R>) -> tauri::Result<Menu<R>> {
    let trades = Submenu::with_id_and_items(
        app,
        "trades",
        "Trades",
        true,
        &[
            &item(app, "trades.import", "Import trades…", Some("CmdOrCtrl+O"))?,
            &item(app, "trades.reload", "Reload", Some("CmdOrCtrl+Shift+R"))?,
            &item(app, "trades.open_sheet", "Open sheet", None)?,
            &item(app, "trades.reveal", "Reveal CSV folder", None)?,
            &item(app, "trades.clear", "Clear trades", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, "trades.demo", "Load demo book", None)?,
            &item(app, "trades.leave_demo", "Leave demo", None)?,
        ],
    )?;

    // Unconditional, and never behind a `cfg`: see the module docs.
    let edit = Submenu::with_id_and_items(
        app,
        "edit",
        "Edit",
        true,
        &[
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;

    let view = Submenu::with_id_and_items(
        app,
        "view",
        "View",
        true,
        &[
            &item(app, "view.dashboard", "Dashboard", None)?,
            &item(app, "view.risk_graph", "Risk graph", None)?,
            &item(app, "view.closed", "Closed", None)?,
            &item(app, "view.health", "Data health", None)?,
            &PredefinedMenuItem::separator(app)?,
            &item(app, "view.theme", "Toggle theme", None)?,
        ],
    )?;

    let help = Submenu::with_id_and_items(
        app,
        "help",
        "Help",
        true,
        &[
            &item(app, "help.diagnostics", "Diagnostics", None)?,
            &item(app, "help.logs", "Open log folder", None)?,
            &item(app, "help.updates", "Check for updates", None)?,
        ],
    )?;

    Menu::with_items(app, &[&trades, &edit, &view, &help])
}

fn item<R: Runtime>(
    app: &tauri::AppHandle<R>,
    id: &str,
    text: &str,
    accelerator: Option<&str>,
) -> tauri::Result<MenuItem<R>> {
    MenuItem::with_id(app, id, text, true, accelerator)
}
