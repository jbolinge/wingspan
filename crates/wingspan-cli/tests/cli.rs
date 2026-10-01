//! The CLI as the user runs it: the real binary, a real trades folder, and the
//! acceptance numbers `docs/DOMAIN.md` names.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run `wingspan-cli` with a hermetic environment: no user config file, no
/// `WINGSPAN_*` inherited from the shell, and a throwaway state directory.
fn wingspan(config: Option<&Path>, args: &[&str]) -> Output {
    wingspan_env(config, args, &[])
}

/// The same, plus the `WINGSPAN_*` overrides a test wants to set itself.
fn wingspan_env(config: Option<&Path>, args: &[&str], env: &[(&str, &Path)]) -> Output {
    let scratch = wingspan_app::demo::scratch_dir();
    let mut command = Command::new(env!("CARGO_BIN_EXE_wingspan-cli"));
    command.current_dir(workspace_root());
    for (key, _) in std::env::vars() {
        if key.starts_with("WINGSPAN_") {
            command.env_remove(key);
        }
    }
    command
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_STATE_HOME", scratch.join("state"));
    for (key, value) in env {
        command.env(key, value);
    }
    if let Some(path) = config {
        command.arg("--config").arg(path);
    }
    command.args(args).output().expect("wingspan-cli runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ------------------------------------------------------------------ validate

/// The acceptance numbers in `docs/DOMAIN.md`, end to end.
#[test]
fn validate_prints_the_acceptance_numbers_for_the_fixture_book() {
    let output = wingspan(
        None,
        &["validate", "fixtures/trades", "--as-of", "2026-08-31"],
    );
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "exit {:?}\n{text}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(text.contains("Open trades (6)"), "{text}");
    assert!(text.contains("Closed trades (2)"), "{text}");
    assert!(text.contains("6 open, 2 closed"), "{text}");

    // BSH: the rolled Sep put, realised -433, one Oct put left, total -613.78.
    let bsh = line_with(&text, "BSH        2026-07-14");
    assert!(bsh.contains("-613.78"), "{bsh}");
    assert!(bsh.contains("-177.00"), "{bsh}");
    assert!(bsh.contains("DIAG 5450/5050 OCT/SEP26"), "{bsh}");

    // Delta 1 sold the put M4 holds long and banked +135.20.
    let delta = line_with(&text, "Delta 1    2026-08-25");
    assert!(delta.contains("135.20"), "{delta}");

    // LPTA's six byte-identical orders are quarantined, not netted.
    assert!(text.contains("Quarantined orders (6)"), "{text}");
    assert!(
        text.matches("lpta-2026-07-24").count() >= 6,
        "every quarantined order names its trade"
    );
    let lpta = line_with(&text, "LPTA       2026-07-24");
    assert!(lpta.contains("-162.35"), "{lpta}");

    // The JUL 26 box the account held and no trade file explains.
    assert!(
        text.contains("Unassigned account positions in earlier statements (4)"),
        "{text}"
    );
    for code in [
        "SPX260717C6575",
        "SPX260717C6625",
        "SPX260717P6575",
        "SPX260717P6625",
    ] {
        assert!(text.contains(code), "missing {code}\n{text}");
    }

    // Quantity reconciliation against the newest statement, exactly.
    assert!(
        text.contains("Reconciliation vs the 2026-08-28 statement"),
        "{text}"
    );
    assert!(text.contains("23 row(s), 23 match, 0 problem(s)"), "{text}");
    assert!(text.contains("reconciliation clean"), "{text}");
    assert!(!text.contains("qty_mismatch"), "{text}");

    // Quarantine kept 7325P at +4 rather than +6.
    let row = line_with(&text, "SPX270219P7325");
    assert!(row.contains("+4") && row.contains("match"), "{row}");
}

/// `validate [SOURCE]` is step 10's job: a Legs CSV path reproduces the
/// exact same golden numbers `legs_golden.rs` pins on this fixture with the
/// same mark table — the CLI and the core test suite must never disagree.
#[test]
fn validate_reads_a_legs_csv_and_reproduces_the_book() {
    let output = wingspan(
        None,
        &[
            "validate",
            "fixtures/trades/legs.csv",
            "--as-of",
            "2026-08-31",
        ],
    );
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "exit {:?}\n{text}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(text.contains("Open trades (6)"), "{text}");
    assert!(text.contains("Closed trades (2)"), "{text}");
    assert!(text.contains("6 open, 2 closed"), "{text}");

    let source_line = line_with(&text, "source");
    assert!(source_line.contains("legs.csv"), "{source_line}");
    let marks_line = line_with(&text, "marks");
    assert!(marks_line.contains("from the Mark column"), "{marks_line}");
    let legs_line = line_with(&text, "legs ");
    assert!(legs_line.contains("30 row(s)"), "{legs_line}");

    // The same numbers `legs_golden.rs` pins on this exact fixture with the
    // same mark table.
    assert!(line_with(&text, "BSH").contains("-613.78"), "{text}");
    assert!(line_with(&text, "Delta 1").contains("135.20"), "{text}");
    assert!(line_with(&text, "LPTA").contains("-162.35"), "{text}");

    assert!(text.contains("Quarantined orders (0)"), "{text}");
    assert!(text.contains("(no account positions to compare)"), "{text}");
}

/// The regression this pins: after a source is chosen in the app, an
/// argument-less `validate` must read *that* record, not `[trades]
/// csv_path` — the same ladder `wingspan_app::state`'s boot walks.
#[test]
fn validate_with_no_argument_reads_the_source_the_app_saved() {
    let scratch = wingspan_app::demo::scratch_dir();
    let state_dir = scratch.join("state");
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\ncsv_path = {csv:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            csv = workspace_root()
                .join("fixtures/trades/legs.csv")
                .to_string_lossy(),
            state = state_dir.to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["validate", "--as-of", "2026-08-31"]);
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(text.contains("6 open, 2 closed"), "{text}");

    // Now choose a different source in the app's own state directory: it
    // must outrank `[trades] csv_path`.
    let demo_csv = workspace_root().join("fixtures/demo/legs.csv");
    wingspan_app::saved_source::write(
        &state_dir,
        &wingspan_app::saved_source::Record::chosen(
            wingspan_app::source::SourceRequest::Csv { path: demo_csv },
            chrono::Utc::now(),
            Vec::new(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["validate", "--as-of", "2026-08-31"]);
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains("2 open, 1 closed"),
        "the saved source must outrank [trades] csv_path:\n{text}"
    );
}

#[test]
fn validate_refuses_when_there_is_nothing_to_validate() {
    let scratch = wingspan_app::demo::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[market]\nstate_dir = {state:?}\n\n[yfinance]\nenabled = false\n",
            state = scratch.join("state").to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["validate"]);
    assert!(
        !output.status.success(),
        "expected a refusal, got:\n{}",
        stdout(&output)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("nothing to validate"), "{err}");
}

/// `sheet_url::parse` rejects the host before any fetch is even attempted,
/// so this must reach no network at all.
#[test]
fn validate_says_what_is_wrong_for_a_link_that_is_not_a_sheet() {
    let output = wingspan(None, &["validate", "https://example.com/whatever"]);
    assert!(
        !output.status.success(),
        "expected a refusal, got:\n{}",
        stdout(&output)
    );
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        err.contains("neither a file") && err.contains("Google Sheet link"),
        "{err}"
    );
}

/// The demo book is made up (`scripts/make_demo_book.py`), and made up
/// *well*: it goes through the same parsers as the account data and comes out
/// with every execution claimed, nothing quarantined, and the statement
/// agreeing with the trade files — so the demo shows a clean dashboard, not a
/// warnings drawer.
#[test]
fn validate_reads_the_synthetic_demo_book_clean() {
    let output = wingspan(
        None,
        &["validate", "fixtures/demo", "--as-of", "2026-08-31"],
    );
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "exit {:?}\n{text}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(text.contains("4 execution(s), 4 claimed"), "{text}");
    assert!(text.contains("2 open, 1 closed"), "{text}");
    assert!(text.contains("Quarantined orders (0)"), "{text}");
    assert!(
        text.contains("Unassigned account positions in earlier statements (0)"),
        "{text}"
    );

    // A ~45-DTE strangle, a condor one expiry out, and a closed put spread
    // whose closing order carries a `# date` hint. Nothing from the real
    // account: none of its trade names or expiries.
    let strangle = line_with(&text, "Oct Stran");
    assert!(strangle.contains("-1x STRG 8200/7200 OCT26"), "{strangle}");
    assert!(strangle.contains("   46 "), "{strangle}");
    let condor = line_with(&text, "Nov Condor");
    assert!(
        condor.contains("-2x IC 8400/8500/7000/6900 NOV26"),
        "{condor}"
    );
    let spread = line_with(&text, "Sep Put S");
    assert!(spread.contains("-2x VERT 7400/7350 SEP26"), "{spread}");
    assert!(spread.contains("429.92"), "{spread}");
    for real in ["BSH", "LPTA", "Delta 1", "FEB 27", "SPX261016P5450"] {
        assert!(!text.contains(real), "the demo shows {real}\n{text}");
    }

    // The only warnings are the info lines every statement produces.
    assert!(text.contains("Warnings (20)"), "{text}");
    assert!(!text.contains("\n  warn "), "{text}");
    assert!(!text.contains("\n  error "), "{text}");
    assert!(
        text.contains("Reconciliation vs the 2026-08-26 statement"),
        "{text}"
    );
    assert!(text.contains("6 row(s), 6 match, 0 problem(s)"), "{text}");
    assert!(text.contains("reconciliation clean"), "{text}");
}

#[test]
fn validate_can_print_every_leg() {
    let output = wingspan(
        None,
        &[
            "validate",
            "fixtures/trades",
            "--as-of",
            "2026-08-31",
            "--legs",
        ],
    );
    let text = stdout(&output);
    assert!(output.status.success());
    assert!(text.contains("Netted legs"), "{text}");
    assert!(text.contains("SPX261016P5450"), "{text}");
    assert!(text.contains("avg       4.42"), "{text}");
}

/// Discovery descends one level, so pointing at the parent works — and with
/// two books under `fixtures/` it takes the first in sorted order, `demo`.
#[test]
fn validate_finds_the_trades_folder_one_level_down() {
    let output = wingspan(None, &["validate", "fixtures", "--as-of", "2026-08-31"]);
    assert!(output.status.success());
    let text = stdout(&output);
    assert!(text.contains("root         fixtures/demo"), "{text}");
    assert!(text.contains("Open trades (2)"), "{text}");
}

#[test]
fn validate_says_what_is_wrong_when_the_folder_is_not_one() {
    let output = wingspan(None, &["validate", "docs"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("wingspan-cli:"), "{stderr}");
    assert!(stderr.contains("no trades folders"), "{stderr}");
}

// ---------------------------------------------------------------------- risk

/// With nothing configured, `risk` reads the synthetic demo book — so the
/// out-of-the-box curve is the strangle's, not anyone's account.
#[test]
fn risk_prints_a_csv_of_the_curve() {
    let output = wingspan(
        None,
        &["risk", "Oct Strangle", "--csv", "--demo", "--points", "5"],
    );
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let header = lines.next().expect("a header row");
    assert!(header.starts_with("price,t0,"), "{header}");
    assert!(header.contains("expiry"), "{header}");
    let rows: Vec<&str> = lines.collect();
    assert_eq!(rows.len(), 5, "{rows:?}");
    for row in &rows {
        assert_eq!(
            row.split(',').count(),
            header.split(',').count(),
            "ragged row {row}"
        );
    }
}

#[test]
fn risk_lists_the_trades_it_knows_when_the_name_is_wrong() {
    let output = wingspan(None, &["risk", "NOPE", "--demo"]);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("no trade `NOPE`"), "{stderr}");
    assert!(stderr.contains("oct-strangle-2026-08-24"), "{stderr}");
    assert!(stderr.contains("nov-condor-2026-08-26"), "{stderr}");
    assert!(
        !stderr.contains("m1-2026-07-14"),
        "the account book leaked: {stderr}"
    );
}

// -------------------------------------------------------------------- doctor

#[test]
fn doctor_walks_the_checklist_and_reports_the_trade_source() {
    let scratch = wingspan_app::demo::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\ncsv_path = {csv:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            csv = workspace_root()
                .join("fixtures/trades/legs.csv")
                .to_string_lossy(),
            state = scratch.join("state").to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["doctor"]);
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for section in [
        "Configuration",
        "Trades",
        "State",
        "yfinance bridge",
        "Schwab",
        "Tastytrade",
        "Providers",
    ] {
        assert!(text.contains(section), "no {section} section:\n{text}");
    }
    assert!(text.contains("config file"), "{text}");
    // Nothing has been imported in this scratch state dir, so the source row
    // is honestly empty and the configured default is what doctor reports
    // instead — but the CSV it names is still right there on disk, so the
    // rows/legs/skipped rows below it read real numbers with no fetch.
    let trades = section_of(&text, "Trades");
    assert!(trades.contains("nothing imported"), "{trades}");
    assert!(trades.contains("[trades] csv_path"), "{trades}");
    assert!(trades.contains("fixtures/trades/legs.csv"), "{trades}");
    for row in [
        "source",
        "configured default",
        "tab",
        "auto refresh",
        "fetched",
        "rows",
        "legs",
        "skipped",
        "cache age",
    ] {
        assert!(trades.contains(row), "no {row} row:\n{trades}");
    }
    assert!(
        line_with(trades, "fetched").contains("read straight off disk"),
        "{trades}"
    );
    assert!(line_with(trades, "rows ").contains("30"), "{trades}");
    assert!(line_with(trades, "legs ").contains("30"), "{trades}");
    assert!(line_with(trades, "skipped").contains('0'), "{trades}");
    assert!(text.contains("0 problem(s)"), "{text}");
}

/// `[trades] root` survives as a typed-but-unread field so an existing
/// `wingspan.toml` still parses (decision 17). Doctor is where that shows up:
/// one warn row saying the key is dead and what to do instead, rather than a
/// silent shrug on a machine whose config still points at a TOS folder.
#[test]
fn doctor_says_the_retired_folder_key_is_no_longer_read() {
    let scratch = wingspan_app::demo::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\nroot = {dir:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            dir = workspace_root().join("fixtures/trades").to_string_lossy(),
            state = scratch.join("state").to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["doctor"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}");
    assert!(
        text.contains("no longer read; import a Legs sheet or CSV"),
        "{text}"
    );
    // Dead, not broken: a retired key is a warning, never a problem.
    assert!(text.contains("0 problem(s)"), "{text}");
}

/// No Schwab token configured is the ordinary first run, not a fault: the
/// Schwab section has to say which path is missing — that is the sentence a
/// user acts on — grade it a warning, and leave the exit code at 0.
#[test]
fn doctor_is_honest_about_schwab_when_there_is_no_token() {
    let scratch = wingspan_app::demo::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\ncsv_path = {csv:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            csv = workspace_root()
                .join("fixtures/trades/legs.csv")
                .to_string_lossy(),
            state = scratch.join("state").to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["doctor"]);
    let text = stdout(&output);
    assert!(
        output.status.success(),
        "a missing token is never a crash: exit {:?}\n{text}\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    let schwab = section_of(&text, "Schwab");
    assert!(
        schwab.contains("token_path"),
        "the Schwab section must name the path it could not find:\n{schwab}"
    );
    assert!(
        schwab.contains("[warn]"),
        "a missing token is a warning, not a pass and not a failure:\n{schwab}"
    );
    assert!(text.contains("0 problem(s)"), "{text}");
}

/// tastytrade is off by default and not in the default chain, so upgrading
/// must change nothing an existing install sees: one quiet note line, no
/// warning about a provider its owner has never heard of, and the same exit
/// code.
#[test]
fn doctor_is_quiet_about_tastytrade_when_it_is_not_enabled() {
    let scratch = wingspan_app::demo::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\ncsv_path = {csv:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            csv = workspace_root()
                .join("fixtures/trades/legs.csv")
                .to_string_lossy(),
            state = scratch.join("state").to_string_lossy(),
        ),
    )
    .unwrap();

    let output = wingspan(Some(&config_path), &["doctor"]);
    let text = stdout(&output);
    assert!(output.status.success(), "{text}");

    let tastytrade = section_of(&text, "Tastytrade");
    assert!(tastytrade.contains("not enabled"), "{tastytrade}");
    assert!(!tastytrade.contains("[warn]"), "{tastytrade}");
    assert!(!tastytrade.contains("[FAIL]"), "{tastytrade}");
    assert!(text.contains("0 problem(s)"), "{text}");
}

/// The lines under one `doctor` heading, up to the next blank line.
fn section_of<'a>(text: &'a str, heading: &str) -> &'a str {
    let start = text
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("no {heading} section in:\n{text}"))
        + heading.len()
        + 2;
    let rest = &text[start..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn the_binary_reports_its_version() {
    let output = wingspan(None, &["--version"]);
    assert!(output.status.success());
    assert!(
        stdout(&output).contains(wingspan_app::VERSION),
        "{}",
        stdout(&output)
    );
}

// --------------------------------------------------------- dump-contract

/// R2, end to end: `--out` is a path a human typed, and a mistyped or
/// tab-completed one must cost nothing. Pointed at a directory holding the
/// user's own `config.json` — one of the nineteen names the dump writes —
/// the real binary refuses, leaves the bytes alone, and writes nothing at
/// all, marker included. This drives `run()` rather than the gate function,
/// so it also pins the *ordering*: the check stands ahead of every write.
#[test]
fn dump_contract_refuses_a_directory_that_is_not_one_of_its_own_dumps() {
    const THEIRS: &str = "{\"my\":\"precious settings\"}\n";
    let dir = wingspan_app::demo::scratch_dir().join("theirs");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), THEIRS).unwrap();

    let output = wingspan(
        None,
        &[
            "dump-contract",
            "--demo",
            "--out",
            dir.to_str().expect("utf-8 scratch path"),
        ],
    );
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        !output.status.success(),
        "the command must fail\n{}\n{stderr}",
        stdout(&output)
    );
    assert!(stderr.contains(".wingspan-contract"), "{stderr}");

    assert_eq!(
        std::fs::read_to_string(dir.join("config.json")).unwrap(),
        THEIRS,
        "the user's file must survive untouched"
    );
    let mut left: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        ["config.json"],
        "a refusal must write nothing, not even the marker"
    );
}

fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{text}"))
}
