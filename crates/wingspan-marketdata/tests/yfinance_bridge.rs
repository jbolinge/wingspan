//! The yfinance sidecar, without touching Yahoo.
//!
//! Two layers are tested here, both offline:
//!
//! 1. the **Rust wrapper** against fake interpreters (`/bin/sh` running a
//!    canned script) — the plumbing, the timeout, and the precise failure
//!    reasons the status pill shows;
//! 2. the **real `bridge/yfinance_bridge.py`**, run by the real `python3`
//!    against a stub `yfinance` module planted on `PYTHONPATH`. That is the
//!    only way to know the script's chain walking and matching actually work
//!    without asking Yahoo for anything.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rust_decimal_macros::dec;
use serde_json::Value;
use wingspan_core::contract::{OptionContract, Right, Settlement};
use wingspan_marketdata::yfinance::{BridgeError, BRIDGE_SCRIPT, YFINANCE_PIN};
use wingspan_marketdata::{
    ChainExpiry, ExpiryKind, Health, OptionQuoteSource, SpotSource, YFinance, YFinanceConfig,
};

fn contract(strike: i64, right: Right, expiry: (i32, u32, u32)) -> OptionContract {
    OptionContract {
        underlying: "SPX".into(),
        root: "SPX".into(),
        expiry: chrono::NaiveDate::from_ymd_opt(expiry.0, expiry.1, expiry.2).unwrap(),
        settlement: Settlement::Am,
        strike: rust_decimal::Decimal::from(strike),
        right,
        multiplier: 100,
    }
}

/// A "python" that is really `/bin/sh` running `body`.
fn fake(dir: &Path, body: &str) -> YFinanceConfig {
    let script = dir.join("fake_bridge.sh");
    std::fs::write(&script, body).unwrap();
    YFinanceConfig::new(dir)
        .with_python("/bin/sh")
        .with_script(script)
}

fn ok_response() -> &'static str {
    r#"{"ok":true,"as_of":"2026-08-31T17:00:00+00:00",
        "spot":{"symbol":"SPX","last":7752.34,"change":-12.5,"previous_close":7764.84},
        "quotes":[
          {"key":"SPX270219P7325","bid":155.0,"ask":156.0,"last":155.4,"mark":null,"iv":0.2175},
          {"key":"SPX270219C8200","bid":170.0,"ask":172.0,"last":171.0,"mark":171.0,"iv":0.148}],
        "missing":["SPX270219P9999"],"warnings":["^SPX has no 2027-03-19 expiry listed"]}"#
}

