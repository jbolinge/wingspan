//! The command surface, driven through the real IPC with the real ACL.
//!
//! Every trap the Phase 0 spike hit is encoded here: the invoke URL must be
//! the platform's *local* origin (`tauri://localhost`; the docs'
//! `http://tauri.localhost` is the Windows one and the ACL refuses everything
//! with "not allowed. Plugin not found"), the config's windows are not
//! materialised by `build()` so the webview is created here with the label
//! `capabilities/main.json` lists, and `generate_context!()` rather than
//! `mock_context` wherever the assertion is about the manifest.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tauri::test::MockRuntime;
use tauri::webview::InvokeRequest;
use wingspan_desktop::runtime::{AppSlot, GraphGate, Runtime};
use wingspan_server::error::{ApiResult, ProblemJson};

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
    assert_eq!(body["version"], wingspan_server::VERSION);
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
        if *cmd == "health" {
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

    std::env::set_var(wingspan_desktop::boot::AS_OF_ENV, "2026-08-31");
    let options = wingspan_desktop::boot::app_options();
    std::env::remove_var(wingspan_desktop::boot::AS_OF_ENV);

    assert_eq!(options.valuation_date, Some(date));
    assert_eq!(
        options.clock.now(),
        wingspan_server::demo::clock_at(date).now(),
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
    let mut config = wingspan_server::demo::demo_config(wingspan_server::demo::scratch_dir());
    // Fast enough that "two refresh intervals" is a test, not a nap.
    config.market.refresh_secs_open = 1;
    config.market.refresh_secs_closed = 1;
    let options = wingspan_server::state::AppOptions {
        demo: true,
        root_override: Some(wingspan_server::demo::fixtures_root()),
        // Deliberately the *system* clock: the fixture provider stamps every
        // snapshot with `clock.now()`, so a pinned clock would make `as_of`
        // constant and the R3 test below unable to fail.
        ..wingspan_server::state::AppOptions::default()
    };

    let slot = AppSlot::pending();
    let (app, window) = app_with(slot.clone());
    let handle = app.handle().clone();

    tauri::async_runtime::handle().block_on(async {
        let state = wingspan_server::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let runtime = Runtime {
            background: wingspan_server::serve::spawn_background(&state),
            pump: wingspan_desktop::bridge::spawn(handle, state.clone()),
            state,
            graph_gate: GraphGate::new(),
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
    let date = wingspan_server::demo::valuation_date();
    let config = wingspan_server::demo::demo_config(wingspan_server::demo::scratch_dir());
    let options = wingspan_server::state::AppOptions {
        demo: true,
        root_override: Some(wingspan_server::demo::fixtures_root()),
        ..wingspan_desktop::boot::app_options_from(Some("2026-08-31"))
    };
    assert_eq!(options.valuation_date, Some(date));

    let slot = AppSlot::pending();
    let (_app, window) = app_with(slot.clone());
    tauri::async_runtime::handle().block_on(async {
        let state = wingspan_server::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let runtime = Runtime {
            background: wingspan_server::serve::spawn_background(&state),
            pump: tauri::async_runtime::handle().inner().spawn(async {}),
            state,
            graph_gate: GraphGate::new(),
        };
        slot.publish(Ok(Arc::new(runtime)));
    });

    let body = invoke(&window, "app_config", json("{}")).expect("the config answers");
    assert_eq!(body["resolved"]["valuation_date"], date.to_string());
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
