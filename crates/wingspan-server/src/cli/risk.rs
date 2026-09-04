//! `wingspan risk <trade>` — the risk curve as numbers.
//!
//! The same engine the chart uses, printed. `--csv` makes it pipeable into a
//! spreadsheet, which is how the trader checks the graph against ONE.

use std::fmt::Write as _;
use std::process::ExitCode;

use wingspan_pricing::risk_graph::{self, RiskGraphSpec};
use wingspan_pricing::GraphDate;

use crate::cli::RiskArgs;
use crate::config::Config;
use crate::state::{AppOptions, AppState};

pub async fn run(config: &Config, args: &RiskArgs) -> anyhow::Result<ExitCode> {
    let state = AppState::build(config.clone(), options_for(config, args)?).await?;
    let book = state.book();

    let positions = match &args.trade {
        None => book.open_positions(),
        Some(needle) => {
            let id = resolve(&book, needle)?;
            book.select(&[id])?
        }
    };
    if positions.is_empty() {
        anyhow::bail!("nothing to draw: no open trades matched");
    }

    let dates = if args.dates.is_empty() {
        None
    } else {
        Some(
            args.dates
                .iter()
                .map(|spec| spec.parse::<GraphDate>().map_err(|e| anyhow::anyhow!(e)))
                .collect::<anyhow::Result<Vec<_>>>()?,
        )
    };
    let spec = RiskGraphSpec {
        points: args.points.unwrap_or(config.risk_graph.points),
        dates,
        ..RiskGraphSpec::default()
    };

    let priced = state
        .priced()
        .map_err(|e| anyhow::anyhow!("{}", e.detail))?;
    let graph = risk_graph::compute(&positions, &priced.valuation(), &spec)?;

    if args.csv {
        print_csv(&graph);
    } else {
        print_table(&graph, &priced.snapshot.spot_source);
    }
    Ok(ExitCode::SUCCESS)
}

/// Where the book comes from and what day it is valued on.
///
/// With nothing to read — no `[trades] root`, no folder chosen in the app —
/// `risk` falls back to the bundled demo book (`fixtures/demo`, the made-up
/// one `serve --demo` shows) so the command is usable out of the box. That is
/// a dated snapshot, so the fallback also stops the clock on its own day: a
/// curve drawn against a real `now` is not the one the fixtures describe, and
/// after 2026-10-16 the demo strangle is not even a strangle any more.
///
/// The pin is keyed on **the same condition that chose the fixtures**, which
/// is the whole point. `--demo` only says where prices come from, so keying
/// the pin on `--demo` instead made the two disagree in both directions:
/// `risk M1 --demo` against a real configured book measured its DIT/DTE from a
/// fixture day in 2026 (negative DIT for anything opened later), and plain
/// `risk M1` with nothing configured read the fixtures and valued them at
/// today — exactly the rot the pin is for.
fn options_for(config: &Config, args: &RiskArgs) -> anyhow::Result<AppOptions> {
    let mut options = AppOptions {
        demo: args.demo,
        ..AppOptions::default()
    };
    if crate::cli::has_a_trades_root(config, &options) {
        return Ok(options);
    }
    options.root_override = Some(crate::cli::demo_root()?);
    crate::cli::pin_to(&mut options, crate::testing::valuation_date());
    Ok(options)
}

fn resolve(book: &crate::state::BookState, needle: &str) -> anyhow::Result<String> {
    if book.positions.contains_key(needle) {
        return Ok(needle.to_string());
    }
    let matches: Vec<&str> = book
        .book
        .trades
        .iter()
        .filter(|t| t.trade.name.eq_ignore_ascii_case(needle))
        .map(|t| t.trade.id.as_str())
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).to_string()),
        [] => anyhow::bail!(
            "no trade `{needle}`; known ids: {}",
            book.positions
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        ),
        many => anyhow::bail!("`{needle}` matches several trades: {}", many.join(", ")),
    }
}

fn print_csv(graph: &wingspan_pricing::RiskGraph) {
    let mut header = String::from("price");
    for line in &graph.lines {
        header.push(',');
        header.push_str(&line.key);
    }
    println!("{header}");
    for (i, price) in graph.prices.iter().enumerate() {
        let mut row = format!("{price:.2}");
        for line in &graph.lines {
            let _ = write!(row, ",{:.2}", line.pnl.get(i).copied().unwrap_or_default());
        }
        println!("{row}");
    }
}