#[tokio::test]
async fn a_successful_round_trip_maps_quotes_and_stamps_the_delay() {
    let dir = tempfile::tempdir().unwrap();
    // The fake records what it was sent, then answers.
    let request_log = dir.path().join("request.json");
    let body = format!(
        "cat > {} \ncat <<'JSON'\n{}\nJSON\n",
        request_log.display(),
        ok_response()
    );
    let yf = YFinance::new(fake(dir.path(), &body));

    let contracts = vec![
        contract(7325, Right::Put, (2027, 2, 19)),
        contract(8200, Right::Call, (2027, 2, 19)),
    ];
    let quotes = yf.quotes(&contracts).await.unwrap();
    assert_eq!(quotes.len(), 2);

    let put = &quotes[0];
    assert_eq!(put.contract, contracts[0]);
    assert_eq!(put.bid, Some(dec!(155)));
    assert_eq!(put.ask, Some(dec!(156)));
    // No provider mark: the mid stands in.
    assert_eq!(put.mark, Some(dec!(155.5)));
    assert_eq!(put.iv_hint, Some(0.2175));
    assert!(put.greeks_hint.is_none(), "Yahoo has no greeks");
    // Stamped fifteen minutes before the fetch, so the staleness chip errs
    // towards telling the user the data is old.
    assert_eq!(put.as_of.to_rfc3339(), "2026-08-31T16:45:00+00:00");
    assert_eq!(quotes[1].mark, Some(dec!(171)));

    // The request the script received names contracts by TOS code, with the
    // Yahoo symbol resolved.
    let sent: Value = serde_json::from_slice(&std::fs::read(&request_log).unwrap()).unwrap();
    assert_eq!(sent["underlying"], "SPX");
    assert_eq!(sent["yahoo_symbol"], "^SPX");
    assert_eq!(sent["want_spot"], false);
    assert_eq!(sent["contracts"][0]["key"], "SPX270219P7325");
    assert_eq!(sent["contracts"][0]["expiry"], "2027-02-19");
    assert_eq!(sent["contracts"][0]["strike"], 7325.0);
    assert_eq!(sent["contracts"][0]["right"], "P");

    // Healthy, but never claiming to be live.
    match OptionQuoteSource::health(&yf) {
        Health::Degraded(reason) => assert!(reason.contains("~15 min delayed"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn the_spot_comes_back_with_its_day_change() {
    let dir = tempfile::tempdir().unwrap();
    let yf = YFinance::new(fake(
        dir.path(),
        &format!("cat > /dev/null\ncat <<'JSON'\n{}\nJSON\n", ok_response()),
    ));
    let spot = yf.spot("SPX").await.unwrap();
    assert_eq!(spot.symbol, "SPX");
    assert_eq!(spot.last, dec!(7752.34));
    assert_eq!(spot.change, Some(dec!(-12.5)));
}

#[tokio::test]
async fn a_spot_of_zero_is_no_spot_at_all() {
    // Yahoo answers 0 for an index between sessions often enough to matter:
    // publishing it would stop the fallback chain here, on a price of 0.
    let dir = tempfile::tempdir().unwrap();
    let response = r#"{"ok":true,"as_of":"2026-08-31T17:00:00+00:00",
        "spot":{"symbol":"SPX","last":0.0,"change":null,"previous_close":7764.84},
        "quotes":[],"missing":[],"warnings":[]}"#;
    let yf = YFinance::new(fake(
        dir.path(),
        &format!("cat > /dev/null\ncat <<'JSON'\n{response}\nJSON\n"),
    ));
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("no price for SPX"), "{err}");
}

#[tokio::test]
async fn a_missing_interpreter_says_so_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let config = YFinanceConfig::new(dir.path())
        .with_python("/nonexistent/bin/python")
        .with_script(dir.path().join("unused.py"));
    let yf = YFinance::new(config);
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(
        err.to_string()
            .contains("python not found at `/nonexistent/bin/python`"),
        "{err}"
    );
    assert!(err.to_string().contains("[yfinance] python"), "{err}");
    assert!(matches!(SpotSource::health(&yf), Health::Unavailable(_)));
}

#[tokio::test]
async fn the_three_bridge_failure_kinds_are_distinguishable() {
    /// script body, predicate on the error kind, a phrase the user must see
    type Case = (&'static str, fn(&BridgeError) -> bool, &'static str);
    let cases: [Case; 3] = [
        (
            r#"cat > /dev/null; echo '{"ok":false,"error":{"kind":"yfinance_missing","message":"import yfinance failed"}}'"#,
            |e| matches!(e, BridgeError::YfinanceMissing { .. }),
            "pip install",
        ),
        (
            r#"cat > /dev/null; echo '{"ok":false,"error":{"kind":"yahoo_error","message":"HTTPError: 429"}}'"#,
            |e| matches!(e, BridgeError::Yahoo(_)),
            "429",
        ),
        (
            r#"cat > /dev/null; echo '{"ok":false,"error":{"kind":"bad_request","message":"stdin is not JSON"}}'"#,
            |e| matches!(e, BridgeError::BadRequest(_)),
            "not JSON",
        ),
    ];
    for (body, matches_kind, needle) in cases {
        let dir = tempfile::tempdir().unwrap();
        let yf = YFinance::new(fake(dir.path(), body));
        let err = yf.spot("SPX").await.unwrap_err();
        let wingspan_marketdata::MarketDataError::Bridge(bridge) = err else {
            panic!("expected a bridge error, got {err}");
        };
        assert!(matches_kind(&bridge), "wrong kind: {bridge}");
        assert!(bridge.to_string().contains(needle), "{bridge}");
    }
}

#[tokio::test]
async fn an_import_failure_before_our_handler_is_still_recognised() {
    let dir = tempfile::tempdir().unwrap();
    // What a bare `import yfinance` at module scope actually does.
    let yf = YFinance::new(fake(
        dir.path(),
        "cat > /dev/null\necho \"ModuleNotFoundError: No module named 'yfinance'\" >&2\nexit 1\n",
    ));
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(
        err.to_string().contains("has no yfinance installed"),
        "{err}"
    );
}

#[tokio::test]
async fn a_hung_bridge_is_killed_at_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = fake(dir.path(), "cat > /dev/null\nsleep 30\n");
    config.timeout = Duration::from_millis(150);
    let yf = YFinance::new(config);
    let started = std::time::Instant::now();
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("timed out after 0.15s"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it was not killed"
    );
}

