//! `wingspan dump-contract --demo --out <dir>` — the frozen API contract as
//! files.
//!
//! This is a drift detector, not a feature. It builds the app over the
//! bundled synthetic demo book, calls every function in
//! [`crate::service`] with a fixed, hard-coded request set, and writes each
//! response as pretty JSON — one file per call — into `--out`
//! (`web/e2e/fixtures/contract` is where the checked-in copy lives). CI
//! re-runs this command and `git diff --exit-code`s the result: a clean diff
//! means the API's response bodies are exactly what they were the day the
//! fixtures were last regenerated.
//!
//! It deliberately ignores the user's own configuration — no
//! `~/.config/wingspan/wingspan.toml`, no `./wingspan.toml`, no `WINGSPAN_*`
//! environment variable is read — so the output is byte-identical on every
//! machine that checks out the same commit. Everything machine-specific
//! (absolute paths under the checkout, the temp state directory, `$HOME`) is
//! redacted before it reaches disk.
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
use crate::dto::{RiskGraphRequest, WhatIfRequest};
use crate::error::ApiResult;
use crate::service;
use crate::state::{AppOptions, AppState};
use crate::VERSION;

/// The demo book's two open trades (`crates/wingspan-server/tests/cli.rs`,
/// `web/e2e/helpers.ts`'s `DEMO_BOOK_TRADE_IDS`). A fixed literal, never
/// read out of the book, so the request set the contract freezes cannot
/// silently change shape if the demo book is regenerated.
const OPEN_IDS: [&str; 2] = ["oct-strangle-2026-08-24", "nov-condor-2026-08-26"];

/// The demo book's one closed trade. Found once by running
/// `wingspan validate fixtures/demo --as-of 2026-08-31 --legs` and reading
/// the id it prints, then hard-coded here — never derived from the book at
/// dump time. Used only as a guard: if the demo book ever changes shape, this
/// command must fail loudly rather than quietly freeze a different contract.
const CLOSED_ID: &str = "sep-put-spread-2026-08-10";

pub async fn run(args: &DumpContractArgs) -> anyhow::Result<ExitCode> {
    if !args.demo {
        anyhow::bail!("dump-contract only knows how to dump the demo book; pass --demo");
    }

    // Hermetic config: no config file, no environment variable, so the dump
    // is identical everywhere. `state_dir` is a throwaway scratch directory
    // — never the user's own state — canonicalised so the path we redact
    // below is the same one that lands in the response bodies.
    let mut config = crate::config::Config::default();
    config.trades.root = None;
    let scratch = crate::demo::scratch_dir();
    let state_dir = std::fs::canonicalize(&scratch).unwrap_or(scratch);
    config.market.state_dir = Some(state_dir.clone());

    // Options: what `serve --demo` with no `--root`/`--as-of` builds, by
    // hand — `pin_to` keeps the valuation date and the pricing clock moving
    // together, exactly as `serve` itself insists on.
    let root = crate::cli::demo_root()?;
    let mut options = AppOptions {
        demo: true,
        dev: false,
        ..AppOptions::default()
    };
    let as_of = crate::demo::valuation_date();
    crate::cli::pin_to(&mut options, as_of);
    options.root_override = Some(root);

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
            "reconciliation.json",
            to_value(&service::reconciliation(&state))?,
        ),
        ("warnings.json", to_value(&service::warnings(&state))?),
        (
            "trade_by_id.json",
            dump(service::trade_by_id(&state, "oct-strangle-2026-08-24"))?,
        ),
        (
            "trade_by_id_unknown.json",
            dump(service::trade_by_id(&state, "ghost-2026-01-01"))?,
        ),
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
        ("reload.json", dump(service::reload(&state))?),
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

    std::fs::create_dir_all(&args.out)
        .map_err(|e| anyhow::anyhow!("dump-contract: creating {}: {e}", args.out.display()))?;
    let writing: BTreeSet<&str> = entries.iter().map(|(name, _)| *name).collect();
    clear_stale_json(&args.out, &writing)?;

    for (name, mut value) in entries {
        redact(&mut value, &replacements);
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

/// Delete the `*.json` a previous dump left behind that this run no longer
/// produces, so a response that goes away cannot linger as a stale fixture.
///
/// `--out` is a path a human typed, so this refuses to delete anything from a
/// directory that is not recognisably one of our own dumps: unrecognised JSON
/// beside none of the files we are about to write is somebody else's data, and
/// R2's rule — never delete a byte the user did not ask us to — applies to a
/// developer command as much as to `clear`. (`remove_dir_all` is banned
/// outright by `clippy.toml`; this removes files one at a time regardless.)
fn clear_stale_json(dir: &Path, writing: &BTreeSet<&str>) -> anyhow::Result<()> {
    let (mut stale, mut ours) = (Vec::new(), 0usize);
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            if writing.contains(entry.file_name().to_string_lossy().as_ref()) {
                // Rewritten below; nothing to delete.
                ours += 1;
            } else {
                stale.push(path);
            }
        }
    }
    if !stale.is_empty() && ours == 0 {
        anyhow::bail!(
            "dump-contract: {} holds JSON this command did not write, and none of the \
             files it does write — point --out at an empty directory (or at \
             web/e2e/fixtures/contract) rather than at somebody else's data",
            dir.display()
        );
    }
    for path in stale {
        std::fs::remove_file(&path)
            .map_err(|e| anyhow::anyhow!("dump-contract: removing {}: {e}", path.display()))?;
    }
    Ok(())
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
    /// JSON is refused, not emptied; one holding a previous dump is tidied.
    #[test]
    fn stale_json_is_only_cleared_from_a_directory_that_holds_a_previous_dump() {
        let dir = crate::demo::scratch_dir();
        let writing: BTreeSet<&str> = ["health.json", "trades.json"].into_iter().collect();

        let theirs = dir.join("package-lock.json");
        std::fs::write(&theirs, "{}").unwrap();
        let refused = clear_stale_json(&dir, &writing).unwrap_err().to_string();
        assert!(refused.contains("somebody else's data"), "{refused}");
        assert!(theirs.is_file(), "a refusal must not delete anything");

        // With one of our own files beside it, the directory is a dump of
        // ours: the file we are about to rewrite stays, the stale one goes.
        std::fs::write(dir.join("health.json"), "{}").unwrap();
        clear_stale_json(&dir, &writing).unwrap();
        assert!(dir.join("health.json").is_file());
        assert!(!theirs.exists(), "the stale fixture is removed");
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
