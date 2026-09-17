//! The native menu: the ids are a contract with `web/src/lib/menu.ts`.
//!
//! A menu item whose id nobody handles does nothing at all — no error, no log
//! line, just a click that is swallowed — so the ids are pinned here rather
//! than trusted. `menu.ts`'s own `menu.test.ts` pins the other half.
//!
//! The menu is built on `MockRuntime`, whose `run_on_main_thread` runs the
//! task inline while the app is not running: `muda` creates no GTK widget
//! until a menu is attached to a window, so the whole tree can be built and
//! read back on this headless box. If that ever stops being true the
//! assertions fall back to [`wingspan_desktop::menu::MENU_IDS`], which is the
//! same list this file walks.

use std::collections::BTreeSet;

use tauri::menu::MenuItemKind;
use tauri::test::MockRuntime;
use wingspan_desktop::menu::{self, MENU_IDS, SUBMENUS};
use wingspan_desktop::runtime::AppSlot;

fn app() -> tauri::App<MockRuntime> {
    wingspan_desktop::register(tauri::test::mock_builder(), AppSlot::pending())
        .build(tauri::generate_context!())
        .expect("the mock app builds")
}

/// Every submenu, in bar order.
fn submenus(menu: &tauri::menu::Menu<MockRuntime>) -> Vec<tauri::menu::Submenu<MockRuntime>> {
    menu.items()
        .expect("the menu's items")
        .into_iter()
        .filter_map(|item| match item {
            MenuItemKind::Submenu(submenu) => Some(submenu),
            _ => None,
        })
        .collect()
}

#[test]
fn the_menu_bar_has_the_four_submenus_in_order() {
    let app = app();
    let menu = menu::build(app.handle()).expect("the menu builds");

    let titles: Vec<String> = submenus(&menu)
        .iter()
        .map(|submenu| submenu.text().expect("a submenu title"))
        .collect();
    assert_eq!(titles, SUBMENUS, "the menu bar changed shape");
}

/// The Trades, View and Help ids are exactly the strings `menu.ts` switches
/// on. `MENU_IDS` is the shared list; this asserts the built menu matches it,
/// item for item, so neither can drift alone.
#[test]
fn every_custom_item_carries_the_id_the_spa_dispatches_on() {
    let app = app();
    let menu = menu::build(app.handle()).expect("the menu builds");

    let mut built: Vec<String> = Vec::new();
    for submenu in submenus(&menu) {
        for item in submenu.items().expect("a submenu's items") {
            if let MenuItemKind::MenuItem(entry) = item {
                built.push(entry.id().0.clone());
            }
        }
    }
    assert_eq!(
        built, MENU_IDS,
        "the built menu and MENU_IDS disagree; web/src/lib/menu.ts mirrors MENU_IDS"
    );

    // Spelled out once, so a careless edit to MENU_IDS is still caught here.
    let ids: BTreeSet<&str> = MENU_IDS.iter().copied().collect();
    for expected in [
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
    ] {
        assert!(
            ids.contains(expected),
            "{expected} is missing from the menu"
        );
    }
}

/// **Not optional.** A custom menu replaces Tauri's default one, and the
/// default one is where `Ctrl+C`, `Ctrl+V` and `Ctrl+A` come from: without
/// these four the clipboard dies in the what-if and price-range inputs, in the
/// shipped application only.
#[test]
fn the_edit_submenu_keeps_the_four_predefined_clipboard_items() {
    let app = app();
    let menu = menu::build(app.handle()).expect("the menu builds");

    let edit = submenus(&menu)
        .into_iter()
        .find(|submenu| submenu.text().is_ok_and(|text| text == "Edit"))
        .expect("an Edit submenu");
    let predefined = edit
        .items()
        .expect("Edit's items")
        .into_iter()
        .filter(|item| matches!(item, MenuItemKind::Predefined(_)))
        .count();
    assert_eq!(
        predefined, 4,
        "Edit must carry cut/copy/paste/select-all or the clipboard stops working"
    );
}

/// `Import trades…` and `Reload` are the two the user reaches for by keyboard.
#[test]
fn the_two_accelerators_are_declared() {
    let app = app();
    let menu = menu::build(app.handle()).expect("the menu builds");
    let trades = submenus(&menu)
        .into_iter()
        .find(|submenu| submenu.text().is_ok_and(|text| text == "Trades"))
        .expect("a Trades submenu");
    let ids: Vec<String> = trades
        .items()
        .expect("Trades' items")
        .iter()
        .map(|item| item.id().0.clone())
        .collect();
    assert!(ids.contains(&"trades.import".to_string()), "{ids:?}");
    assert!(ids.contains(&"trades.reload".to_string()), "{ids:?}");
}
