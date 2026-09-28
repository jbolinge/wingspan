//! The command surface, driven through the real IPC with the real ACL.
//!
//! Every trap this surface has hit is encoded here: the invoke URL must be
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
    ("trade_source", "{}"),
    (
        "probe_trade_source",
        r#"{"request": {"source": {"kind": "csv", "path": "/nowhere/legs.csv"}}}"#,
    ),
    (
        "set_trade_source",
        r#"{"request": {"source": {"kind": "csv", "path": "/nowhere/legs.csv"}}}"#,
    ),
    ("clear_trade_source", "{}"),
    // Settings, diagnostics, the demo book and the openers. The five
    // pickers are absent on purpose: they open a GTK file
    // chooser the moment they are called, with no argument that could fail
    // deserialisation first, so there is nothing to invoke headlessly. They
    // are proved declared instead — see
    // `the_five_path_pickers_are_declared_even_though_they_cannot_be_invoked`.
    ("load_demo", "{}"),
    ("leave_demo", "{}"),
    ("diagnostics", r#"{"request": {"probe": false}}"#),
    ("settings_read", "{}"),
    ("settings_update", r#"{"request": {}}"#),
    ("probe_python", "{}"),
    ("reveal_folder", r#"{"request": {"path": "/nowhere"}}"#),
    ("open_log_folder", "{}"),
    ("open_config_file", "{}"),
    ("open_sheet", "{}"),
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
    let rejection = invoke(
        &window,
        "reveal_folder",
        json(r#"{"request": {"path": "/nowhere"}}"#),
    )
    .expect_err("the boot failed");
    assert_eq!(rejection["detail"], SENTENCE);

    // A missing field is refused before the command body runs, and arrives as
    // a bare string rather than a ProblemJson — which is why `client.ts` has
    // to accept both.
    let rejection =
        invoke(&window, "reveal_folder", json(r#"{"request": {}}"#)).expect_err("path is required");
    assert_eq!(
        rejection,
        serde_json::json!(
            "invalid args `request` for command `reveal_folder`: missing field `path`"
        )
    );

    // Spread fields are not the envelope: `{"path": ...}` is not `request`.
    let rejection = invoke(&window, "reveal_folder", json(r#"{"path": "/nowhere"}"#))
        .expect_err("not the envelope");
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
/// The three trade-source commands call service functions that are already
/// `async`: the fetch-or-read work is on a blocking worker, but the tail —
/// building the DTO, forgetting the price cache — runs on the caller's task.
/// A panic there drops the responder just as surely, so those go through
/// [`wingspan_desktop::commands::off_runtime_async`], which puts a
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
    assert_eq!(options.secrets, secrets);
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
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: wingspan_app::demo::fixtures_root().join("legs.csv"),
        }),
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
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: wingspan_app::demo::fixtures_root().join("legs.csv"),
        }),
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

/// A config that would not parse, and an `env_file` that is not there, are
/// notes no `AppState` can carry: they belong to the `Loaded` the runtime was
/// built from, and `resolved.provider_notes` is the only place the user ever
/// sees them. `service::config` nests the array inside `resolved`, so the
/// append has to descend — reaching for a top-level `provider_notes` is a
/// silent no-op.
///
/// The second half is the regression: the notes are read off the *current*
/// runtime, so a rebuild onto a config whose `env_file` now exists — which is
/// exactly what the pickers do — stops reporting it as missing. Held in a
/// process-wide cell they would say "not configured" beside a working
/// provider for the life of the process.
#[test]
fn the_config_notes_reach_provider_notes_and_a_rebuild_refreshes_them() {
    const NOTE: &str = "[tastytrade] env_file /nowhere.env does not exist";
    let config = wingspan_app::demo::demo_config(wingspan_app::demo::scratch_dir());
    let options = wingspan_app::state::AppOptions {
        demo: true,
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: wingspan_app::demo::fixtures_root().join("legs.csv"),
        }),
        ..wingspan_app::state::AppOptions::default()
    };

    let slot = AppSlot::pending();
    let (app, window) = app_with(slot.clone());
    let handle = app.handle().clone();
    let state = tauri::async_runtime::handle().block_on(async {
        wingspan_app::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds")
    });

    // One `AppState`, two runtimes over it: the swap is the rebuild, and the
    // only thing that differs between them is the `Loaded` they carry.
    let publish = |notes: Vec<String>| {
        tauri::async_runtime::handle().block_on(async {
            let runtime = Runtime {
                background: wingspan_app::serve::spawn_background(&state),
                pump: wingspan_desktop::bridge::spawn(handle.clone(), state.clone()),
                graph_gate: GraphGate::new(),
                loaded: Loaded {
                    notes,
                    ..loaded(&state.config)
                },
                state: state.clone(),
            };
            slot.publish(Ok(Arc::new(runtime)));
        });
    };

    publish(vec![NOTE.to_string()]);
    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");
    let notes = body["resolved"]["provider_notes"]
        .as_array()
        .unwrap_or_else(|| panic!("provider_notes lives under resolved: {body}"));
    assert!(
        notes.iter().any(|note| note == NOTE),
        "the config note never arrived: {notes:?}"
    );

    publish(Vec::new());
    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");
    let notes = body["resolved"]["provider_notes"]
        .as_array()
        .unwrap_or_else(|| panic!("provider_notes lives under resolved: {body}"));
    assert!(
        !notes.iter().any(|note| note == NOTE),
        "the rebuilt runtime still reports the stale note: {notes:?}"
    );
}

/// The trade-source trio go straight to the service's own `async` functions,
/// which already do their fetch-or-read work off the runtime: a file that
/// cannot even be read as a Legs table is a sentence the user can act on,
/// not a debug dump.
///
/// A CSV whose header simply lacks a required column (`a,b,c`) does **not**
/// refuse here — `probe_trade_source` persists and loads nothing, and its
/// whole point is to report `columns_missing` rather than decline to answer
/// (`docs/IPC.md`: "non-empty here means the import would be refused" is
/// about `set_trade_source`, the two-step commit's second half — only
/// [`wingspan_app::source::refuse_if_columns_missing`], called from `load`,
/// enforces that). What genuinely refuses at the probe step is a file this
/// app cannot read as text at all, which is what this test uses.
#[test]
fn probing_a_csv_that_is_not_a_legs_table_is_a_sentence() {
    let (_app, window, _slot) = booted();
    let tmp = tempfile::tempdir().expect("a temp dir");
    let file = tmp.path().join("not-legs.csv");
    std::fs::write(&file, [0xFF, 0xFE, 0x00, 0xFF]).expect("a non-utf8 file");
    // Stand in for the GTK chooser, which cannot run headless: this is a path
    // the user picked and confirmed, so it gets past the provenance gate and
    // reaches the parser this test is actually about.
    wingspan_desktop::picker::remember_confirmed(&file);
    let body = serde_json::json!({
        "request": { "source": { "kind": "csv", "path": file } }
    });

    let rejection = invoke(&window, "probe_trade_source", body).expect_err("not a Legs table");
    assert_eq!(rejection["status"], 422, "{rejection}");
    assert!(
        rejection["type"]
            .as_str()
            .is_some_and(|s| s.ends_with("invalid-trade-source")),
        "{rejection}"
    );
}

/// **The webview does not get to name a file.**
///
/// `probe_trade_source` and `set_trade_source` take a path straight off the
/// IPC payload, and a probe echoes header text back (`columns_ignored` is the
/// raw trimmed text of every unrecognised header cell), so without a
/// provenance gate a script running in the webview could read the first line
/// of anything the user can read — `~/.ssh/id_ed25519` included — and
/// `set_trade_source` would then copy the whole file into the state dir's
/// cache. `docs/IPC.md`'s "path only ever from `pick_csv_file`" has to be a
/// rule Rust enforces, not a comment.
///
/// The folder era's `probe_folder` answered only counts; this is the one
/// command that could have become a general read primitive.
#[test]
fn a_csv_path_the_picker_never_confirmed_is_refused() {
    let (_app, window, _slot) = booted();
    let tmp = tempfile::tempdir().expect("a temp dir");
    // A perfectly valid Legs CSV — the refusal is about provenance, not
    // content, or an attacker could simply point at a file that parses.
    let file = stage_legs_csv(tmp.path());

    for command in ["probe_trade_source", "set_trade_source"] {
        let body = serde_json::json!({
            "request": { "source": { "kind": "csv", "path": file } }
        });
        let rejection = invoke(&window, command, body).expect_err("an unnamed path must not read");
        assert_eq!(rejection["status"], 422, "{command}: {rejection}");
        assert!(
            rejection["type"]
                .as_str()
                .is_some_and(|s| s.ends_with("invalid-trade-source")),
            "{command}: {rejection}"
        );
        assert!(
            rejection["detail"]
                .as_str()
                .is_some_and(|s| s.contains("file chooser")),
            "{command}: the sentence does not say how to fix it: {rejection}"
        );
    }

    // And the same path, once picked and confirmed, imports normally.
    wingspan_desktop::picker::remember_confirmed(&file);
    let body = serde_json::json!({
        "request": { "source": { "kind": "csv", "path": file } }
    });
    let probe = invoke(&window, "probe_trade_source", body).expect("a confirmed pick is admitted");
    assert!(
        probe["columns_missing"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "{probe}"
    );
}

/// A sheet request needs no gate — `sheet_url::parse` refuses anything that
/// is not a `docs.google.com` spreadsheet — so the gate must not accidentally
/// swallow one on its way past.
#[test]
fn a_sheet_request_is_not_touched_by_the_csv_gate() {
    let (_app, window, _slot) = booted();
    let body = serde_json::json!({
        "request": { "source": { "kind": "sheet", "url": "not a link at all" } }
    });
    let rejection = invoke(&window, "probe_trade_source", body).expect_err("not a sheet link");
    assert_eq!(rejection["status"], 422, "{rejection}");
    assert!(
        rejection["detail"]
            .as_str()
            .is_some_and(|s| !s.contains("file chooser")),
        "a sheet was refused with the CSV gate's sentence: {rejection}"
    );
}

/// The share link is bearer-equivalent and must never reach the settings
/// card, whatever the active source is.
#[test]
fn the_source_status_block_never_carries_the_spreadsheet_id() {
    let (_app, window, _slot) = booted();
    let body = invoke(&window, "settings_read", json("{}")).expect("the settings answer");

    if let Some(status) = body["source_status"].as_object() {
        let host = status
            .get("host")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        assert!(
            host.is_null() || host == serde_json::json!("docs.google.com"),
            "{status:?}"
        );
    }

    let serialised = serde_json::to_string(&body).expect("the body serialises");
    assert!(
        !serialised.contains("/spreadsheets/d/"),
        "a spreadsheet link leaked into settings_read: {serialised}"
    );
}

// ---------------------------------------------------------------- ACL

/// The five path pickers open a GTK file chooser as their first statement
/// and take no argument that could fail deserialisation first, so there is
/// no way to prove them reachable headlessly by invoking them. What can be
/// proved is that
/// they are *declared*: `tauri_build` writes one `permissions/autogenerated/
/// <cmd>.toml` per name in `build.rs`'s `COMMANDS`, and the ACL refuses any
/// command that is not also listed in `capabilities/main.json`. Both halves
/// are asserted here, which is exactly what
/// `every_command_is_registered_under_the_name_the_spa_calls` proves for the
/// rest. That they are dispatched off the IPC thread — the bug that made
/// them unusable in 0.2.0 — is pinned separately, by
/// `every_picker_is_async_so_it_never_runs_on_the_ipc_thread`.
#[test]
fn the_five_path_pickers_are_declared_even_though_they_cannot_be_invoked() {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let capability = std::fs::read_to_string(crate_dir.join("capabilities/main.json"))
        .expect("the capability file");
    for command in [
        "pick_python",
        "pick_env_file",
        "pick_token_file",
        "pick_tastytrade_env_file",
        "pick_csv_file",
    ] {
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

/// **The 0.2.0 "Choose folder…" hang, guarded at compile time.**
///
/// A plain `#[tauri::command]` on a non-`async fn` is not run on a worker
/// thread: `tauri-macros` calls it inline on the thread that delivered the
/// IPC request, which on Linux is the GTK main thread. A picker that opens a
/// chooser there and then waits for it is waiting for the one thread that
/// has to build it, and the application wedges for good — which is exactly
/// what the shipped 0.2.0 `.deb` did.
///
/// Nothing on this box can open a chooser to prove that (no display, and the
/// mock runtime never registers the dialog plugin), so the guard is the shape
/// instead: each picker is called and its return value handed to something
/// that only accepts a `Future`. A sync picker returns a `Result`, and this
/// test stops compiling. Belt and braces, a sync picker would also *run* on
/// this line and panic on the unmanaged `Dialog` state; an `async fn` body
/// does not start until it is polled, and these futures never are.
#[test]
fn every_picker_is_async_so_it_never_runs_on_the_ipc_thread() {
    fn only_a_future<F: std::future::Future>(_: F) {}

    let (app, _window) = failed_app();
    let handle = app.handle().clone();
    only_a_future(wingspan_desktop::settings_commands::pick_python(
        handle.clone(),
    ));
    only_a_future(wingspan_desktop::settings_commands::pick_env_file(
        handle.clone(),
    ));
    only_a_future(wingspan_desktop::settings_commands::pick_token_file(
        handle.clone(),
    ));
    only_a_future(wingspan_desktop::settings_commands::pick_tastytrade_env_file(handle.clone()));
    only_a_future(wingspan_desktop::picker::pick_csv_file(handle));
}

/// The same rule for every command, including ones not written yet.
///
/// The compile-time guard above names five functions; this one reads the
/// crate. Every `#[tauri::command]` in `src/` must sit on an `async fn`, or
/// carry the `(async)` attribute that spawns a sync body onto a tokio worker.
/// Neither shape can run inline on the IPC thread, which is the whole point.
#[test]
fn no_command_is_dispatched_inline_on_the_ipc_thread() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut checked = 0_usize;
    for entry in std::fs::read_dir(&src).expect("src/ is readable") {
        let path = entry.expect("a directory entry").path();
        // The walk is flat on purpose, so the day a module directory
        // appears this says so instead of silently skipping it.
        assert!(
            path.is_file(),
            "{} is not a file; teach this test to walk into it",
            path.display()
        );
        if path.extension().and_then(std::ffi::OsStr::to_str) != Some("rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a source file");
        let lines: Vec<&str> = text.lines().map(str::trim_start).collect();
        for (n, line) in lines.iter().enumerate() {
            if !line.starts_with("#[tauri::command") {
                continue;
            }
            checked += 1;
            // `#[tauri::command(async)]` on a sync fn is the other legal
            // shape: it lands on a tokio worker, not on the IPC thread.
            if line.contains("async") {
                continue;
            }
            // Whatever follows the attribute, past any further attributes
            // and comments, is the signature.
            let signature = lines[n + 1..]
                .iter()
                .find(|l| !(l.starts_with('#') || l.starts_with("//") || l.is_empty()))
                .unwrap_or_else(|| panic!("{}:{}: nothing follows", path.display(), n + 1));
            assert!(
                signature.contains("async fn "),
                "{}:{}: `{signature}` is a plain `#[tauri::command]` on a sync fn, so Tauri \
                 runs it inline on the IPC thread — the GTK main thread on Linux. Make it \
                 `async fn` and hand any blocking work to `commands::spawned_blocking`; \
                 `#[tauri::command(async)]` is the last resort, since it parks a tokio \
                 worker rather than the blocking pool. See the header of src/picker.rs.",
                path.display(),
                n + 2
            );
        }
    }
    // A floor with room to delete a few commands, not today's exact count.
    assert!(
        checked >= 25,
        "only {checked} commands found; this test stopped reading the crate"
    );
}

// --------------------------------------------------------------- demo

/// A copy of `fixtures/trades/legs.csv`, staged under `parent` with a
/// distinguishable name so the loaded book is tellable from the real
/// fixtures directory — never a symlink, and never sourced from
/// `~/workspace/example_trades`.
fn stage_legs_csv(parent: &Path) -> PathBuf {
    std::fs::create_dir_all(parent).expect("the staging dir");
    let staged = parent.join("staged-legs.csv");
    std::fs::copy(
        wingspan_app::demo::fixtures_root().join("legs.csv"),
        &staged,
    )
    .expect("a staged legs csv");
    std::fs::canonicalize(&staged).unwrap_or(staged)
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

/// **R14.** `AppState::build` reads `trade_source.json` *before* it looks at
/// `source_override`, so without `SavedSourcePolicy::Ignore` a user who has
/// imported a CSV and clicks "Load demo book" gets their **own** positions
/// priced off the synthetic fixture provider, measured from a fixture day,
/// and reported as `kind: "csv"` — fabricated marks on a real book, from a
/// menu item. And leaving the demo has to put the real source straight back,
/// with the record on disk never rewritten.
#[test]
fn load_demo_never_reads_the_saved_source() {
    // `leave_demo` reads `WINGSPAN_AS_OF`, which is process-wide.
    let _unpinned = AS_OF_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
    let tmp = tempfile::tempdir().expect("a temp dir");
    let staged = stage_legs_csv(tmp.path());
    let state_dir = wingspan_app::demo::scratch_dir();
    std::fs::create_dir_all(&state_dir).expect("the state dir");
    wingspan_app::saved_source::write(
        &state_dir,
        &wingspan_app::saved_source::Record::chosen(
            wingspan_app::source::SourceRequest::Csv {
                path: staged.clone(),
            },
            chrono::Utc::now(),
            Vec::new(),
        ),
    )
    .expect("the saved record");
    let record_before =
        std::fs::read(wingspan_app::saved_source::path(&state_dir)).expect("the record file");

    // Demo *prices* (no network) but the ordinary saved-source policy, so the
    // app boots on the CSV the user imported — the situation R14 is about.
    let options = wingspan_app::state::AppOptions {
        source_override: None,
        ..wingspan_app::demo::demo_options()
    };
    let (_app, window, _slot) = boot(offline_config(&state_dir), options);

    let source = invoke(&window, "trade_source", json("{}")).expect("the source answers");
    assert_eq!(source["kind"], "csv", "the saved csv should be loaded");
    assert_eq!(source["path"], staged.display().to_string());
    let staged_ids = open_trade_ids(&window);
    assert_eq!(staged_ids.len(), 6, "the staged book's own open trades");

    // Load demo book.
    let demo = invoke(&window, "load_demo", json("{}")).expect("the demo loads");
    assert_eq!(
        demo["kind"], "demo",
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
    // which source came back.
    let demo_cache = state_dir.join("demo").join("snapshot.json");
    assert!(
        demo_cache.is_file(),
        "the demo run left no snapshot at {}",
        demo_cache.display()
    );
    std::fs::copy(&demo_cache, state_dir.join("snapshot.json")).expect("seeding the cache rung");

    // Leave demo.
    let back = invoke(&window, "leave_demo", json("{}")).expect("the demo is left");
    assert_eq!(back["kind"], "csv", "the csv did not come back: {back}");
    assert_eq!(back["path"], staged.display().to_string());
    assert_eq!(open_trade_ids(&window), staged_ids);
    assert_eq!(
        std::fs::read(wingspan_app::saved_source::path(&state_dir)).expect("the record file"),
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
    let staged = stage_legs_csv(tmp.path());
    let state_dir = wingspan_app::demo::scratch_dir();
    std::fs::create_dir_all(&state_dir).expect("the state dir");
    wingspan_app::saved_source::write(
        &state_dir,
        &wingspan_app::saved_source::Record::chosen(
            wingspan_app::source::SourceRequest::Csv {
                path: staged.clone(),
            },
            chrono::Utc::now(),
            Vec::new(),
        ),
    )
    .expect("the saved record");

    let options = wingspan_app::state::AppOptions {
        source_override: None,
        ..wingspan_app::demo::demo_options()
    };
    let (_app, window, _slot) = boot(offline_config(&state_dir), options);

    invoke(&window, "load_demo", json("{}")).expect("the demo loads");
    // The rebuilt real application prices off the cache rung and nothing else
    // — see `load_demo_never_reads_the_saved_source`.
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

// ----------------------------------------------------------- settings

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
    assert_eq!(body["settings"]["trades"]["tab"], "Legs");
    assert_eq!(body["settings"]["trades"]["poll_secs"], 0);
    assert_eq!(
        body["settings"]["pricing"]["weighted_vega_reference"], "30d",
        "the default reference is a fixed 30 days: {body}"
    );
    // Deliberately absent: a path key here would be a JS-writable interpreter.
    assert!(body["settings"]["yfinance"]["python"].is_null(), "{body}");

    let runtime = slot.peek().expect("a booted runtime");
    assert_eq!(
        body["paths"]["source_label"],
        match runtime.state.published().source.label() {
            Some(label) => serde_json::Value::String(label),
            None => serde_json::Value::Null,
        }
    );
    assert!(body["paths"]["python"].is_string(), "{body}");
}

/// The tastytrade half of `settings_read`: the variable *names* round-trip
/// through `settings`, the credentials file sits in `paths`, and the status is
/// `null` while the provider is off — which is the default, so an install that
/// has never heard of tastytrade sees nothing at all.
///
/// No value of any credential appears anywhere in the body. That is asserted
/// on the serialized JSON rather than field by field, so a field added later
/// cannot smuggle one in.
#[test]
fn settings_read_reports_the_tastytrade_paths_and_status() {
    let (_app, window, _slot) = booted();
    let body = invoke(&window, "settings_read", json("{}")).expect("the settings answer");

    assert_eq!(body["settings"]["tastytrade"]["enabled"], false);
    assert_eq!(
        body["settings"]["tastytrade"]["client_secret_env"],
        "TASTYTRADE_CLIENT_SECRET"
    );
    assert_eq!(
        body["settings"]["tastytrade"]["refresh_token_env"],
        "TASTYTRADE_REFRESH_TOKEN"
    );
    // Deliberately absent: a path key here would be a JS-writable credentials
    // file. It lives in `paths`, and only a picker writes it.
    assert!(
        body["settings"]["tastytrade"]["env_file"].is_null(),
        "{body}"
    );
    // The test config names no `[tastytrade] env_file`, and the provider is
    // off, so both the path and the status are absent rather than invented.
    assert!(body["paths"]["tastytrade_env_file"].is_null(), "{body}");
    assert!(body["tastytrade_status"].is_null(), "{body}");
    // `token_status` is still Schwab's, under its frozen name.
    assert!(body.get("token_status").is_some(), "{body}");
}

// ---------------------------------------------------------- diagnostics

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
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: wingspan_app::demo::fixtures_root().join("legs.csv"),
        }),
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
        source_override: Some(wingspan_app::source::SourceRef::Csv {
            path: wingspan_app::demo::fixtures_root().join("legs.csv"),
        }),
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

// ------------------------------------------------------------- reveal

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