#[tokio::test]
async fn a_crash_and_a_garbled_answer_are_told_apart() {
    let dir = tempfile::tempdir().unwrap();
    let yf = YFinance::new(fake(
        dir.path(),
        "cat > /dev/null\necho 'segfault-ish' >&2\nexit 3\n",
    ));
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("exited with code 3"), "{err}");
    assert!(err.to_string().contains("segfault-ish"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    let yf = YFinance::new(fake(
        dir.path(),
        "cat > /dev/null\necho 'not json at all'\n",
    ));
    let err = yf.spot("SPX").await.unwrap_err();
    assert!(err.to_string().contains("unreadable output"), "{err}");
}

#[tokio::test]
async fn chatter_on_stdout_before_the_json_is_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let yf = YFinance::new(fake(
        dir.path(),
        &format!(
            "cat > /dev/null\necho 'FutureWarning: something'\ncat <<'JSON'\n{}\nJSON\n",
            ok_response()
        ),
    ));
    assert_eq!(yf.spot("SPX").await.unwrap().last, dec!(7752.34));
}

// ------------------------------------------------------- the option chain

/// A fake bridge that records its request and answers with `response`.
fn answering(dir: &Path, response: &str) -> (YFinanceConfig, PathBuf) {
    let request_log = dir.join("request.json");
    let body = format!(
        "cat > {} \ncat <<'JSON'\n{response}\nJSON\n",
        request_log.display()
    );
    (fake(dir, &body), request_log)
}

fn section(root: &str, settlement: Settlement) -> ChainExpiry {
    ChainExpiry {
        expiry: chrono::NaiveDate::from_ymd_opt(2026, 10, 16).unwrap(),
        root: root.into(),
        settlement,
        kind: ExpiryKind::Monthly,
    }
}

#[tokio::test]
async fn expirations_asks_for_the_list_and_infers_roots_for_the_listing_only() {
    let dir = tempfile::tempdir().unwrap();
    let (config, request_log) = answering(
        dir.path(),
        r#"{"ok":true,"as_of":"2026-10-01T17:00:00+00:00","spot":null,"quotes":[],
            "missing":[],"warnings":[],"expirations":["2026-10-02","2026-10-16"]}"#,
    );
    let yf = YFinance::new(config);

    let listed = yf.expirations("SPX").await.unwrap();
    let summary: Vec<_> = listed
        .iter()
        .map(|e| (e.expiry.to_string(), e.root.clone(), e.settlement, e.kind))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "2026-10-02".to_string(),
                "SPXW".to_string(),
                Settlement::Pm,
                ExpiryKind::Weekly
            ),
            (
                "2026-10-16".to_string(),
                "SPX".to_string(),
                Settlement::Am,
                ExpiryKind::Monthly
            ),
            (
                "2026-10-16".to_string(),
                "SPXW".to_string(),
                Settlement::Pm,
                ExpiryKind::Monthly
            ),
        ]
    );

    let sent: Value = serde_json::from_slice(&std::fs::read(&request_log).unwrap()).unwrap();
    assert_eq!(sent["yahoo_symbol"], "^SPX");
    assert_eq!(sent["list_expirations"], true);
    assert_eq!(sent["want_spot"], false);
    assert!(sent.get("chain").is_none(), "{sent}");
    assert_eq!(sent["contracts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn an_empty_expiration_list_is_no_data_not_an_empty_success() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = answering(
        dir.path(),
        r#"{"ok":true,"spot":null,"quotes":[],"missing":[],
            "warnings":["^SPX lists no option expirations"],"expirations":[]}"#,
    );
    let err = YFinance::new(config).expirations("SPX").await.unwrap_err();
    assert!(!err.is_unsupported());
    assert!(
        err.to_string().contains("Yahoo lists none for ^SPX"),
        "{err}"
    );
}

