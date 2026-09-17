//! `wingspan-cli dump-contract --demo --out <dir>` — the frozen API contract as
//! files.
//!
//! This is a drift detector, not a feature. It builds the app over the
//! bundled synthetic demo book, calls every function in
//! [`wingspan_app::service`] with a fixed, hard-coded request set, and writes each
//! response as pretty JSON — one file per call — into `--out`
//! (`web/e2e/fixtures/contract` is where the checked-in copy lives). CI — and
//! `just contract-check` — re-run this command into a scratch directory and
//! `diff -r` it against the checked-in copy, so an added, removed or changed
//! response body all fail alike: no diff means the API's response bodies are
//! exactly what they were the day the fixtures were last regenerated.
//!
//! It deliberately ignores the user's own configuration — no
//! `~/.config/wingspan/wingspan.toml`, no `./wingspan.toml`, no `WINGSPAN_*`
//! environment variable is read — and everything machine-specific (absolute
//! paths under the checkout, the temp state directory, `$HOME`) is redacted
//! before it reaches disk, so nothing about *this* machine reaches the files.
//!
//! The one input the command cannot pin is the C library. The greeks and the
//! risk-graph curves come out of `exp`/`ln` in the platform's libm, which is
//! not promised to agree to the last bit across glibc versions, and the IV
//! solver's own `1e-12` vol tolerance can turn a last-bit disagreement into a
//! visible digit. Every float is therefore rounded to [`FLOAT_DECIMALS`]
//! decimals on the way out — orders of magnitude coarser than that noise and
//! orders of magnitude finer than any change to a fee, a sign or a formula —
//! so the same commit dumped on the dev box and on CI writes the same bytes.
//!
//! Both `trades_root.json` and `trade_source.json` are dumped side by side
//! during the folder-to-source transition (steps 5-9); the former goes away
//! once the folder surface is removed.
//!
//! `--out` is a path a human typed, so the command refuses to work in a
//! directory that is not one of its own: unless the directory is new, empty,
//! or already carries the `.wingspan-contract` marker a previous dump wrote,
//! nothing is written and nothing is deleted (see [`prepare_out_dir`]). A
//! mistyped or tab-completed `--out` costs an error message, never a file.
//!
//! A diff CI reports on this fixture is one of two things: an intended
//! contract change (regenerate with `just contract` — or the `cargo run`
//! line in this module's doc — and commit the new files), or a regression
//! that changed a response body nobody meant to change.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::NaiveDate;
use serde::Serialize;
use serde_json::Value;
use wingspan_pricing::PriceRange;

use crate::cli::DumpContractArgs;
use wingspan_app::dto::{RiskGraphRequest, WhatIfRequest};
use wingspan_app::error::ApiResult;
use wingspan_app::service;
use wingspan_app::source::SourceRef;
use wingspan_app::state::{AppOptions, AppState};
use wingspan_app::VERSION;

/// The demo book's two open trades (`crates/wingspan-cli/tests/cli.rs`,
/// `web/e2e/helpers.ts`'s `DEMO_BOOK_TRADE_IDS`). A fixed literal, never
/// read out of the book, so the request set the contract freezes cannot
/// silently change shape if the demo book is regenerated.
const OPEN_IDS: [&str; 2] = ["oct-strangle-2026-08-24", "nov-condor-2026-08-26"];

/// The demo book's one closed trade. Found once by running
/// `wingspan-cli validate fixtures/demo --as-of 2026-08-31 --legs` and reading
/// the id it prints, then hard-coded here — never derived from the book at
/// dump time. Used only as a guard: if the demo book ever changes shape, this
/// command must fail loudly rather than quietly freeze a different contract.
const CLOSED_ID: &str = "sep-put-spread-2026-08-10";

/// Decimals every `f64` in a dumped body is rounded to.
///
/// The floats here are dollars, vols and greeks solved through libm's
/// transcendentals, so their last few digits are a property of the machine
/// rather than of this code (see the module doc). Six decimals is a
/// millionth of a cent — below anything a real contract change can move, and
/// ~10^6 above the drift an IV solve can amplify a last-bit difference into.
const FLOAT_DECIMALS: i32 = 6;

