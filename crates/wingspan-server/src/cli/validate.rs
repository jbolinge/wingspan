//! `wingspan validate <dir>` — the user's sanity check.
//!
//! Reads a trades folder with no network and no UI, and prints everything the
//! dashboard would derive from it: the netted legs, the P&L at the newest
//! statement's own marks, every warning, the quarantined orders and the
//! quantity reconciliation. If this output disagrees with the trader's own
//! spreadsheet, the app is wrong — that is the whole point of the command.
//!
//! Marks come from the last statement's `Mark` column rather than a provider,
//! so the numbers are reproducible forever and the command works on a plane.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

use rust_decimal::Decimal;
use wingspan_core::book::{self, Book, LoadOptions};
use wingspan_core::position::MarkSource;
use wingspan_core::reconcile::{self, ReconStatus};
use wingspan_core::warning::Severity;
use wingspan_core::{AssembledTrade, Trade};

use crate::cli::ValidateArgs;
use crate::config::Config;

pub fn run(config: &Config, args: &ValidateArgs) -> anyhow::Result<ExitCode> {
    // With no argument: `[trades] root` first (a deployer's setting for this
    // machine), then whatever folder was chosen in the app — so `wingspan
    // validate` on its own reports on the book the dashboard is showing
    // rather than refusing.
    let start: PathBuf = args
        .dir
        .clone()
        .or_else(|| config.trades.resolved_root())
        .or_else(|| crate::cli::saved_trades_root(config))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "no directory given, [trades] root is not set in the config, and no folder \
                 has been chosen in the app"
            )
        })?;
    let options = LoadOptions {
        dir_names: config.trades.dir_names(),
        timezone: config.trades.timezone()?,
        valuation_date: args.as_of,
        ..LoadOptions::default()
    };
    let book = book::load_with(&start, &options)?;
    Ok(report(&book, args.legs, args.all_warnings))
}