#[tokio::test]
async fn chain_asks_for_one_expiry_and_keeps_the_symbols_own_root() {
    let dir = tempfile::tempdir().unwrap();
    let (config, request_log) = answering(
        dir.path(),
        r#"{"ok":true,"as_of":"2026-10-01T17:00:00+00:00","spot":null,"quotes":[],
            "missing":[],"warnings":[],"chain":[
            {"symbol":"SPXW261016C06500000","right":"C","strike":6500.0,"bid":1250.1,
             "ask":1253.9,"last":1251.0,"mark":1252.0,"iv":0.2412,"volume":12,"open_interest":845},
            {"symbol":"SPX261016C06500000","right":"C","strike":6500.0,"bid":1251.0,
             "ask":1255.0,"last":null,"mark":null,"iv":null,"volume":null,"open_interest":null},
            {"symbol":"SPXW261016P06500000","right":"P","strike":6500.0,"bid":0.1,"ask":0.2,
             "last":0.15,"mark":0.15,"iv":0.31,"volume":0,"open_interest":9000}]}"#,
    );
    let yf = YFinance::new(config);

    let weekly = yf
        .chain("SPX", &section("SPXW", Settlement::Pm), None)
        .await
        .unwrap();
    assert_eq!(weekly.len(), 2);
    assert!(weekly.iter().all(|q| q.contract.root == "SPXW"));
    let call = weekly
        .iter()
        .find(|q| q.contract.right == Right::Call)
        .unwrap();
    assert_eq!(call.contract.settlement, Settlement::Pm);
    assert_eq!(call.volume, Some(12));
    assert_eq!(call.open_interest, Some(845));
    assert_eq!(call.mark, Some(dec!(1252)));
    // Stamped the usual quarter hour before the fetch.
    assert_eq!(call.as_of.to_rfc3339(), "2026-10-01T16:45:00+00:00");

    let monthly = yf
        .chain("SPX", &section("SPX", Settlement::Am), None)
        .await
        .unwrap();
    assert_eq!(monthly.len(), 1);
    assert_eq!(monthly[0].contract.root, "SPX");
    assert_eq!(monthly[0].contract.settlement, Settlement::Am);
    assert_eq!(monthly[0].mark, Some(dec!(1253)));

    let sent: Value = serde_json::from_slice(&std::fs::read(&request_log).unwrap()).unwrap();
    assert_eq!(sent["chain"]["expiry"], "2026-10-16");
    assert!(sent.get("list_expirations").is_none(), "{sent}");
}

#[tokio::test]
async fn a_guessed_monthly_with_no_spx_symbols_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = answering(
        dir.path(),
        r#"{"ok":true,"spot":null,"quotes":[],"missing":[],"warnings":[],"chain":[
            {"symbol":"SPXW261016C06500000","right":"C","strike":6500.0,"bid":1.0,"ask":2.0}]}"#,
    );
    let err = YFinance::new(config)
        .chain("SPX", &section("SPX", Settlement::Am), None)
        .await
        .unwrap_err();
    assert!(!err.is_unsupported());
    let text = err.to_string();
    assert!(text.contains("SPX contracts expiring 2026-10-16"), "{text}");
    assert!(text.contains("1 of another root or date"), "{text}");
}

/// A `[yfinance] script` override written before the chain fields existed
/// answers a request it does not understand with an ordinary quotes response.
#[tokio::test]
async fn a_bridge_script_that_predates_the_chain_fields_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = answering(dir.path(), ok_response());
    let yf = YFinance::new(config);
    for err in [
        yf.expirations("SPX").await.unwrap_err(),
        yf.chain("SPX", &section("SPX", Settlement::Am), None)
            .await
            .unwrap_err(),
    ] {
        assert!(err.to_string().contains("[yfinance] script"), "{err}");
        assert!(!err.is_unsupported());
    }
    // A real failure, so the status pill says so.
    assert!(matches!(
        OptionQuoteSource::health(&yf),
        Health::Unavailable(_)
    ));
}

