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
use wingspan_marketdata::{Health, OptionQuoteSource, SpotSource, YFinance, YFinanceConfig};

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