/// The file that marks a directory as one of *our* dumps.
///
/// `--out` is a path a human typed; nothing in it is written *or* deleted
/// unless this file says a previous run of this command owns the directory,
/// or the directory is empty and so has nothing to lose.
const MARKER: &str = ".wingspan-contract";

/// The line that proves a [`MARKER`] file is one of ours.
///
/// Ownership is decided on this prefix rather than on the whole body: an
/// empty or `touch`ed file of that name proves nothing, while a marker an
/// older version of this command wrote — the body is prose, and prose gets
/// edited — still opens the directory it has always owned. Reworded text
/// must never lock a developer out of their own fixture directory.
const MARKER_SIGNATURE: &str = "Written by `wingspan";

/// The marker's body. Fixed text, no timestamp: it is checked in beside the
/// fixtures and CI diffs the whole directory. It opens with
/// [`MARKER_SIGNATURE`], which is what [`is_ours`] actually reads.
const MARKER_BODY: &str = "\
Written by `wingspan-cli dump-contract`. Its presence lets that command rewrite
the *.json in this directory and delete the ones a previous dump left behind;
without it the command refuses to touch a directory that already holds files.
Do not put anything else in this directory.
";

pub async fn run(args: &DumpContractArgs) -> anyhow::Result<ExitCode> {
    if !args.demo {
        anyhow::bail!("dump-contract only knows how to dump the demo book; pass --demo");
    }

    // Ownership is settled first, before the book is loaded and long before
    // a byte lands in `--out`: this command rewrites fifteen fixed
    // filenames and deletes the leftovers of a previous dump, and neither
    // may happen in a directory that is not ours.
    prepare_out_dir(&args.out)?;

    // Hermetic config: no config file, no environment variable, so the dump
    // is identical everywhere. `state_dir` is a throwaway scratch directory
    // — never the user's own state — canonicalised so the path we redact
    // below is the same one that lands in the response bodies.
    let mut config = wingspan_app::config::Config::default();
    config.trades.root = None;
    let scratch = wingspan_app::demo::scratch_dir();
    let state_dir = std::fs::canonicalize(&scratch).unwrap_or(scratch);
    config.market.state_dir = Some(state_dir.clone());

    // Options: what `serve --demo` with no `--root`/`--as-of` builds, by
    // hand — `pin_to` keeps the valuation date and the pricing clock moving
    // together, exactly as `serve` itself insists on.
    let root = crate::cli::demo_root()?;
    let mut options = AppOptions {
        demo: true,
        ..AppOptions::default()
    };
    let as_of = wingspan_app::demo::valuation_date();
    crate::cli::pin_to(&mut options, as_of);
    options.source_override = Some(SourceRef::Folder { path: root });

    let state = AppState::build(config, options).await?;

    // A guard, not a data source: the request set below is fixed regardless
    // of what this returns, but a demo book that has drifted away from
    // `CLOSED_ID` must fail this command loudly rather than silently freeze
    // a contract nobody asked for.
    let closed = service::trades_closed(&state)
        .map_err(|e| anyhow::anyhow!("dump-contract: trades_closed failed: {}", e.detail))?;
    let closed_ids: Vec<&str> = closed.trades.iter().map(|t| t.id.as_str()).collect();
    if closed_ids != [CLOSED_ID] {
        anyhow::bail!(
            "dump-contract: the demo book's closed trades are {closed_ids:?}, not \
             [{CLOSED_ID:?}] — this command's fixed request set assumes the demo book \
             it was written against; update CLOSED_ID (and re-check the rest of the \
             fixed request set) if the demo book changed on purpose"
        );
    }

    let ids = |ids: &[&str]| -> Vec<String> { ids.iter().map(|s| (*s).to_string()).collect() };

    let default_risk_graph = || RiskGraphRequest {
        trade_ids: ids(&OPEN_IDS),
        price_range: None,
        points: Some(241),
        dates: None,
        vol_shift: 0.0,
        vol_shift_by_expiry: BTreeMap::new(),
        spot_override: None,
        include_realized: true,
    };
    let risk_graph_whole_book = RiskGraphRequest {
        trade_ids: Vec::new(),
        ..default_risk_graph()
    };
    let risk_graph_crossed = RiskGraphRequest {
        price_range: Some(PriceRange::Absolute {
            min: 8000.0,
            max: 7000.0,
        }),
        ..default_risk_graph()
    };
    let what_if_request = WhatIfRequest {
        trade_ids: ids(&OPEN_IDS),
        price: 7250.0,
        date: NaiveDate::from_ymd_opt(2026, 8, 31).expect("valid date"),
        vol_shift: 0.05,
        // The SPA's default (`dto::WhatIfRequest`'s serde default: `yes()`).
        include_realized: true,
    };

    // Order matters only for `reload.json`, which must come last: it
    // re-reads the book and broadcasts a `trades_changed` event, so every
    // other call above sees the same, undisturbed load.
    let entries: Vec<(&str, Value)> = vec![
        ("health.json", to_value(&service::health())?),
        (
            "market_snapshot.json",
            dump(service::market_snapshot(&state))?,
        ),
        ("config.json", service::config(&state)),
        ("trades.json", dump(service::trades(&state))?),
        ("trades_closed.json", to_value(&closed)?),
        ("trades_root.json", to_value(&service::trades_root(&state))?),
        (
            "trade_source.json",
            to_value(&service::trade_source(&state))?,
        ),
        (
            "reconciliation.json",
            to_value(&service::reconciliation(&state))?,
        ),
        ("warnings.json", to_value(&service::warnings(&state))?),
        (
            "portfolio_all.json",
            dump(service::portfolio(&state, &ids(&OPEN_IDS)))?,
        ),
        (
            "portfolio_one.json",
            dump(service::portfolio(
                &state,
                &["oct-strangle-2026-08-24".to_string()],
            ))?,
        ),
        (
            "risk_graph.json",
            dump(service::risk_graph(&state, default_risk_graph()))?,
        ),
        (
            "risk_graph_whole_book.json",
            dump(service::risk_graph(&state, risk_graph_whole_book))?,
        ),
        (
            "risk_graph_crossed.json",
            dump(service::risk_graph(&state, risk_graph_crossed))?,
        ),
        (
            "what_if.json",
            dump(service::what_if(&state, what_if_request))?,
        ),
        ("reload.json", dump(service::reload(&state).await)?),
    ];

    // The redaction targets, longest/most-specific first so a nested path
    // (repo, inside home) is never half-replaced by the shorter one.
    let repo_raw = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let repo = std::fs::canonicalize(&repo_raw).unwrap_or(repo_raw);
    let home = std::env::var("HOME").unwrap_or_default();
    let replacements: Vec<(String, &str)> = vec![
        (state_dir.display().to_string(), "<state_dir>"),
        (repo.display().to_string(), "<repo>"),
        (home, "<home>"),
    ];
    let replacements: Vec<(&str, &str)> = replacements
        .iter()
        .filter(|(from, _)| !from.is_empty())
        .map(|(from, to)| (from.as_str(), *to))
        .collect();

    // `--out` was proved ours at the top of this function; all that is left
    // is the deletion of the fixtures a previous dump wrote and this one no
    // longer produces.
    let writing: BTreeSet<&str> = entries.iter().map(|(name, _)| *name).collect();
    clear_stale_json(&args.out, &writing)?;

    for (name, mut value) in entries {
        redact(&mut value, &replacements);
        round_floats(&mut value);
        let text = serde_json::to_string_pretty(&value)? + "\n";
        for (needle, _) in &replacements {
            if text.contains(needle) {
                anyhow::bail!(
                    "dump-contract: {name} still contains a machine-specific path after \
                     redaction — a leaked absolute path would make CI fail on every \
                     machine but this one"
                );
            }
        }
        let path = args.out.join(name);
        std::fs::write(&path, &text)
            .map_err(|e| anyhow::anyhow!("dump-contract: writing {}: {e}", path.display()))?;
        println!("wrote {}", path.display());
    }

    Ok(ExitCode::SUCCESS)
}