#[tokio::test]
async fn a_yahoo_failure_on_the_chain_is_the_same_named_error_as_on_quotes() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = answering(
        dir.path(),
        r#"{"ok":false,"error":{"kind":"yahoo_error","message":"HTTPError: 429"}}"#,
    );
    let yf = YFinance::new(config);
    let err = yf.expirations("SPX").await.unwrap_err();
    assert!(
        err.to_string().contains("Yahoo error: HTTPError: 429"),
        "{err}"
    );
}

// ------------------------------------------------------ spawn hygiene

/// The sentence a user sees when the import fails has to name a version they
/// can actually install, and it is only useful while it names the same one
/// the repo pins.
#[test]
fn the_pin_matches_bridge_requirements() {
    let requirements = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../bridge/requirements.txt"
    ));
    let wanted = format!("yfinance=={YFINANCE_PIN}");
    assert!(
        requirements.lines().any(|line| line.trim() == wanted),
        "bridge/requirements.txt does not pin `{wanted}`:\n{requirements}"
    );
}

/// The interpreter is user-picked and drags a whole pip tree behind it, and
/// an app launched from a `.desktop` entry hands its children whatever working
/// directory the launcher had. Neither may be inherited: the child runs in the
/// state dir, and the Schwab credentials are not in its environment.
///
/// The "interpreter" here is `/bin/sh` running a script that prints its own
/// working directory and what it can see of the two variables; the output is
/// not JSON, so it comes back inside `BridgeError::BadOutput` and can be
/// asserted on. Deleting either `env_remove` in `call_inner` makes this fail.
#[tokio::test]
async fn the_child_environment_has_neither_credential() {
    const KEY: &str = "should-not-appear-key";
    const SECRET: &str = "should-not-appear-secret";
    // The only env-touching test in this binary, by design.
    std::env::set_var("SCHWAB_APP_KEY", KEY);
    std::env::set_var("SCHWAB_APP_SECRET", SECRET);

    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    std::fs::create_dir_all(&state).unwrap();
    // Somewhere other than the state dir, so `pwd` proves `current_dir` and
    // not merely "where the script happens to live".
    let script = dir.path().join("show.sh");
    std::fs::write(
        &script,
        r#"cat > /dev/null
pwd -P
echo "key=[$SCHWAB_APP_KEY] secret=[$SCHWAB_APP_SECRET]"
"#,
    )
    .unwrap();

    let config = YFinanceConfig::new(&state)
        .with_python("/bin/sh")
        .with_script(&script)
        .with_credential_env_names(["SCHWAB_APP_KEY", "SCHWAB_APP_SECRET"]);
    let err = YFinance::new(config).spot("SPX").await.unwrap_err();

    std::env::remove_var("SCHWAB_APP_KEY");
    std::env::remove_var("SCHWAB_APP_SECRET");

    // The child's own output, surfaced verbatim because it is not JSON.
    let text = err.to_string();
    assert!(
        text.contains("unreadable output") || text.contains("exited with"),
        "expected the child's output back, got {text}"
    );
    assert!(
        !text.contains(KEY) && !text.contains(SECRET),
        "the child inherited a Schwab credential: {text}"
    );
    let expected = std::fs::canonicalize(&state).unwrap();
    assert!(
        text.contains(&expected.display().to_string()),
        "the child did not run in the state dir ({}): {text}",
        expected.display()
    );
}

// ------------------------------------------------------- the real script

fn python3() -> Option<PathBuf> {
    std::process::Command::new("python3")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()
        .filter(std::process::ExitStatus::success)
        .map(|_| PathBuf::from("python3"))
}

/// A stand-in for `yfinance` that answers exactly like the real one, with the
/// shapes the bridge relies on: `fast_info`, `options`, `option_chain`, and
/// data frames that yield named tuples.
const STUB_YFINANCE: &str = r#"
class _FastInfo:
    last_price = 7752.34
    previous_close = 7764.84


class _Row:
    def __init__(self, strike, bid, ask, last, iv):
        self.strike = strike
        self.bid = bid
        self.ask = ask
        self.lastPrice = last
        self.impliedVolatility = iv


class _Frame:
    def __init__(self, rows):
        self._rows = rows

    def itertuples(self):
        return iter(self._rows)


class _Chain:
    def __init__(self, calls, puts):
        self.calls = _Frame(calls)
        self.puts = _Frame(puts)


