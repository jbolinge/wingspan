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
    let mut options = AppOptions {
        demo: args.demo,
        ..AppOptions::default()
    };
    if config.trades.root.is_none() {
        options.root_override = Some(crate::cli::demo_root()?);
    }
    let state = AppState::build(config.clone(), options).await?;
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
