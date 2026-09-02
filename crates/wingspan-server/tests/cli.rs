//! The CLI as the user runs it: the real binary, a real trades folder, and the
//! numbers the plan's verification section names.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Run `wingspan` with a hermetic environment: no user config file, no
/// `WINGSPAN_*` inherited from the shell, and a throwaway state directory.
fn wingspan(config: Option<&Path>, args: &[&str]) -> Output {
    let scratch = wingspan_server::testing::scratch_dir();
    let mut command = Command::new(env!("CARGO_BIN_EXE_wingspan"));
    command.current_dir(workspace_root());
    for (key, _) in std::env::vars() {
        if key.starts_with("WINGSPAN_") {
            command.env_remove(key);
        }
    }
    command
        .env("XDG_CONFIG_HOME", scratch.join("config"))
        .env("XDG_STATE_HOME", scratch.join("state"));
    if let Some(path) = config {
        command.arg("--config").arg(path);
    }
    command.args(args).output().expect("wingspan runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

// ------------------------------------------------------------------ validate

/// Plan §Verification 2, end to end.
#[test]
fn validate_prints_the_plans_numbers_for_the_fixture_book() {
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
    assert!(stderr.contains("wingspan:"), "{stderr}");
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
fn doctor_walks_the_checklist_and_finds_the_fixture_folders() {
    let scratch = wingspan_server::testing::scratch_dir();
    let config_path = scratch.join("wingspan.toml");
    std::fs::write(
        &config_path,
        format!(
            "[trades]\nroot = {root:?}\n\n[market]\nstate_dir = {state:?}\n\n\
             [yfinance]\nenabled = false\n",
            root = workspace_root().join("fixtures/trades").to_string_lossy(),
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
        "Providers",
    ] {
        assert!(text.contains(section), "no {section} section:\n{text}");
    }
    assert!(text.contains("(loopback)"), "{text}");
    assert!(text.contains("open trades"), "{text}");
    assert!(text.contains("statements"), "{text}");
    // No token path configured is a warning, never a crash.
    assert!(text.contains("token_path"), "{text}");
    assert!(text.contains("0 problem(s)"), "{text}");
}

#[test]
fn schwab_status_is_honest_when_there_is_no_token() {
    let output = wingspan(None, &["schwab", "status"]);
    let text = stdout(&output);
    assert!(!output.status.success());
    assert!(text.contains("Schwab is not configured."), "{text}");
    assert!(text.contains("token_path"), "{text}");
}

#[test]
fn the_binary_reports_its_version() {
    let output = wingspan(None, &["--version"]);
    assert!(output.status.success());
    assert!(
        stdout(&output).contains(wingspan_server::VERSION),
        "{}",
        stdout(&output)
    );
}

fn line_with<'a>(text: &'a str, needle: &str) -> &'a str {
    text.lines()
        .find(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line containing {needle:?} in:\n{text}"))
}