/// Print the whole report; the exit code is `FAILURE` when something is
/// actually wrong (an `error` warning, or a reconciliation that does not
/// balance) rather than merely noteworthy.
pub fn report(book: &Book, show_legs: bool, all_warnings: bool) -> ExitCode {
    let marks = book.statement_marks();
    header(book, &marks);

    let open: Vec<&AssembledTrade> = book.open_trades().collect();
    let closed: Vec<&AssembledTrade> = book.closed_trades().collect();
    trade_table("Open trades", &open, book, &marks);
    trade_table("Closed trades", &closed, book, &marks);
    if show_legs {
        legs(book, &marks);
    }
    quarantined(book);
    let warning_counts = warnings(book, all_warnings);
    let recon_problems = reconciliation(book);
    unassigned_history(book);

    println!();
    println!(
        "{} open, {} closed · {} warning(s) · reconciliation {}",
        open.len(),
        closed.len(),
        book.warnings.len(),
        if recon_problems == 0 {
            "clean".to_string()
        } else {
            format!("{recon_problems} problem(s)")
        }
    );

    if warning_counts.errors > 0 || recon_problems > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn header(book: &Book, marks: &dyn MarkSource) {
    let coverage = &book.coverage;
    println!("Wingspan · validate");
    println!("  root         {}", book.root.root.display());
    for (label, dir) in [
        ("open", &book.root.open_dir),
        ("closed", &book.root.closed_dir),
        ("statements", &book.root.statements_dir),
    ] {
        match dir {
            Some(path) => println!("  {label:<12} {}", path.display()),
            None => println!("  {label:<12} (missing)"),
        }
    }
    let span = match (coverage.first, coverage.last) {
        (Some(first), Some(last)) => format!("{first} … {last}"),
        _ => "none".to_string(),
    };
    println!(
        "  statements   {} file(s), {span}; {} execution(s), {} claimed",
        coverage.statement_count, coverage.executions, coverage.claimed_executions
    );
    println!(
        "  marks        {}",
        book.as_of.map_or_else(
            || "none (legs valued at entry)".to_string(),
            |d| format!("{d} statement close"),
        )
    );
    println!("  valuation    {}", book.valuation_date);
    let _ = marks;
}

fn trade_table(title: &str, trades: &[&AssembledTrade], book: &Book, marks: &dyn MarkSource) {
    println!();
    println!("{title} ({})", trades.len());
    if trades.is_empty() {
        println!("  (none)");
        return;
    }
    println!(
        "  {:<10} {:<10} {:>4} {:>5} {:>11} {:>11} {:>11} {:>11} {:>9}  STRUCTURE",
        "NAME", "OPENED", "DIT", "DTE", "COST", "VALUE", "P&L OPEN", "P&L TOTAL", "%"
    );
    let mut total = Decimal::ZERO;
    for assembled in trades {
        let pnl = assembled.position.pnl(marks);
        total += pnl.total;
        println!(
            "  {:<10} {:<10} {:>4} {:>5} {:>11} {:>11} {:>11} {:>11} {:>9}  {}",
            truncate(&assembled.trade.name, 10),
            assembled.trade.opened_on,
            assembled.position.dit,
            assembled
                .position
                .dte
                .map_or_else(|| "—".to_string(), |d| d.to_string()),
            money(pnl.cost_basis),
            money(pnl.value),
            money(pnl.open),
            money(pnl.total),
            pnl.pct
                .map_or_else(|| "—".to_string(), |p| format!("{:.2}%", p.round_dp(2))),
            assembled.position.structure,
        );
    }
    println!(
        "  {:<10} {:<10} {:>4} {:>5} {:>11} {:>11} {:>11} {:>11}",
        "TOTAL",
        "",
        "",
        "",
        "",
        "",
        "",
        money(total)
    );
    let _ = book;
}

fn legs(book: &Book, marks: &dyn MarkSource) {
    println!();
    println!("Netted legs");
    for assembled in &book.trades {
        println!("  {} ({})", assembled.trade.name, assembled.trade.id);
        if assembled.position.legs.is_empty() && assembled.position.expired_legs.is_empty() {
            println!("    (flat)");
        }
        for leg in &assembled.position.legs {
            println!(
                "    {:>+4} {:<18} avg {:>10} mark {:>10} value {:>12}",
                leg.qty,
                leg.contract.to_tos_code(),
                leg.avg_cost.map_or_else(|| "—".to_string(), money),
                marks
                    .mark(&leg.contract)
                    .map_or_else(|| "—".to_string(), money),
                money(
                    marks.mark(&leg.contract).unwrap_or_default()
                        * Decimal::from(leg.qty)
                        * Decimal::from(leg.contract.multiplier)
                ),
            );
        }
        for leg in &assembled.position.expired_legs {
            println!(
                "    {:>+4} {:<18} EXPIRED",
                leg.qty,
                leg.contract.to_tos_code()
            );
        }
    }
}

fn quarantined(book: &Book) {
    let rows: Vec<(&str, &wingspan_core::QuarantinedOrder)> = book
        .trades
        .iter()
        .flat_map(|t| {
            t.position
                .quarantined_orders
                .iter()
                .map(move |q| (t.trade.id.as_str(), q))
        })
        .collect();
    println!();
    println!("Quarantined orders ({})", rows.len());
    if rows.is_empty() {
        println!("  (none)");
        return;
    }
    println!("  excluded from legs, cash flow and reconciliation");
    for (trade_id, order) in rows {
        let reason = match &order.reason {
            wingspan_core::QuarantineReason::DuplicateExecutionClaim { claimed_by } => {
                format!("claimed by {claimed_by}")
            }
            wingspan_core::QuarantineReason::Other { detail } => detail.clone(),
        };
        println!("  {trade_id:<20} {reason:<28} {}", order.raw);
    }
}

struct WarningCounts {
    errors: usize,
}

/// Errors and warnings in full; `info` collapsed to one line per kind unless
/// `all` is set, because fifty "skipped the Forex section" lines bury the six
/// that matter.
fn warnings(book: &Book, all: bool) -> WarningCounts {
    let mut by_severity: BTreeMap<Severity, Vec<&wingspan_core::Warning>> = BTreeMap::new();
    for warning in &book.warnings {
        by_severity
            .entry(warning.severity())
            .or_default()
            .push(warning);
    }
    println!();
    println!("Warnings ({})", book.warnings.len());
    if book.warnings.is_empty() {
        println!("  (none)");
    }
    // Loudest first.
    for severity in [Severity::Error, Severity::Warn, Severity::Info] {
        let Some(warnings) = by_severity.get(&severity) else {
            continue;
        };
        let label = match severity {
            Severity::Error => "error",
            Severity::Warn => "warn",
            Severity::Info => "info",
        };
        if severity == Severity::Info && !all {
            let mut by_code: BTreeMap<&str, (usize, String)> = BTreeMap::new();
            for warning in warnings {
                let entry = by_code
                    .entry(warning.code())
                    .or_insert((0, warning.message()));
                entry.0 += 1;
            }
            for (code, (count, example)) in by_code {
                if count == 1 {
                    println!("  {label:<6} {code:<30} {example}");
                } else {
                    println!("  {label:<6} {code:<30} {count}x, e.g. {example}");
                }
            }
            continue;
        }
        for warning in warnings {
            println!("  {label:<6} {:<30} {}", warning.code(), warning.message());
        }
    }
    if !all && by_severity.contains_key(&Severity::Info) {
        println!("  (re-run with --all-warnings to list every info line)");
    }
    WarningCounts {
        errors: by_severity.get(&Severity::Error).map_or(0, Vec::len),
    }
}

fn reconciliation(book: &Book) -> usize {
    let recon = &book.reconciliation;
    println!();
    println!(
        "Reconciliation vs the {} statement (quantities only)",
        recon
            .statement_as_of
            .map_or_else(|| "—".to_string(), |d| d.to_string())
    );
    if recon.rows.is_empty() {
        println!("  (no account positions to compare)");
        return 0;
    }
    println!(
        "  {:<20} {:>10} {:>8}  STATUS",
        "CONTRACT", "STATEMENT", "TRADES"
    );
    for row in &recon.rows {
        println!(
            "  {:<20} {:>10} {:>8}  {}",
            row.tos_code,
            format!("{:+}", row.statement_qty),
            format!("{:+}", row.trades_qty),
            status_label(row.status)
        );
    }
    let problems = recon.problems().count();
    println!(
        "  {} row(s), {} match, {problems} problem(s)",
        recon.rows.len(),
        recon.rows.len() - problems
    );
    println!("  {}", recon.note);
    problems
}

/// Positions the account held on *any* statement day that no trade file
/// explains — the JUL 26 box lives here, because it had already expired by the
/// newest statement and so never reaches the headline reconciliation.
fn unassigned_history(book: &Book) {
    let trades: Vec<Trade> = book.trades.iter().map(|t| t.trade.clone()).collect();
    let mut seen: BTreeMap<String, (chrono::NaiveDate, i32)> = BTreeMap::new();
    for statement in book.statements() {
        let recon = reconcile::reconcile(statement, &trades);
        for row in recon
            .rows
            .iter()
            .filter(|r| r.status == ReconStatus::Unassigned)
        {
            seen.entry(row.tos_code.clone())
                .or_insert((statement.as_of, row.statement_qty));
        }
    }
    // Anything still unassigned on the newest statement was already printed.
    if let Some(last) = book.as_of {
        seen.retain(|_, (date, _)| *date != last);
    }
    println!();
    println!(
        "Unassigned account positions in earlier statements ({})",
        seen.len()
    );
    if seen.is_empty() {
        println!("  (none)");
        return;
    }
    println!("  held by the account, explained by no trade file — a warning, not an error");
    for (code, (date, qty)) in seen {
        println!("  {date}  {qty:>+3} {code}");
    }
}

fn status_label(status: ReconStatus) -> &'static str {
    match status {
        ReconStatus::Match => "match",
        ReconStatus::QtyMismatch => "qty_mismatch",
        ReconStatus::Unassigned => "unassigned",
        ReconStatus::Missing => "missing",
    }
}

fn money(value: Decimal) -> String {
    format!("{:.2}", value.round_dp(2))
}

fn truncate(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    text.chars()
        .take(width.saturating_sub(1))
        .collect::<String>()
        + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_always_two_places() {
        assert_eq!(money(Decimal::new(-61378, 2)), "-613.78");
        assert_eq!(money(Decimal::ZERO), "0.00");
    }

    #[test]
    fn long_names_are_elided_not_wrapped() {
        assert_eq!(truncate("BSH", 10), "BSH");
        assert_eq!(truncate("ABCDEFGHIJKL", 6), "ABCDE…");
    }
}
