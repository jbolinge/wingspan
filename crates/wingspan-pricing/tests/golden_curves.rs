//! Golden test: the six default curves over the deterministic synthetic
//! surface, committed as CSV.
//!
//! This is the regression net for the whole pipeline — parity calibration, the
//! IV solve, sticky-strike repricing, intrinsic settlement and the cash-flow
//! arithmetic all have to stay put for these 246 x 6 numbers to hold (241
//! uniform samples plus spot and the strikes as exact points). It is
//! also a readable artefact: open the CSV and you are looking at the same
//! curves the UI draws.
//!
//! Regenerate deliberately, never reflexively:
//!
//! ```text
//! UPDATE_GOLDEN=1 cargo test -p wingspan-pricing --test golden_curves
//! ```

mod common;

use std::fmt::Write as _;
use std::path::PathBuf;

use wingspan_pricing::risk_graph::{self, RiskGraphSpec};

/// Values are dollars; this is far tighter than any real change and loose
/// enough to survive a different platform's `exp`/`ln`.
const TOLERANCE: f64 = 1e-6;

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/risk_curves.csv")
}

fn render() -> String {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let graph = risk_graph::compute(&common::book(), &v, &RiskGraphSpec::default())
        .expect("the synthetic book always draws");

    assert_eq!(graph.lines.len(), 6, "the golden file has six curves");
    let mut out = String::from("price");
    for line in &graph.lines {
        write!(out, ",{}", line.key).unwrap();
    }
    out.push('\n');
    for (i, price) in graph.prices.iter().enumerate() {
        write!(out, "{price:.6}").unwrap();
        for line in &graph.lines {
            write!(out, ",{:.6}", line.pnl[i]).unwrap();
        }
        out.push('\n');
    }
    out
}

fn parse(csv: &str) -> (Vec<String>, Vec<Vec<f64>>) {
    let mut lines = csv.lines();
    let header = lines
        .next()
        .expect("golden csv has a header")
        .split(',')
        .map(str::to_string)
        .collect();
    let rows = lines
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            line.split(',')
                .map(|cell| cell.parse::<f64>().expect("golden csv holds numbers"))
                .collect()
        })
        .collect();
    (header, rows)
}

#[test]
fn six_curves_match_the_golden_file() {
    let rendered = render();
    let path = golden_path();
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &rendered).expect("golden file is writable");
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "missing golden file {}: {e}\nregenerate with UPDATE_GOLDEN=1",
            path.display()
        )
    });

    let (expected_header, expected_rows) = parse(&expected);
    let (actual_header, actual_rows) = parse(&rendered);
    assert_eq!(actual_header, expected_header, "curve set changed");
    assert_eq!(actual_rows.len(), expected_rows.len(), "row count changed");

    for (row, (actual, want)) in actual_rows.iter().zip(&expected_rows).enumerate() {
        assert_eq!(actual.len(), want.len(), "row {row} width changed");
        for (column, (got, expected)) in actual.iter().zip(want).enumerate() {
            let slack = TOLERANCE * expected.abs().max(1.0);
            assert!(
                (got - expected).abs() <= slack,
                "row {row} column {} ({}): {got} != {expected}\n\
                 regenerate with UPDATE_GOLDEN=1 if this change is intended",
                column,
                expected_header[column],
            );
        }
    }
}

/// The golden file is only meaningful if it really is the P&L the dashboard
/// would print, so pin the anchor value too.
#[test]
fn the_golden_t0_curve_still_passes_through_the_mark_based_pnl() {
    let (params, forwards, surface) = common::market();
    let v = common::valuation(&params, &forwards, &surface);
    let book = common::book();
    let expected = common::pnl_from_marks(&book, &surface, true);
    let point = risk_graph::what_if(&book, &v, common::SPOT, v.now, 0.0, true);
    assert!(
        (point.pnl - expected).abs() < 1e-6,
        "{} != {expected}",
        point.pnl
    );
}
