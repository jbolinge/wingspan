//! The three facts about where this application keeps things that everything
//! else assumes and nothing else checks.

use tauri::Manager;

fn app() -> tauri::App<tauri::test::MockRuntime> {
    wingspan_desktop::register(
        tauri::test::mock_builder(),
        wingspan_desktop::runtime::AppSlot::pending(),
    )
    // The real context, not a mock one: this test is about what
    // `tauri.conf.json` compiled into the binary.
    .build(tauri::generate_context!())
    .expect("the mock app builds")
}

/// The identifier is the directory name every path below hangs off, so a
/// change to `tauri.conf.json` that nobody notices would silently move the
/// user's state.
#[test]
fn the_compiled_identifier_is_the_one_the_config_declares() {
    assert_eq!(wingspan_desktop::paths::APP_ID, "com.jbolinge.wingspan");
    assert_eq!(app().config().identifier, "com.jbolinge.wingspan");
}

/// The CLI writes `snapshot.json`/`closes.json`/`trades_root.json` through
/// `wingspan_app::paths::default_state_dir()` and the desktop reads them
/// through Tauri's `app_data_dir()`. If those two ever disagree, `doctor`
/// reports one directory and the app uses another.
#[test]
fn the_state_directory_is_tauris_app_data_dir_plus_state() {
    let app = app();
    let tauri_side = app
        .path()
        .app_data_dir()
        .expect("a data directory")
        .join("state");
    assert_eq!(wingspan_app::paths::default_state_dir(), tauri_side);
}

/// wry creates the webview data directory — which holds
/// `localStorage['wingspan.ui']`, i.e. the ids of the user's trades —
/// underneath the app data directory with the process umask. Creating the
/// parents `0o700` first is what keeps that private on a shared machine.
#[cfg(unix)]
#[test]
fn the_directories_the_app_makes_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().expect("a temp dir");
    let data = tmp.path().join("data/com.jbolinge.wingspan");
    let config = tmp.path().join("config/com.jbolinge.wingspan");
    let logs = data.join("logs");
    wingspan_desktop::paths::create_private_dirs(&data, &config, &logs).expect("the dirs are made");

    for dir in [&data, &config, &logs] {
        let mode = std::fs::metadata(dir)
            .expect("the directory exists")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "{} is {mode:o}", dir.display());
    }
}