/// `Ok(v)` as its DTO, `Err(e)` as the frozen `{type,title,status,detail}`
/// problem body — so a file always holds exactly what the wire would carry.
fn dump<T: Serialize>(result: ApiResult<T>) -> anyhow::Result<Value> {
    match result {
        Ok(v) => to_value(&v),
        Err(e) => to_value(&e.problem()),
    }
}

fn to_value<T: Serialize>(v: &T) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(v)?)
}

/// Create `--out` if it is missing, prove it is ours, and only then mark it.
///
/// `--out` is a path a human typed, and this command is destructive twice
/// over: it overwrites fifteen fixed filenames — `config.json`,
/// `health.json`, `trades.json` are among the commonest names there are —
/// and it deletes the `*.json` a previous dump left behind. R2's rule (never
/// destroy a byte the user did not ask us to) covers both, so ownership is
/// decided once, here, *before anything is written*: the directory must be
/// new, empty, or already carry our own [`MARKER`].
///
/// The order matters as much as the check. The marker is planted only after
/// the directory has already proved itself, so a directory can never become
/// "ours" merely by having been written into once — the guard cannot arm
/// itself on somebody else's folder. And because the gate stands ahead of
/// the writing rather than inside the deletion, a filename collision is not
/// consent: a lone `config.json` of the user's is refused, not silently
/// replaced.
fn prepare_out_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .map_err(|e| anyhow::anyhow!("dump-contract: creating {}: {e}", dir.display()))?;
    if !is_ours(dir)? {
        return Err(not_ours(dir));
    }
    let marker = dir.join(MARKER);
    std::fs::write(&marker, MARKER_BODY)
        .map_err(|e| anyhow::anyhow!("dump-contract: writing {}: {e}", marker.display()))
}