fn print_table(graph: &wingspan_pricing::RiskGraph, source: &str) {
    println!(
        "spot {:.2} · priced from {source} · {} point(s)",
        graph.spot,
        graph.prices.len()
    );
    print!("{:>10}", "PRICE");
    for line in &graph.lines {
        print!(" {:>12}", line.label);
    }
    println!();
    // A full 241-row dump is unreadable; print every ~20th row plus the ends.
    let step = (graph.prices.len() / 20).max(1);
    for (i, price) in graph.prices.iter().enumerate() {
        if i % step != 0 && i + 1 != graph.prices.len() {
            continue;
        }
        print!("{price:>10.2}");
        for line in &graph.lines {
            print!(" {:>12.2}", line.pnl.get(i).copied().unwrap_or_default());
        }
        println!();
    }
    println!();
    for line in &graph.lines {
        println!(
            "{:<14} breakevens {:<28} max {:>12.2} / {:>12.2}",
            line.label,
            if line.breakevens.is_empty() {
                "none in range".to_string()
            } else {
                line.breakevens
                    .iter()
                    .map(|b| format!("{b:.2}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            },
            line.max_profit,
            line.max_loss,
        );
    }
    if let Some(pop) = graph.pop {
        println!("probability of profit at expiry {:.1}%", pop * 100.0);
    }
    if let (Some(bands), Some(iv)) = (graph.sigma_bands, graph.front_atm_iv) {
        println!(
            "front ATM IV {:.2}% · ±1σ {:.0}–{:.0} · ±2σ {:.0}–{:.0}",
            iv * 100.0,
            bands.one[0],
            bands.one[1],
            bands.two[0],
            bands.two[1]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    fn args(demo: bool) -> RiskArgs {
        RiskArgs {
            trade: None,
            csv: false,
            points: None,
            dates: Vec::new(),
            demo,
        }
    }

    /// The regression: the fixture *root* was chosen when nothing was
    /// configured, but the fixture *day* was pinned when `--demo` was passed.
    /// Those are independent, so they disagreed in both directions — and the
    /// wrong one is the dangerous one: `risk M1 --demo` against a real book
    /// measured DIT/DTE from a 2026 fixture day, so anything opened after it
    /// showed a negative DIT and anything that expired after it was counted as
    /// still live.
    #[test]
    fn the_fixture_pin_follows_the_fixture_root_not_dash_demo() {
        let mut config = crate::config::Config::default();
        // Never read (or write) the developer's own saved override.
        config.market.state_dir = Some(testing::scratch_dir());
        let state_dir = config.market.resolved_state_dir();

        // Nothing to read: the bundled demo book, valued on its own day.
        let options = options_for(&config, &args(false)).unwrap();
        assert_eq!(
            options.root_override,
            Some(crate::cli::demo_root().expect("the demo book is in the checkout"))
        );
        assert_eq!(options.root_override, Some(testing::demo_book_root()));
        assert_eq!(options.valuation_date, Some(testing::valuation_date()));
        assert_eq!(
            options.clock.now().date_naive(),
            testing::valuation_date(),
            "the prices must be solved on the day the book is valued"
        );

        // A configured book: `--demo` chooses the synthetic price source and
        // nothing else. Pinning here would re-date the user's own trades.
        config.trades.root = Some(testing::fixtures_root());
        let options = options_for(&config, &args(true)).unwrap();
        assert!(options.demo, "--demo still picks the fixture surface");
        assert!(options.root_override.is_none(), "read the configured book");
        assert_eq!(
            options.valuation_date, None,
            "the user's own book is valued today, not on a fixture day"
        );

        // A folder chosen in the app is just as much a real book: it outranks
        // the fixtures in `state::start_root`, so it must not be pinned
        // either.
        config.trades.root = None;
        crate::saved_root::write(
            &state_dir,
            &crate::saved_root::Record::local(
                testing::fixtures_root(),
                chrono::Utc::now(),
                Vec::new(),
            ),
        )
        .unwrap();
        let options = options_for(&config, &args(true)).unwrap();
        assert!(options.root_override.is_none());
        assert_eq!(options.valuation_date, None);
    }
}
