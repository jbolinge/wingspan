//! The command surface, driven through the real IPC with the real ACL.
//!
//! Every trap the Phase 0 spike hit is encoded here: the invoke URL must be
//! the platform's *local* origin (`tauri://localhost`; the docs'
//! `http://tauri.localhost` is the Windows one and the ACL refuses everything
//! with "not allowed. Plugin not found"), the config's windows are not
//! materialised by `build()` so the webview is created here with the label
//! `capabilities/main.json` lists, and `generate_context!()` rather than
//! `mock_context` wherever the assertion is about the manifest.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tauri::test::MockRuntime;
use tauri::webview::InvokeRequest;
use wingspan_app::config::{Config, ConfigSources, Loaded};
use wingspan_app::error::{ApiResult, ProblemJson};
use wingspan_desktop::runtime::{AppSlot, GraphGate, Runtime};

/// Every command the SPA is allowed to call, and a body that parses.
///
/// The names are the wire contract: `build.rs` declares them, and
/// `capabilities/main.json` lists the same set with dashes. All three have to
/// agree or the command is refused.
const COMMANDS: &[(&str, &str)] = &[
    ("health", "{}"),
    ("market_snapshot", "{}"),
    ("trades", "{}"),
    ("trades_closed", "{}"),
    ("trade_by_id", r#"{"request": {"id": "M1"}}"#),
    ("portfolio", r#"{"request": {"ids": []}}"#),
    ("risk_graph", r#"{"request": {}, "seq": 1}"#),
    (
        "what_if",
        r#"{"request": {"trade_ids": [], "price": 6000.0, "date": "2026-09-18"}}"#,
    ),
    ("reconciliation", "{}"),
    ("warnings", "{}"),
    ("app_config", "{}"),
    ("reload", "{}"),
    ("trades_root", "{}"),
    ("probe_folder", r#"{"request": {"root": "/nowhere"}}"#),
    ("set_trades_root", r#"{"request": {"root": "/nowhere"}}"#),
    ("clear_trades_root", "{}"),
    ("refresh_now", "{}"),
    // Phase 8. The three pickers are absent on purpose: they open a GTK file
    // chooser the moment they are called, with no argument that could fail
    // deserialisation first, so there is nothing to invoke headlessly. They
    // are proved declared instead — see
    // `the_three_path_pickers_are_declared_even_though_they_cannot_be_invoked`.
    ("load_demo", "{}"),
    ("leave_demo", "{}"),
    ("diagnostics", r#"{"request": {"probe": false}}"#),
    ("settings_read", "{}"),
    ("settings_update", r#"{"request": {}}"#),
    ("probe_python", "{}"),
    (
        "delete_discarded_uploads",
        r#"{"request": {"path": "/nowhere"}}"#,
    ),
    ("reveal_folder", r#"{"request": {"path": "/nowhere"}}"#),
    ("open_log_folder", "{}"),
    ("open_config_file", "{}"),
    ("check_for_updates", "{}"),
];

/// The commands that deliberately answer **without** a booted application.
///
/// `health` is the SPA's "are you up yet". The three openers are the Help
/// menu, and a boot that failed is exactly when the user is told to send in
/// their log: gating "Open log folder" on a working application would take it
/// away at the only moment it matters.
const WITHOUT_A_BOOTED_APP: &[&str] = &[
    "health",
    "open_log_folder",
    "open_config_file",
    "check_for_updates",
];

const SENTENCE: &str = "Wingspan could not start: the book would not load";

fn req(cmd: &str, body: serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        cmd: cmd.into(),
        callback: tauri::ipc::CallbackFn(0),
        error: tauri::ipc::CallbackFn(1),
        // MUST be the platform's local origin — see the module docs.
        url: "tauri://localhost".parse().expect("a local origin"),
        body: tauri::ipc::InvokeBody::Json(body),
        headers: tauri::http::HeaderMap::default(),
        invoke_key: tauri::test::INVOKE_KEY.to_string(),
    }
}

fn json(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).expect("a body that parses")
}

/// The real command surface under the real capabilities, on a slot the caller
/// controls.
fn app_with(slot: AppSlot) -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>) {
    let app = wingspan_desktop::register(tauri::test::mock_builder(), slot)
        .build(tauri::generate_context!())
        .expect("the mock app builds");
    // `build()` does not materialise the config's windows, and the label has
    // to be the one `capabilities/main.json` lists or the ACL refuses
    // everything.
    let window = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .expect("the main webview");
    (app, window)
}

/// A slot whose boot has already failed — enough to prove a command is
/// reachable without building an `AppState` for it.
fn failed_app() -> (tauri::App<MockRuntime>, tauri::WebviewWindow<MockRuntime>) {
    let slot = AppSlot::pending();
    slot.publish(Err(ProblemJson::internal(SENTENCE)));
    app_with(slot)
}

fn invoke(
    window: &tauri::WebviewWindow<MockRuntime>,
    cmd: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, serde_json::Value> {
    match tauri::test::get_ipc_response(window, req(cmd, body)) {
        Ok(value) => Ok(value.deserialize::<serde_json::Value>().expect("json")),
        Err(value) => Err(value),
    }
}

// ------------------------------------------------------------------ the ACL

#[test]
fn every_command_is_registered_under_the_name_the_spa_calls() {
    let (_app, window) = failed_app();
    for (cmd, body) in COMMANDS {
        // `health` answers `Ok` even on a failed boot; everything else
        // rejects. Either way, what must never happen is the ACL's refusal.
        if let Err(rejection) = invoke(&window, cmd, json(body)) {
            assert!(
                !rejection
                    .as_str()
                    .is_some_and(|s| s.contains("not allowed")),
                "{cmd} was refused by the ACL: {rejection}"
            );
        }
    }

    // `pick_folder` opens a GTK chooser, so it is proved reachable rather
    // than run: the ACL is consulted before the arguments are parsed, so an
    // "invalid args" rejection means the command exists and was allowed.
    let rejection = invoke(
        &window,
        "pick_folder",
        json(r#"{"request": {"start": 12}}"#),
    )
    .expect_err("start is a string");
    assert!(
        rejection
            .as_str()
            .is_some_and(|s| s.contains("invalid args `request` for command `pick_folder`")),
        "{rejection}"
    );
}

/// The slot has to keep a value published before anything subscribed.
///
/// `watch::Sender::send` **discards** the value when every receiver has been
/// dropped, and at boot there is no receiver yet — so a slot built on `send`
/// stays empty and every command pends for ever on a boot that finished.
#[test]
fn the_slot_keeps_a_value_published_before_anyone_subscribed() {
    let slot = AppSlot::pending();
    assert!(!slot.ready_now());
    slot.publish(Err(ProblemJson::internal(SENTENCE)));

    let answer = tauri::async_runtime::handle().block_on(async { slot.ready().await });
    let Err(problem) = answer else {
        panic!("the published failure came back as a runtime");
    };
    assert_eq!(problem.detail, SENTENCE, "the published value was dropped");
}

/// `health` is deliberately the exception: it must answer *while* boot runs,
/// so it never awaits the slot.
#[test]
fn health_answers_without_waiting_for_the_boot_it_reports_on() {
    let (_app, window) = failed_app();
    let body = invoke(&window, "health", json("{}")).expect("health always answers");
    assert_eq!(body["status"], "ok");
    assert_eq!(body["version"], wingspan_app::VERSION);
    assert_eq!(body["ready"], false, "the boot failed, so we are not ready");

    let (_app, window) = app_with(AppSlot::pending());
    let body = invoke(&window, "health", json("{}")).expect("health does not pend");
    assert_eq!(body["ready"], false, "boot has not finished");
}

/// Once `build.rs` declares an app manifest the ACL gates *every* command, so
/// a name that is not on the list is refused — with the same bare string a
/// nonexistent command gets.
#[test]
fn an_undeclared_command_is_refused() {
    let (_app, window) = failed_app();
    let rejection = invoke(&window, "delete_everything", json("{}"))
        .expect_err("an undeclared command must not run");
    assert_eq!(
        rejection,
        serde_json::json!("delete_everything not allowed. Command not found")
    );
}

// -------------------------------------------------------------- error shape

/// A `ProblemJson` `Err` reaches JS as the serialised struct, so `client.ts`
/// reads `detail` and `status` exactly as it read them off problem+json.
#[test]
fn a_fatal_boot_rejects_every_command_with_the_startup_sentence() {
    let (_app, window) = failed_app();
    let expected = serde_json::json!({
        "type": "https://wingspan.invalid/problems/internal",
        "title": "Internal Server Error",
        "status": 500,
        "detail": SENTENCE,
    });
    for (cmd, body) in COMMANDS {
        if WITHOUT_A_BOOTED_APP.contains(cmd) {
            continue;
        }
        let rejection = invoke(&window, cmd, json(body)).expect_err("the boot failed");
        assert_eq!(rejection, expected, "{cmd} answered with the wrong shape");
    }
}

/// The envelope: one argument named `request`, never spread fields.
#[test]
fn the_request_envelope_deserialises_and_a_missing_field_is_a_bare_string() {
    let (_app, window) = failed_app();

    // A body that parses gets past deserialisation and reaches the slot.
    let rejection = invoke(&window, "trade_by_id", json(r#"{"request": {"id": "M1"}}"#))
        .expect_err("the boot failed");
    assert_eq!(rejection["detail"], SENTENCE);

    // A missing field is refused before the command body runs, and arrives as
    // a bare string rather than a ProblemJson — which is why `client.ts` has
    // to accept both.
    let rejection =
        invoke(&window, "trade_by_id", json(r#"{"request": {}}"#)).expect_err("id is required");
    assert_eq!(
        rejection,
        serde_json::json!("invalid args `request` for command `trade_by_id`: missing field `id`")
    );

    // Spread fields are not the envelope: `{"id": ...}` is not `request`.
    let rejection =
        invoke(&window, "trade_by_id", json(r#"{"id": "M1"}"#)).expect_err("not the envelope");
    assert!(
        rejection
            .as_str()
            .is_some_and(|s| s.contains("invalid args `request`")),
        "{rejection}"
    );
}

// ----------------------------------------------------------------- pending

/// A command invoked before boot finishes **pends**; it is never a 503.
///
/// A 503 would reject `useTrades` and render `ErrorNote "Request failed.
/// starting up [Retry]"` for up to ~40 s on a cold Schwab launch, where
/// pending is exactly what a not-yet-listening socket did.
#[test]
fn a_command_invoked_before_boot_pends() {
    let slot = AppSlot::pending();
    let (_app, window) = app_with(slot.clone());

    let publisher = slot.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        publisher.publish(Err(ProblemJson::internal(SENTENCE)));
    });

    let started = Instant::now();
    let rejection = invoke(&window, "trades", json("{}")).expect_err("the boot failed");
    let waited = started.elapsed();
    assert_eq!(rejection["detail"], SENTENCE);
    assert!(
        waited >= Duration::from_millis(350),
        "the command answered without pending, after {waited:?}"
    );
}

// ------------------------------------------------------------------ panics

/// A command that panics rejects with a 500-shaped `ProblemJson`, promptly.
///
/// Without [`wingspan_desktop::commands::off_runtime`]'s mapping an async
/// command that panics drops its responder and the JS promise never settles:
/// a spinner, for ever, with no error anywhere. (A *sync* command would be
/// worse still — a non-unwinding panic inside WebKit's `extern "C"` callback,
/// SIGABRT, exit 134.) The assertion is that it settles quickly, not that it
/// eventually returns.
///
/// It uses `mock_context` because the panicking command is a test fixture and
/// so is not in the ACL, which the real context enforces.
#[test]
fn a_panicking_command_rejects() {
    #[tauri::command]
    async fn boom() -> Result<u32, ProblemJson> {
        wingspan_desktop::commands::off_runtime(|| -> ApiResult<u32> {
            panic!("boom inside spawn_blocking")
        })
        .await
    }

    let app = tauri::test::mock_builder()
        .invoke_handler(tauri::generate_handler![boom])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("the mock app builds");
    let window = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .expect("the main webview");

    let started = Instant::now();
    let rejection = invoke(&window, "boom", json("{}")).expect_err("a panic must reject");
    let waited = started.elapsed();
    assert_eq!(rejection["status"], 500);
    assert_eq!(rejection["detail"], "the request did not finish");
    assert!(
        waited < Duration::from_secs(1),
        "the promise settled only after {waited:?}"
    );
}

/// The same, for the half of a command that is *not* under `spawn_blocking`.
///
/// The three folder commands call service functions that are already `async`:
/// the filesystem work is on a blocking worker, but the tail — building the
/// DTO, re-installing the watcher, forgetting the price cache — runs on the
/// caller's task. A panic there drops the responder just as surely, so those
/// go through [`wingspan_desktop::commands::off_runtime_async`], which puts a
/// `JoinHandle` in the same place.
#[test]
fn a_panic_after_an_await_rejects_too() {
    #[tauri::command]
    async fn boom_later() -> Result<u32, ProblemJson> {
        wingspan_desktop::commands::off_runtime_async(async {
            // An await point first, so the panic is genuinely on the spawned
            // task's async half rather than inside a blocking closure.
            tokio::task::yield_now().await;
            panic!("boom after the await")
        })
        .await
    }

    let app = tauri::test::mock_builder()
        .invoke_handler(tauri::generate_handler![boom_later])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .expect("the mock app builds");
    let window = tauri::WebviewWindowBuilder::new(&app, "main", tauri::WebviewUrl::default())
        .build()
        .expect("the main webview");

    let started = Instant::now();
    let rejection = invoke(&window, "boom_later", json("{}")).expect_err("a panic must reject");
    let waited = started.elapsed();
    assert_eq!(rejection["status"], 500);
    assert_eq!(rejection["detail"], "the request did not finish");
    assert!(
        waited < Duration::from_secs(1),
        "the promise settled only after {waited:?}"
    );
}

// -------------------------------------------------------- WINGSPAN_AS_OF

/// The pin moves the valuation date and the pricing clock **together**.
///
/// Setting one without the other is the bug `cli::pin_to` exists to prevent:
/// a leg between the two dates is counted as live by the date and priced as
/// expired by the clock, so the position renders with a structurally
/// worthless leg.
#[test]
fn wingspan_as_of_pins_both_the_valuation_date_and_the_clock() {
    let date = chrono::NaiveDate::from_ymd_opt(2026, 8, 31).expect("a real date");

    let _pinned = AS_OF_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    std::env::set_var(wingspan_desktop::boot::AS_OF_ENV, "2026-08-31");
    let secrets: std::collections::BTreeMap<String, String> =
        [("SCHWAB_APP_KEY".to_string(), "a value".to_string())]
            .into_iter()
            .collect();
    let options = wingspan_desktop::boot::app_options(secrets.clone());
    std::env::remove_var(wingspan_desktop::boot::AS_OF_ENV);

    assert_eq!(options.valuation_date, Some(date));
    // The credentials `main` parsed are carried through as values rather
    // than exported.
    assert_eq!(options.schwab_secrets, secrets);
    assert_eq!(
        options.clock.now(),
        wingspan_app::demo::clock_at(date).now(),
        "the clock has to move with the date"
    );

    // A mistyped date is a warning, never a refusal to start: the window is
    // the only place the user can read the warning.
    let ignored = wingspan_desktop::boot::app_options_from(Some("last tuesday"));
    assert_eq!(ignored.valuation_date, None);
    let unset = wingspan_desktop::boot::app_options_from(None);
    assert_eq!(unset.valuation_date, None);
}

// ------------------------------------------------------- a booted runtime

/// Boot a real `AppState` over the fixtures with the synthetic provider and a
/// one-second refresh, and hold it the way `setup` does.
fn booted() -> (
    tauri::App<MockRuntime>,
    tauri::WebviewWindow<MockRuntime>,
    AppSlot,
) {
    let mut config = wingspan_app::demo::demo_config(wingspan_app::demo::scratch_dir());
    // Fast enough that "two refresh intervals" is a test, not a nap.
    config.market.refresh_secs_open = 1;
    config.market.refresh_secs_closed = 1;
    let options = wingspan_app::state::AppOptions {
        demo: true,
        root_override: Some(wingspan_app::demo::fixtures_root()),
        // Deliberately the *system* clock: the fixture provider stamps every
        // snapshot with `clock.now()`, so a pinned clock would make `as_of`
        // constant and the R3 test below unable to fail.
        ..wingspan_app::state::AppOptions::default()
    };
    boot(config, options)
}

/// A `Loaded` around a config that was never read from a file — what a test
/// hands `Runtime`, whose `loaded` is what `diagnostics` reports on.
fn loaded(config: &Config) -> Loaded {
    Loaded {
        config: config.clone(),
        sources: ConfigSources::default(),
        notes: Vec::new(),
        env_values: std::collections::BTreeMap::new(),
        layers: wingspan_app::config::Layers::user_only(),
    }
}

/// `booted()`, for a caller that wants its own config and options.
fn boot(
    config: Config,
    options: wingspan_app::state::AppOptions,
) -> (
    tauri::App<MockRuntime>,
    tauri::WebviewWindow<MockRuntime>,
    AppSlot,
) {
    let slot = AppSlot::pending();
    let (app, window) = app_with(slot.clone());
    let handle = app.handle().clone();
    let published = loaded(&config);

    tauri::async_runtime::handle().block_on(async {
        let state = wingspan_app::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let runtime = Runtime {
            background: wingspan_app::serve::spawn_background(&state),
            pump: wingspan_desktop::bridge::spawn(handle, state.clone()),
            state,
            graph_gate: GraphGate::new(),
            loaded: published,
        };
        slot.publish(Ok(Arc::new(runtime)));
    });
    (app, window, slot)
}

/// **R3.** `Background` holds a `RefreshHandle` and a `CloseRecorder` that
/// abort on drop. Created in `setup` and not owned by the `Runtime` the slot
/// holds, they would be dropped when `setup` returned: prices would freeze
/// after the one refresh `AppState::build` awaits, with the status pill still
/// green. So this asserts the snapshot actually *advances* — not that a
/// handle exists somewhere.
#[test]
fn the_market_snapshot_advances_because_the_background_handles_are_owned() {
    let (_app, window, _slot) = booted();

    let first = invoke(&window, "market_snapshot", json("{}")).expect("a snapshot");
    let before = as_of(&first);

    // Everything the *boot* produced is now in the past, so a snapshot
    // stamped after this instant can only have come from the running loop —
    // which is the whole assertion. (Comparing the two `as_of` values alone
    // is not enough: `AppState::build` awaits one refresh and the loop fires
    // another milliseconds later, so two boot-time snapshots already differ.)
    let waiting_from = chrono::Utc::now();
    // Two refresh intervals plus slack.
    std::thread::sleep(Duration::from_millis(2600));

    let second = invoke(&window, "market_snapshot", json("{}")).expect("a later snapshot");
    let after = as_of(&second);

    assert!(
        after > before,
        "the refresh loop stopped: as_of stayed at {before} (then {after})"
    );
    assert!(
        after > waiting_from,
        "the refresh loop stopped at boot: the newest snapshot is {after}, \
         from before we started waiting at {waiting_from}"
    );
}

fn as_of(snapshot: &serde_json::Value) -> chrono::DateTime<chrono::Utc> {
    snapshot["as_of"]
        .as_str()
        .expect("an as_of")
        .parse()
        .expect("an RFC 3339 instant")
}

/// `seq` rides beside `request`, never inside it — a per-call field would
/// make `queryKeys.riskGraph(request)` a fresh key every time and lose the
/// 5 s cache hit on a returned slider. And a call overtaken while it queued
/// still answers: the JS side has already dropped that promise, and an error
/// would mark an unobserved query errored.
#[test]
fn risk_graph_takes_seq_beside_request_and_answers_even_when_superseded() {
    let (_app, window, _slot) = booted();

    let newest = invoke(&window, "risk_graph", json(r#"{"request": {}, "seq": 10}"#))
        .expect("the newest call answers");
    assert!(newest["lines"].is_array(), "{newest}");

    // Overtaken: seq 1 is older than the 10 the gate has already seen.
    let superseded = invoke(&window, "risk_graph", json(r#"{"request": {}, "seq": 1}"#))
        .expect("a superseded call returns its result anyway");
    assert!(superseded["lines"].is_array(), "{superseded}");

    // `seq` inside the envelope is not `seq`: the argument is required.
    let rejection = invoke(&window, "risk_graph", json(r#"{"request": {"seq": 1}}"#))
        .expect_err("seq is a top-level argument");
    assert!(
        rejection
            .as_str()
            .is_some_and(|s| s.contains("invalid args") && s.contains("seq")),
        "{rejection}"
    );
}

/// A pinned run has to say so where the user can see it, or a forgotten
/// `WINGSPAN_AS_OF` silently values the book on the wrong day for ever.
#[test]
fn a_pinned_valuation_date_surfaces_in_app_config() {
    let date = wingspan_app::demo::valuation_date();
    let config = wingspan_app::demo::demo_config(wingspan_app::demo::scratch_dir());
    let options = wingspan_app::state::AppOptions {
        demo: true,
        root_override: Some(wingspan_app::demo::fixtures_root()),
        ..wingspan_desktop::boot::app_options_from(Some("2026-08-31"))
    };
    assert_eq!(options.valuation_date, Some(date));

    let slot = AppSlot::pending();
    let (_app, window) = app_with(slot.clone());
    tauri::async_runtime::handle().block_on(async {
        let state = wingspan_app::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let runtime = Runtime {
            background: wingspan_app::serve::spawn_background(&state),
            pump: tauri::async_runtime::handle().inner().spawn(async {}),
            loaded: loaded(&state.config),
            state,
            graph_gate: GraphGate::new(),
        };
        slot.publish(Ok(Arc::new(runtime)));
    });

    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");
    assert_eq!(body["resolved"]["valuation_date"], date.to_string());
}

/// A config file we could not read is the one note no `AppState` can carry:
/// the application booted on defaults instead, and `resolved.provider_notes`
/// is the only place the user ever sees that. `service::config` nests the
/// array inside `resolved`, so the append has to descend — reaching for a
/// top-level `provider_notes` is a silent no-op.
#[test]
fn a_startup_note_reaches_the_config_beside_the_provider_notes() {
    const NOTE: &str = "your config file could not be read, so defaults are in use";
    // Process-wide and set once, which is exactly its scope; no other test in
    // this binary records or asserts on it.
    wingspan_desktop::boot::record_startup_notes(vec![NOTE.to_string()]);

    let (_app, window, _slot) = booted();
    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");

    let notes = body["resolved"]["provider_notes"]
        .as_array()
        .unwrap_or_else(|| panic!("provider_notes lives under resolved: {body}"));
    assert!(
        notes.iter().any(|note| note == NOTE),
        "the startup note never arrived: {notes:?}"
    );
}

/// The trades-root trio go straight to the service's own `async` functions,
/// which already do their filesystem work off the runtime. A folder that is
/// not a trades folder is a sentence the user can act on, not a debug dump.
#[test]
fn probing_a_folder_that_is_not_a_trades_folder_is_a_sentence() {
    let (_app, window, _slot) = booted();
    let tmp = tempfile::tempdir().expect("a temp dir");
    let body = serde_json::json!({ "request": { "root": tmp.path() } });

    let rejection = invoke(&window, "probe_folder", body).expect_err("not a trades folder");
    assert_eq!(rejection["status"], 422);
    assert!(
        rejection["detail"]
            .as_str()
            .is_some_and(|s| s.contains("Open Trades")),
        "{rejection}"
    );
}

/// The round trip the SPA actually makes: one argument named `request`, a
/// real trade back.
#[test]
fn a_trade_round_trips_through_the_request_envelope() {
    let (_app, window, _slot) = booted();
    let trades = invoke(&window, "trades", json("{}")).expect("the book loads");
    let id = trades["trades"][0]["id"]
        .as_str()
        .expect("at least one open trade")
        .to_string();

    let body = serde_json::json!({ "request": { "id": id } });
    let trade = invoke(&window, "trade_by_id", body).expect("the trade answers");
    assert_eq!(trade["id"], id);
}

// ------------------------------------------------------------ Phase 8: ACL

/// The three path pickers open a GTK file chooser as their first statement
/// and take no argument that could fail deserialisation first, so — unlike
/// `pick_folder`, which has a `request` to reject — there is no way to prove
/// them reachable headlessly by invoking them. What can be proved is that
/// they are *declared*: `tauri_build` writes one `permissions/autogenerated/
/// <cmd>.toml` per name in `build.rs`'s `COMMANDS`, and the ACL refuses any
/// command that is not also listed in `capabilities/main.json`. Both halves
/// are asserted here, which is exactly what
/// `every_command_is_registered_under_the_name_the_spa_calls` proves for the
/// rest.
#[test]
fn the_three_path_pickers_are_declared_even_though_they_cannot_be_invoked() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let capability = std::fs::read_to_string(crate_dir.join("capabilities/main.json"))
        .expect("the capability file");
    for command in ["pick_python", "pick_env_file", "pick_token_file"] {
        let generated = crate_dir.join(format!("permissions/autogenerated/{command}.toml"));
        assert!(
            generated.is_file(),
            "{command} is missing from build.rs's COMMANDS: {} was never written",
            generated.display()
        );
        let allow = format!("allow-{}", command.replace('_', "-"));
        assert!(
            capability.contains(&allow),
            "capabilities/main.json does not list {allow}, so the ACL refuses {command}"
        );
    }

    // The one thing that must never appear: a blanket permission.
    assert!(
        !capability.contains("core:default"),
        "core:default hands the webview every core command"
    );
}

// ----------------------------------------------------------- Phase 8: demo

/// A second, smaller book in a temp dir, staged out of the fixtures exactly
/// as `crates/wingspan-app/tests/roots.rs` stages one. Nothing about it
/// matches the demo book, so which one is loaded is visible in the ids.
fn stage_folder(parent: &Path) -> PathBuf {
    let root = parent.join("Staged Trades");
    let fixtures = wingspan_app::demo::fixtures_root();
    for (dir, files) in [
        (
            "Open Trades",
            vec!["BSH July 14 2026", "M1 July 14 2026"] as Vec<&str>,
        ),
        ("Closed Trades", vec!["Delta 1 Aug 25 2026 Closed"]),
    ] {
        std::fs::create_dir_all(root.join(dir)).expect("the staged folder");
        for file in files {
            std::fs::copy(fixtures.join(dir).join(file), root.join(dir).join(file))
                .expect("a staged trade file");
        }
    }
    let statements = root.join("Daily Trade Statement");
    std::fs::create_dir_all(&statements).expect("the statements folder");
    for entry in std::fs::read_dir(fixtures.join("Daily Trade Statement")).expect("the fixtures") {
        let entry = entry.expect("a statement");
        std::fs::copy(entry.path(), statements.join(entry.file_name())).expect("a statement copy");
    }
    std::fs::canonicalize(&root).unwrap_or(root)
}

/// A config that can reach nothing: no yfinance subprocess, no Schwab, and a
/// state directory of this test's own. `leave_demo` rebuilds with
/// `demo: false`, which is the one path in these tests that builds the *real*
/// provider chain, so it has to be a chain with no network rung in it.
fn offline_config(state_dir: &Path) -> Config {
    let mut config = wingspan_app::demo::demo_config(state_dir.to_path_buf());
    config.trades.root = None;
    config.yfinance.enabled = false;
    config.market.spot_sources = vec!["cache".to_string()];
    config.market.option_sources = vec!["cache".to_string(), "statement".to_string()];
    config
}

fn open_trade_ids(window: &tauri::WebviewWindow<MockRuntime>) -> Vec<String> {
    let trades = invoke(window, "trades", json("{}")).expect("the book loads");
    let mut ids: Vec<String> = trades["trades"]
        .as_array()
        .expect("a trades array")
        .iter()
        .map(|trade| trade["id"].as_str().expect("a trade id").to_string())
        .collect();
    ids.sort();
    ids
}

/// **R14.** `AppState::build` reads `trades_root.json` *before* it looks at
/// `root_override`, so without `SavedRootPolicy::Ignore` a user who has chosen
/// a folder and clicks "Load demo book" gets their **own** positions priced
/// off the synthetic fixture provider, measured from a fixture day, and
/// reported as `source: "local"` — fabricated marks on a real book, from a
/// menu item. And leaving the demo has to put the real folder straight back,
/// with the record on disk never rewritten.
#[test]
fn load_demo_never_reads_the_saved_folder() {
    // `leave_demo` reads `WINGSPAN_AS_OF`, which is process-wide.
    let _unpinned = AS_OF_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let tmp = tempfile::tempdir().expect("a temp dir");
    let staged = stage_folder(tmp.path());
    let state_dir = wingspan_app::demo::scratch_dir();
    std::fs::create_dir_all(&state_dir).expect("the state dir");
    wingspan_app::saved_root::write(
        &state_dir,
        &wingspan_app::saved_root::Record::local(staged.clone(), chrono::Utc::now(), Vec::new()),
    )
    .expect("the saved record");
    let record_before =
        std::fs::read(wingspan_app::saved_root::path(&state_dir)).expect("the record file");

    // Demo *prices* (no network) but the ordinary saved-root policy, so the
    // app boots on the folder the user chose — the situation R14 is about.
    let options = wingspan_app::state::AppOptions {
        root_override: None,
        ..wingspan_app::demo::demo_options()
    };
    let (_app, window, _slot) = boot(offline_config(&state_dir), options);

    let root = invoke(&window, "trades_root", json("{}")).expect("the root answers");
    assert_eq!(root["source"], "local", "the saved folder should be loaded");
    assert_eq!(root["root"], staged.display().to_string());
    let staged_ids = open_trade_ids(&window);
    assert_eq!(staged_ids.len(), 2, "the staged book's own trades");

    // Load demo book.
    let demo = invoke(&window, "load_demo", json("{}")).expect("the demo loads");
    assert_eq!(
        demo["source"], "demo",
        "the demo showed the user's own book: {demo}"
    );
    assert_eq!(
        open_trade_ids(&window),
        ["nov-condor-2026-08-26", "oct-strangle-2026-08-24"],
        "the demo showed something other than the demo book"
    );

    // Leaving the demo rebuilds with `demo: false`, which is the one place in
    // these tests that builds the *real* provider chain — and this config's
    // chain is `cache` only, so that it can reach nothing. Hand the cache rung
    // the snapshot the demo run just wrote, or the rebuilt app has no spot at
    // all and `trades` is a 503 about market data rather than an answer about
    // which folder came back.
    let demo_cache = state_dir.join("demo").join("snapshot.json");
    assert!(
        demo_cache.is_file(),
        "the demo run left no snapshot at {}",
        demo_cache.display()
    );
    std::fs::copy(&demo_cache, state_dir.join("snapshot.json")).expect("seeding the cache rung");

    // Leave demo.
    let back = invoke(&window, "leave_demo", json("{}")).expect("the demo is left");
    assert_eq!(
        back["source"], "local",
        "the folder did not come back: {back}"
    );
    assert_eq!(back["root"], staged.display().to_string());
    assert_eq!(open_trade_ids(&window), staged_ids);
    assert_eq!(
        std::fs::read(wingspan_app::saved_root::path(&state_dir)).expect("the record file"),
        record_before,
        "the demo rewrote the saved record"
    );
}

/// `WINGSPAN_AS_OF` is a property of the *launch*, not of the book on screen.
///
/// Leaving the demo rebuilds the application, and rebuilding it from
/// `AppOptions::default()` would quietly hand back the system clock: every
/// DIT/DTE, every "has this leg expired" and the surface's own pricing clock
/// would move, with nothing in the window saying why. The smoke suite launches
/// pinned, so this is the ordinary case there rather than an exotic one.
#[test]
fn leaving_the_demo_keeps_the_launch_pin() {
    let pinned = wingspan_app::demo::valuation_date();
    let _guard = AS_OF_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    std::env::set_var(wingspan_desktop::boot::AS_OF_ENV, pinned.to_string());

    let tmp = tempfile::tempdir().expect("a temp dir");
    let staged = stage_folder(tmp.path());
    let state_dir = wingspan_app::demo::scratch_dir();
    std::fs::create_dir_all(&state_dir).expect("the state dir");
    wingspan_app::saved_root::write(
        &state_dir,
        &wingspan_app::saved_root::Record::local(staged.clone(), chrono::Utc::now(), Vec::new()),
    )
    .expect("the saved record");

    let options = wingspan_app::state::AppOptions {
        root_override: None,
        ..wingspan_app::demo::demo_options()
    };
    let (_app, window, _slot) = boot(offline_config(&state_dir), options);

    invoke(&window, "load_demo", json("{}")).expect("the demo loads");
    // The rebuilt real application prices off the cache rung and nothing else
    // — see `load_demo_never_reads_the_saved_folder`.
    let demo_cache = state_dir.join("demo").join("snapshot.json");
    std::fs::copy(&demo_cache, state_dir.join("snapshot.json")).expect("seeding the cache rung");
    invoke(&window, "leave_demo", json("{}")).expect("the demo is left");

    std::env::remove_var(wingspan_desktop::boot::AS_OF_ENV);

    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");
    assert_eq!(
        body["resolved"]["valuation_date"],
        pinned.to_string(),
        "leaving the demo re-valued the book on today rather than the pinned date: {body}"
    );
}

// ------------------------------------------------------- Phase 8: settings

/// `dirs::config_dir()` — and so `settings::settings_path()` — pointed at a
/// directory of this test binary's own, so nothing here can read or write the
/// developer's real `~/.config/wingspan/wingspan.toml`.
static CONFIG_HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
/// The two settings tests write the same file, so they take turns.
static SETTINGS_LOCK: Mutex<()> = Mutex::new(());
/// `WINGSPAN_AS_OF` is process-wide, and `leave_demo` reads it (that is the
/// point: the pin belongs to the launch, not to the book on screen). The
/// tests that set it and the tests that leave the demo take turns.
static AS_OF_LOCK: Mutex<()> = Mutex::new(());

fn private_config_home() -> PathBuf {
    CONFIG_HOME
        .get_or_init(|| {
            let dir = tempfile::tempdir().expect("a temp config home");
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            dir
        })
        .path()
        .to_path_buf()
}

/// **R20.** `SettingsPatch` is `deny_unknown_fields` and has no path key at
/// all, so a payload naming `python` — arbitrary code execution on every
/// refresh tick — is refused by serde *before the command body runs*. That
/// makes the rejection the bare argument-deserialisation string rather than a
/// `ProblemJson`, which is exactly why `client.ts` has to accept both shapes.
#[test]
fn settings_update_refuses_a_path_field() {
    let _turn = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let file = wingspan_app::settings::settings_path_in(&private_config_home());
    let before = std::fs::read(&file).ok();

    let (_app, window, _slot) = booted();
    let rejection = invoke(
        &window,
        "settings_update",
        json(r#"{"request": {"yfinance": {"python": "/tmp/evil"}}}"#),
    )
    .expect_err("a path field must not deserialise");
    assert!(
        rejection
            .as_str()
            .is_some_and(|s| s.contains("invalid args `request` for command `settings_update`")),
        "{rejection}"
    );

    assert_eq!(
        std::fs::read(&file).ok(),
        before,
        "{} was written by a refused patch",
        file.display()
    );
}

/// The write is `toml_edit`, atomic and `0600`: an untouched key keeps its
/// comment and its place, and the file a Schwab path lives in is never
/// world-readable.
#[test]
fn a_settings_write_preserves_comments_and_is_private() {
    use std::os::unix::fs::PermissionsExt as _;

    let _turn = SETTINGS_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = private_config_home();
    let file = wingspan_app::settings::settings_path_in(&home);
    std::fs::create_dir_all(file.parent().expect("a parent")).expect("the config dir");
    // A state dir of this test's own, because `settings_update` re-reads this
    // very file and rebuilds the application from it: the rebuild must not
    // run a first-boot migration over the developer's real state directory.
    let scratch = wingspan_app::demo::scratch_dir();
    std::fs::write(
        &file,
        format!(
            "# Wingspan settings — this comment must survive a save.\n\
             [market]\n\
             # how often we refresh while the session is open\n\
             refresh_secs_open = 15\n\
             state_dir = {:?}\n\
             \n\
             [yfinance]\n\
             enabled = false\n",
            scratch.display().to_string()
        ),
    )
    .expect("a settings file with comments");

    let (_app, window, _slot) = booted();
    let written = invoke(
        &window,
        "settings_update",
        json(r#"{"request": {"market": {"refresh_secs_open": 42}}}"#),
    )
    .expect("the patch is accepted");
    assert_eq!(written["file"], file.display().to_string());

    let body = std::fs::read_to_string(&file).expect("the settings file");
    assert!(
        body.contains("# Wingspan settings — this comment must survive a save."),
        "the header comment was lost: {body}"
    );
    assert!(
        body.contains("# how often we refresh while the session is open"),
        "the key's own comment was lost: {body}"
    );
    assert!(
        body.contains("refresh_secs_open = 42"),
        "the value was not written: {body}"
    );
    assert!(
        body.contains("enabled = false"),
        "an untouched key was dropped: {body}"
    );

    let mode = std::fs::metadata(&file)
        .expect("the settings file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "wingspan.toml is readable by someone else");
}

/// `settings_read` reports the running configuration, not one layer of it:
/// every field is `Some`, and the read-only paths sit beside it.
#[test]
fn settings_read_reports_the_resolved_config_and_the_read_only_paths() {
    let (_app, window, slot) = booted();
    let body = invoke(&window, "settings_read", json("{}")).expect("the settings answer");

    assert_eq!(body["settings"]["market"]["refresh_secs_open"], 1);
    assert_eq!(body["settings"]["trades"]["open_dir"], "Open Trades");
    assert!(
        body["settings"]["pricing"]["weighted_vega_reference"].is_string(),
        "{body}"
    );
    // Deliberately absent: a path key here would be a JS-writable interpreter.
    assert!(body["settings"]["yfinance"]["python"].is_null(), "{body}");

    let runtime = slot.peek().expect("a booted runtime");
    assert_eq!(
        body["paths"]["trades_root"],
        serde_json::Value::String(
            runtime
                .state
                .root()
                .dir()
                .expect("a root")
                .display()
                .to_string()
        )
    );
    assert!(body["paths"]["python"].is_string(), "{body}");
    assert!(body["paths"]["state_dir"].is_string(), "{body}");
    assert!(body["paths"]["log_dir"].is_string(), "{body}");
}

// ---------------------------------------------------- Phase 8: diagnostics

/// **The "one implementation, two renderers" gate.** Data health and
/// `wingspan-cli doctor` are the same checklist; the command is only a
/// serializer for it. A second implementation here would drift on the first
/// row anybody edits, so this asserts row for row against the library
/// function itself, on the very `Loaded` the app was built from.
#[test]
fn the_diagnostics_command_returns_the_same_rows_as_doctor() {
    let state_dir = wingspan_app::demo::scratch_dir();
    let config = offline_config(&state_dir);
    let options = wingspan_app::state::AppOptions {
        root_override: Some(wingspan_app::demo::fixtures_root()),
        ..wingspan_app::demo::demo_options()
    };
    let expected = tauri::async_runtime::handle()
        .block_on(async { wingspan_app::diagnostics::diagnostics(&loaded(&config), false).await });

    let (_app, window, _slot) = boot(config, options);
    let body = invoke(
        &window,
        "diagnostics",
        json(r#"{"request": {"probe": false}}"#),
    )
    .expect("the diagnostics answer");

    let rows = body["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), expected.len(), "a different number of rows");
    for (row, check) in rows.iter().zip(&expected) {
        let grade = match check.grade {
            wingspan_app::diagnostics::Grade::Ok => "ok",
            wingspan_app::diagnostics::Grade::Warn => "warn",
            wingspan_app::diagnostics::Grade::Fail => "fail",
            wingspan_app::diagnostics::Grade::Note => "note",
        };
        assert_eq!(row["grade"], grade, "{row}");
        assert_eq!(row["section"], check.section, "{row}");
        assert_eq!(row["name"], check.name, "{row}");
        assert_eq!(row["detail"], check.detail, "{row}");
    }
    assert!(
        body["discarded_uploads"].is_array(),
        "the card needs the list even when it is empty: {body}"
    );
}

/// Warn, never fail: a machine with no Python at all is a supported
/// configuration, so the probe answers `{ok, detail}` rather than rejecting.
#[test]
fn probe_python_warns_rather_than_failing() {
    let state_dir = wingspan_app::demo::scratch_dir();
    let mut config = offline_config(&state_dir);
    config.yfinance.enabled = true;
    config.yfinance.python = Some(PathBuf::from("/definitely/not/a/python"));
    let options = wingspan_app::state::AppOptions {
        root_override: Some(wingspan_app::demo::fixtures_root()),
        ..wingspan_app::demo::demo_options()
    };
    let (_app, window, _slot) = boot(config, options);

    let body = invoke(&window, "probe_python", json("{}")).expect("the probe answers");
    assert_eq!(body["ok"], false, "{body}");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("/definitely/not/a/python")),
        "the sentence should name the interpreter: {body}"
    );
}

// -------------------------------------------------------- Phase 8: refresh

/// `refresh_now` nudges the market loop rather than fetching itself, so the
/// assertion is that a *newer snapshot lands* — not that the command returned.
#[test]
fn refresh_now_asks_the_market_service() {
    let (_app, window, _slot) = booted();
    let before = as_of(&invoke(&window, "market_snapshot", json("{}")).expect("a snapshot"));

    let answered = invoke(&window, "refresh_now", json("{}")).expect("the nudge is accepted");
    assert!(answered.is_null(), "refresh_now answers null: {answered}");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut after = before;
    while Instant::now() < deadline {
        after = as_of(&invoke(&window, "market_snapshot", json("{}")).expect("a snapshot"));
        if after > before {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        after > before,
        "no newer snapshot arrived within 2 s: as_of stayed at {before}"
    );
}

// --------------------------------------------------------- Phase 8: reveal

/// `reveal_folder` is the only command that takes a path *and* reaches the
/// desktop, so it answers for exactly three paths the application already
/// knows. An XSS in the webview must not be able to use it as a general
/// "open anything" primitive.
#[test]
fn reveal_folder_refuses_a_path_that_is_not_one_of_the_three_it_knows() {
    let (_app, window, _slot) = booted();
    let tmp = tempfile::tempdir().expect("a temp dir");

    let rejection = invoke(
        &window,
        "reveal_folder",
        serde_json::json!({ "request": { "path": tmp.path() } }),
    )
    .expect_err("an arbitrary folder must be refused");
    assert_eq!(rejection["status"], 422, "{rejection}");
    assert!(
        rejection["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("not a folder Wingspan can open")),
        "{rejection}"
    );
}