/// Whether `dir` is one this command may write into: either it carries our
/// [`MARKER`], opening with [`MARKER_SIGNATURE`] — an empty or `touch`ed
/// file of that name proves nothing — or it is empty, and so has nothing to
/// lose.
fn is_ours(dir: &Path) -> anyhow::Result<bool> {
    if std::fs::read_to_string(dir.join(MARKER))
        .is_ok_and(|body| body.starts_with(MARKER_SIGNATURE))
    {
        return Ok(true);
    }
    let mut entries = std::fs::read_dir(dir)
        .map_err(|e| anyhow::anyhow!("dump-contract: reading {}: {e}", dir.display()))?;
    Ok(entries.next().is_none())
}

/// The one refusal sentence, shared by the gate and by the deletion step it
/// stands in front of.
fn not_ours(dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "dump-contract: {} already holds files and carries no {MARKER} marker of ours, \
         so it is not one of this command's dumps — it rewrites a fixed set of \
         ordinary names (config.json, health.json, trades.json, …) and deletes what a \
         previous dump left behind, so point --out at a new or empty directory (or at \
         web/e2e/fixtures/contract) rather than at somebody else's data",
        dir.display()
    )
}

/// Delete the `*.json` a previous dump left behind that this run no longer
/// produces, so a response that goes away cannot linger as a stale fixture.
///
/// [`prepare_out_dir`] has already proved the directory is ours by the time
/// `run` calls this, so the [`MARKER`] check below is a second lock on the
/// only irreversible act in the command rather than the only one: deleting
/// files is worth checking twice, and the check keeps this function safe to
/// call on its own. (`remove_dir_all` is banned outright by `clippy.toml`;
/// this removes files one at a time regardless.)
fn clear_stale_json(dir: &Path, writing: &BTreeSet<&str>) -> anyhow::Result<()> {
    let mut stale = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json")
            && !writing.contains(entry.file_name().to_string_lossy().as_ref())
        {
            stale.push(path);
        }
    }
    if stale.is_empty() {
        return Ok(());
    }
    if !is_ours(dir)? {
        return Err(not_ours(dir));
    }
    for path in stale {
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("dump-contract: removing {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Round every `f64` in the tree to [`FLOAT_DECIMALS`] decimals, so the file
/// does not freeze bits that belong to the host's libm (see the module doc).
/// Integers are left exactly as they are.
fn round_floats(value: &mut Value) {
    match value {
        Value::Number(n) if n.is_f64() => {
            let Some(x) = n.as_f64() else { return };
            let scale = 10f64.powi(FLOAT_DECIMALS);
            if let Some(rounded) = serde_json::Number::from_f64((x * scale).round() / scale) {
                *n = rounded;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(round_floats),
        Value::Object(map) => map.values_mut().for_each(round_floats),
        _ => {}
    }
}

/// Walk a JSON value, redacting every string: an exact match on
/// [`VERSION`] becomes `<version>` (so a price or an id that happens to
/// equal the version string is left alone); everything else has each
/// `replacements` needle substituted in order.
fn redact(value: &mut Value, replacements: &[(&str, &str)]) {
    match value {
        Value::String(s) => {
            if s == VERSION {
                *s = "<version>".to_string();
                return;
            }
            for (from, to) in replacements {
                if s.contains(from) {
                    *s = s.replace(from, to);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| redact(item, replacements)),
        Value::Object(map) => map.values_mut().for_each(|item| redact(item, replacements)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--out` is a path a human typed. A directory full of somebody else's
    /// JSON is refused, not emptied — even when one of their filenames
    /// happens to be one of ours; only our own marker unlocks the deletion.
    #[test]
    fn stale_json_is_only_cleared_from_a_directory_that_holds_a_previous_dump() {
        let dir = wingspan_app::demo::scratch_dir();
        let writing: BTreeSet<&str> = ["health.json", "config.json"].into_iter().collect();

        let theirs = dir.join("package-lock.json");
        std::fs::write(&theirs, "{}").unwrap();
        let refused = clear_stale_json(&dir, &writing).unwrap_err().to_string();
        assert!(refused.contains(MARKER), "{refused}");
        assert!(theirs.is_file(), "a refusal must not delete anything");

        // A name collision is not consent: `config.json` is one of the
        // commonest filenames there is, and the user's own copy of it beside
        // the user's own data must not authorise deleting that data.
        let collision = dir.join("config.json");
        std::fs::write(&collision, r#"{"theirs":true}"#).unwrap();
        let refused = clear_stale_json(&dir, &writing).unwrap_err().to_string();
        assert!(refused.contains(MARKER), "{refused}");
        assert!(
            theirs.is_file(),
            "an unrelated *.json must survive a name collision"
        );
        assert!(collision.is_file());

        // With the marker this command writes, the directory is a dump of
        // ours: the file we are about to rewrite stays, the stale one goes.
        std::fs::write(dir.join(MARKER), MARKER_BODY).unwrap();
        clear_stale_json(&dir, &writing).unwrap();
        assert!(collision.is_file(), "a file we rewrite is not deleted");
        assert!(!theirs.exists(), "the stale fixture is removed");
    }

    /// The gate is a precondition of *writing*, not merely of deleting. A
    /// directory whose only file is the user's own `config.json` — one of
    /// the fifteen names this command writes — is refused with its bytes
    /// intact, and no marker is planted, so the refusal cannot be undone by
    /// the very run it refused.
    #[test]
    fn a_directory_that_is_not_ours_is_refused_before_a_byte_is_written() {
        const THEIRS: &str = r#"{"my":"precious settings"}"#;
        for name in ["config.json", "notes.txt"] {
            let dir = wingspan_app::demo::scratch_dir();
            let theirs = dir.join(name);
            std::fs::write(&theirs, THEIRS).unwrap();

            let refused = prepare_out_dir(&dir).unwrap_err().to_string();
            assert!(refused.contains(MARKER), "{refused}");
            assert_eq!(
                std::fs::read_to_string(&theirs).unwrap(),
                THEIRS,
                "{name} must survive a refusal untouched"
            );
            assert!(
                !dir.join(MARKER).exists(),
                "a refusal must not plant the marker that would arm the next run"
            );
        }
    }

    /// An empty (or missing) directory becomes ours and stays ours; a file
    /// merely *named* like the marker does not make somebody else's folder
    /// ours.
    /// The promise on [`MARKER_SIGNATURE`]: a marker an older build wrote,
    /// with the body worded differently, still opens its directory.
    #[test]
    fn a_marker_an_older_build_wrote_still_opens_the_directory() {
        let dir = wingspan_app::demo::scratch_dir().join("older-marker");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(MARKER),
            "Written by `wingspan dump-contract`. Its presence lets that command rewrite\n",
        )
        .unwrap();
        std::fs::write(dir.join("trades.json"), "{}").unwrap();
        assert!(
            is_ours(&dir).unwrap(),
            "an old-wording marker is still ours"
        );
        prepare_out_dir(&dir).expect("the directory it has always owned");
    }

    #[test]
    fn an_empty_directory_becomes_ours_and_a_hand_made_marker_does_not() {
        assert!(
            MARKER_BODY.starts_with(MARKER_SIGNATURE),
            "the marker we write must be one we accept"
        );
        let dir = wingspan_app::demo::scratch_dir().join("fresh");
        prepare_out_dir(&dir).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(MARKER)).unwrap(),
            MARKER_BODY
        );

        // A second dump into our own directory is exactly the `just contract`
        // case: the fixtures are there, the marker is ours, it proceeds.
        std::fs::write(dir.join("trades.json"), "{}").unwrap();
        prepare_out_dir(&dir).unwrap();

        let theirs = wingspan_app::demo::scratch_dir();
        std::fs::write(theirs.join(MARKER), "touched\n").unwrap();
        std::fs::write(theirs.join("my-data.json"), "{}").unwrap();
        let refused = prepare_out_dir(&theirs).unwrap_err().to_string();
        assert!(refused.contains(MARKER), "{refused}");
        assert!(theirs.join("my-data.json").is_file());
    }

    /// The dumped floats are rounded, and only the floats: an integer count
    /// or an id must survive untouched.
    #[test]
    fn round_floats_trims_the_machine_dependent_digits_and_leaves_integers_alone() {
        let mut value = serde_json::json!({
            "theta": 375.594_707_708_738_84_f64,
            "iv": 0.223_814_024_445_064_49_f64,
            "whole": 100.0_f64,
            "count": 241,
            "id": "oct-strangle-2026-08-24",
            "nested": [{ "pnl": -16_496.836_910_049_107_f64 }],
        });
        round_floats(&mut value);
        assert_eq!(value["theta"], serde_json::json!(375.594_708));
        assert_eq!(value["iv"], serde_json::json!(0.223_814));
        assert_eq!(value["whole"], serde_json::json!(100.0));
        assert_eq!(value["count"], serde_json::json!(241));
        assert_eq!(value["id"], "oct-strangle-2026-08-24");
        assert_eq!(
            value["nested"][0]["pnl"],
            serde_json::json!(-16_496.836_910)
        );
    }

    #[test]
    fn redact_replaces_the_longer_path_before_the_shorter_one_it_contains() {
        let mut value = serde_json::json!({
            "root": "/home/user/workspace/wingspan/fixtures/demo",
            "home_only": "/home/user/.cache/thing",
            "untouched": "spot price 100.5",
        });
        let replacements = [
            ("/home/user/workspace/wingspan", "<repo>"),
            ("/home/user", "<home>"),
        ];
        redact(&mut value, &replacements);
        assert_eq!(value["root"], "<repo>/fixtures/demo");
        assert_eq!(value["home_only"], "<home>/.cache/thing");
        assert_eq!(value["untouched"], "spot price 100.5");
    }

    #[test]
    fn redact_matches_the_version_string_exactly_not_as_a_substring() {
        let mut value = serde_json::json!({
            "version": VERSION,
            "id": format!("trade-{VERSION}-2026"),
        });
        redact(&mut value, &[]);
        assert_eq!(value["version"], "<version>");
        assert_eq!(value["id"], format!("trade-{VERSION}-2026"));
    }
}