class Ticker:
    options = ("2027-02-19",)

    def __init__(self, symbol):
        self.symbol = symbol
        self.fast_info = _FastInfo()

    def option_chain(self, expiry):
        assert expiry == "2027-02-19", expiry
        return _Chain(
            calls=[_Row(8200.0, 170.0, 172.0, 171.0, 0.148)],
            puts=[_Row(7325.0, 155.0, 156.0, 155.4, 0.2175),
                  _Row(6500.0, 65.0, 68.1, 66.5, 0.2270)],
        )
"#;

#[tokio::test]
async fn the_real_bridge_script_walks_a_chain_and_reports_what_is_missing() {
    let Some(python) = python3() else {
        eprintln!("skipping: no python3 on PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let libs = dir.path().join("libs");
    std::fs::create_dir_all(&libs).unwrap();
    std::fs::write(libs.join("yfinance.py"), STUB_YFINANCE).unwrap();

    let mut config = YFinanceConfig::new(dir.path()).with_python(python);
    config
        .env
        .push(("PYTHONPATH".into(), libs.display().to_string()));
    let yf = YFinance::new(config);

    // The script materialises itself out of the binary.
    let script = yf.script_path().unwrap();
    assert_eq!(std::fs::read_to_string(&script).unwrap(), BRIDGE_SCRIPT);

    let wanted = vec![
        contract(7325, Right::Put, (2027, 2, 19)),
        contract(8200, Right::Call, (2027, 2, 19)),
        contract(6500, Right::Put, (2027, 2, 19)),
        // Yahoo lists no such strike, and no such expiry.
        contract(9999, Right::Put, (2027, 2, 19)),
        contract(7000, Right::Put, (2027, 3, 19)),
    ];
    let outcome = yf.call("SPX", true, &wanted).await.unwrap();

    let spot = outcome.spot.expect("a spot");
    assert_eq!(spot.last, dec!(7752.34));
    assert_eq!(spot.change, Some(dec!(-12.5)));

    assert_eq!(outcome.quotes.len(), 3);
    let put = outcome
        .quotes
        .iter()
        .find(|q| q.contract.strike == dec!(7325))
        .unwrap();
    assert_eq!(put.bid, Some(dec!(155)));
    assert_eq!(put.ask, Some(dec!(156)));
    assert_eq!(put.mark, Some(dec!(155.5)));
    assert_eq!(put.iv_hint, Some(0.2175));
    assert_eq!(put.contract.right, Right::Put);
    let call = outcome
        .quotes
        .iter()
        .find(|q| q.contract.right == Right::Call)
        .unwrap();
    assert_eq!(call.mark, Some(dec!(171)));

    // Both kinds of miss are reported rather than silently dropped.
    assert_eq!(outcome.missing.len(), 2);
    assert!(outcome.missing.contains(&"SPX270219P9999".to_string()));
    assert!(outcome.missing.contains(&"SPX270319P7000".to_string()));
    assert!(
        outcome.warnings.iter().any(|w| w.contains("2027-03-19")),
        "{:?}",
        outcome.warnings
    );
}

#[tokio::test]
async fn the_real_script_reports_a_missing_yfinance_rather_than_a_traceback() {
    let Some(python) = python3() else {
        eprintln!("skipping: no python3 on PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let mut config = YFinanceConfig::new(dir.path()).with_python(python);
    // An import path with nothing on it; the real interpreter will not have
    // yfinance installed in this repo's environment either way.
    config
        .env
        .push(("PYTHONPATH".into(), empty.display().to_string()));
    let yf = YFinance::new(config);

    match yf.spot("SPX").await {
        Err(err) => {
            let text = err.to_string();
            assert!(
                text.contains("has no yfinance installed"),
                "expected a named failure, got {text}"
            );
        }
        // If the machine happens to have yfinance installed, the call reaches
        // Yahoo; either an answer or a network error is fine, but it must
        // never be a panic or a traceback.
        Ok(_) => eprintln!("skipping: this machine really has yfinance"),
    }
}

/// A `yfinance` stand-in for the chain fields: `contractSymbol`, `volume` and
/// `openInterest` columns as Yahoo names them, NaN where Yahoo has gaps, and an
/// `options` list with a junk entry.
const STUB_YFINANCE_CHAIN: &str = r#"
nan = float("nan")


class _Row:
    def __init__(self, symbol, strike, bid, ask, last, iv, volume, open_interest):
        self.contractSymbol = symbol
        self.strike = strike
        self.bid = bid
        self.ask = ask
        self.lastPrice = last
        self.impliedVolatility = iv
        self.volume = volume
        self.openInterest = open_interest


class _Frame:
    def __init__(self, rows):
        self._rows = rows

    def itertuples(self):
        return iter(self._rows)


class _Chain:
    def __init__(self, calls, puts):
        self.calls = _Frame(calls)
        self.puts = _Frame(puts)


class Ticker:
    options = ("2026-10-16", "2026-10-02", "garbage", "2026-10-02")

    def __init__(self, symbol):
        self.symbol = symbol

    def option_chain(self, expiry):
        assert expiry == "2026-10-16", expiry
        return _Chain(
            calls=[_Row("SPXW261016C06500000", 6500.0, 1250.1, 1253.9, 1251.0, 0.2412, 12.0, 845.0),
                   _Row("SPX261016C06500000", 6500.0, 1251.0, 1255.0, nan, nan, nan, nan),
                   _Row(None, 6510.0, 1.0, 2.0, 1.5, 0.2, 1.0, 1.0)],
            puts=[_Row("SPXW261016P06500000", 6500.0, nan, 0.35, nan, 0.0, nan, nan)],
        )
"#;

#[tokio::test]
async fn the_real_script_lists_expirations_and_a_chain_in_one_response_each() {
    let Some(python) = python3() else {
        eprintln!("skipping: no python3 on PATH");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let libs = dir.path().join("libs");
    std::fs::create_dir_all(&libs).unwrap();
    std::fs::write(libs.join("yfinance.py"), STUB_YFINANCE_CHAIN).unwrap();
    let mut config = YFinanceConfig::new(dir.path()).with_python(python);
    config
        .env
        .push(("PYTHONPATH".into(), libs.display().to_string()));
    let yf = YFinance::new(config);

    // Sorted, deduplicated, and without the entry that is not a date.
    let listed = yf.expirations("SPX").await.unwrap();
    let dates: Vec<_> = listed.iter().map(|e| e.expiry.to_string()).collect();
    assert_eq!(dates, ["2026-10-02", "2026-10-16", "2026-10-16"]);

    let weekly = yf
        .chain("SPX", &section("SPXW", Settlement::Pm), None)
        .await
        .unwrap();
    assert_eq!(weekly.len(), 2, "the row with no symbol is skipped");
    let call = weekly
        .iter()
        .find(|q| q.contract.right == Right::Call)
        .unwrap();
    assert_eq!(call.volume, Some(12));
    assert_eq!(call.open_interest, Some(845));
    assert_eq!(call.mark, Some(dec!(1252)));
    // NaN everywhere Yahoo had a gap: `null` on the wire, `None` here.
    let put = weekly
        .iter()
        .find(|q| q.contract.right == Right::Put)
        .unwrap();
    assert_eq!(put.bid, None);
    assert_eq!(put.ask, Some(dec!(0.35)));
    assert_eq!(put.mark, None);
    assert_eq!(put.iv_hint, None);
    assert_eq!((put.volume, put.open_interest), (None, None));

    let monthly = yf
        .chain("SPX", &section("SPX", Settlement::Am), None)
        .await
        .unwrap();
    assert_eq!(monthly.len(), 1);
    assert_eq!(monthly[0].last, None);
    assert_eq!(monthly[0].volume, None);

    // An expiry Yahoo does not list is a plain sentence, not a ValueError.
    let mut absent = section("SPXW", Settlement::Pm);
    absent.expiry = chrono::NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let err = yf.chain("SPX", &absent, None).await.unwrap_err();
    assert!(err.to_string().contains("no data"), "{err}");

    // And the script on its own, by hand: a malformed expiry is a named
    // bad_request, never a traceback.
    let script = yf.script_path().unwrap();
    let output = std::process::Command::new("python3")
        .arg(&script)
        .env("PYTHONPATH", &libs)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .take()
                .unwrap()
                .write_all(br#"{"underlying":"SPX","chain":{"expiry":"soon"}}"#)?;
            child.wait_with_output()
        })
        .unwrap();
    let answer: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(answer["ok"], false);
    assert_eq!(answer["error"]["kind"], "bad_request");
}
