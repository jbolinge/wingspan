//! The `wingspan-cli` command line.
//!
//! The product is the desktop app; these are what `docs/DOMAIN.md` calls the user's
//! sanity checks — `validate` (does the app read my book the way I do?),
//! `risk` (the curve as numbers) and `doctor` (why is the data not live?, and
//! what the Schwab token looks like) — plus `dump-contract`, the drift
//! detector CI runs. All of them are deliberately usable with no network, no
//! window and no UI build.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};

use wingspan_app::config::Config;
use wingspan_app::source::SourceRequest;
use wingspan_app::state::AppOptions;

use crate::{doctor, dump_contract, export_legs, risk, validate};

// `demo_legs_csv` and `pin_to` live beside the fixtures they describe, in
// `wingspan_app::demo`. Re-exported so every `crate::cli::` call site here
// reads as it always did.
pub use wingspan_app::demo::{demo_legs_csv, pin_to};

#[derive(Debug, Parser)]
#[command(
    name = "wingspan-cli",
    version,
    about = "Wingspan's headless checks: validate, risk, doctor, dump-contract, export-legs",
    long_about = None,
)]
pub struct Cli {
    /// Config file to merge last (before `WINGSPAN_*` environment overrides).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// More logging (`-v` = debug, `-vv` = trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Read a trade source — a Legs CSV, a sheet, or the one the app is
    /// using — and print everything derived from it.
    Validate(ValidateArgs),
    /// Print a trade's risk curve as a table or CSV.
    Risk(RiskArgs),
    /// Check config, the trade source, python, the token file and the providers.
    Doctor(DoctorArgs),
    /// Dump every API response for the demo book as JSON (the frozen contract
    /// web/e2e/fixtures/contract holds; CI re-dumps and diffs it).
    DumpContract(DumpContractArgs),
    /// Fixture tooling: turn a TOS trades folder into a Legs CSV.
    ///
    /// Not a user feature and not a migration path — it exists so
    /// `fixtures/trades/legs.csv` and `fixtures/demo/legs.csv` keep
    /// `docs/DOMAIN.md`'s numbers reproducible on the Legs path.
    ExportLegs(ExportLegsArgs),
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// A Legs CSV, a Google Sheet link or id, or a TOS trades folder.
    /// Omit to read the source the app is using.
    pub source: Option<String>,
    /// Sheet tab to read; defaults to `[trades] tab`.
    #[arg(long, value_name = "NAME")]
    pub tab: Option<String>,
    /// Measure DIT/DTE from this date instead of today.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub as_of: Option<NaiveDate>,
    /// Print every netted leg, not just the summary.
    #[arg(long)]
    pub legs: bool,
    /// List every `info` warning instead of one summary line per kind.
    #[arg(long)]
    pub all_warnings: bool,
}

#[derive(Debug, Args)]
pub struct RiskArgs {
    /// Trade id (`m1-2026-07-14`) or name (`M1`); omit for the whole open book.
    pub trade: Option<String>,
    /// Machine-readable output.
    #[arg(long)]
    pub csv: bool,
    /// Points on the price axis.
    #[arg(long)]
    pub points: Option<usize>,
    /// Graph dates: `today`, `+12d`, `expiry:2027-02-19` (repeatable).
    #[arg(long = "date", value_name = "SPEC")]
    pub dates: Vec<String>,
    /// Price the curve from the synthetic fixture surface.
    #[arg(long)]
    pub demo: bool,
}

#[derive(Debug, Default, Args)]
pub struct DoctorArgs {
    /// Also run the configured Python interpreter and resolve the provider
    /// chain once (this spawns a process and touches the network).
    #[arg(long)]
    pub probe: bool,
}

#[derive(Debug, Args)]
pub struct ExportLegsArgs {
    /// Folder holding `Open Trades` / `Closed Trades` / `Daily Trade Statement`.
    pub dir: PathBuf,
    /// Where to write the CSV (overwritten).
    #[arg(long, value_name = "FILE")]
    pub out: PathBuf,
    /// Pin the valuation date; the CSV's own cells do not depend on it.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub as_of: Option<NaiveDate>,
}

#[derive(Debug, Args)]
pub struct DumpContractArgs {
    /// Dump the bundled synthetic demo book. Required: this command has no
    /// other book it is allowed to serialise.
    #[arg(long)]
    pub demo: bool,
    /// Directory to write the JSON files into (created if missing).
    #[arg(long, value_name = "DIR")]
    pub out: PathBuf,
}

/// Parse the command line and run it.
pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match dispatch(cli).await {
        Ok(code) => code,
        Err(err) => {
            // A CLI failure is a sentence, not a backtrace.
            eprintln!("wingspan-cli: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> anyhow::Result<ExitCode> {
    let loaded =
        wingspan_app::config::load(cli.config.as_deref(), wingspan_app::config::Layers::all())?;
    match cli.command {
        Command::Validate(args) => validate::run(&loaded.config, &args).await,
        Command::Risk(args) => risk::run(&loaded.config, &args).await,
        Command::Doctor(args) => doctor::run(&loaded, &args).await,
        Command::DumpContract(args) => dump_contract::run(&args).await,
        Command::ExportLegs(args) => export_legs::run(&loaded.config, &args),
    }
}

/// Is there a book to read at all?
///
/// The sources `docs/CONFIG.md` lists, in any order: `[trades] sheet_url`,
/// `[trades] csv_path`, the source the process was started with (`--demo`,
/// or a source override), and the source the trader chose in the app. That
/// last one is why this is not a handful of `is_none()` checks — after a
/// source is chosen and the app restarted, the record in the state
/// directory is the *only* source, and
/// [`wingspan_app::state::AppState::build`] boots on it happily.
///
/// A saved sheet or CSV is trusted at face value here — re-fetching or
/// re-reading it to check liveness is [`AppState::build`]'s job, not this
/// one's.
pub(crate) fn has_a_trade_source(config: &Config, options: &AppOptions) -> bool {
    saved_or_configured_source(config).is_some() || options.source_override.is_some()
}

/// The source the app itself would boot on, for an argument-less
/// `validate`: the record the trader left in the state directory first,
/// then `[trades] sheet_url`, then `[trades] csv_path`. This is
/// deliberately the same ladder `wingspan_app::state`'s boot walks
/// (minus `--demo`, which is a flag of its own), so `validate` and the
/// dashboard never disagree about which book is in force.
pub(crate) fn saved_or_configured_source(config: &Config) -> Option<SourceRequest> {
    if let Some(request) = wingspan_app::saved_source::read(&config.market.resolved_state_dir())
        .ok()
        .flatten()
    {
        return Some(request);
    }
    if !config.trades.sheet_url.is_empty() {
        return Some(SourceRequest::Sheet {
            url: config.trades.sheet_url.clone(),
            tab: Some(config.trades.tab.clone()),
        });
    }
    config
        .trades
        .resolved_csv_path()
        .map(|path| SourceRequest::Csv { path })
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default = match verbose {
        0 => "wingspan_app=info,wingspan_marketdata=info,warn",
        1 => "debug,hyper=info,reqwest=info",
        _ => "trace,hyper=info,reqwest=info",
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use wingspan_app::demo;
    use wingspan_app::saved_source;
    use wingspan_app::source::{SourceRef, SourceRequest};
    use wingspan_app::state::AppState;

    /// The record choosing a CSV in the app leaves behind.
    fn chosen(path: std::path::PathBuf) -> saved_source::Record {
        saved_source::Record::chosen(SourceRequest::Csv { path }, chrono::Utc::now(), Vec::new())
    }

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn validate_takes_a_source_and_an_as_of() {
        let cli = Cli::try_parse_from([
            "wingspan-cli",
            "validate",
            "fixtures/trades",
            "--as-of",
            "2026-08-31",
        ])
        .unwrap();
        match cli.command {
            Command::Validate(args) => {
                assert_eq!(args.source.as_deref(), Some("fixtures/trades"));
                assert_eq!(args.as_of, NaiveDate::from_ymd_opt(2026, 8, 31));
            }
            other => panic!("parsed {other:?}"),
        }
    }

    /// The regression: choose a source in the app -> quit -> `wingspan-cli
    /// validate` (and `risk`) used to ignore it entirely, even though the
    /// saved source is exactly what the app itself boots on.
    #[test]
    fn the_source_chosen_in_the_app_counts_as_a_book() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let mut config = Config::default();
        config.market.state_dir = Some(state_dir.clone());
        let options = AppOptions::default();

        assert!(
            !has_a_trade_source(&config, &options),
            "nothing configured, nothing saved"
        );

        let csv = demo::fixtures_root().join("legs.csv");
        saved_source::write(&state_dir, &chosen(csv.clone())).unwrap();
        assert!(
            has_a_trade_source(&config, &options),
            "the source chosen in the app was ignored"
        );

        saved_source::remove(&state_dir).unwrap();
        assert!(!has_a_trade_source(&config, &options));

        // And the two ordinary sources still count on their own.
        config.trades.csv_path = csv.display().to_string();
        assert!(has_a_trade_source(&config, &options));
        config.trades.csv_path = String::new();
        assert!(has_a_trade_source(
            &config,
            &AppOptions {
                source_override: Some(SourceRef::Csv { path: csv }),
                ..AppOptions::default()
            }
        ));
    }

    /// The regression: `--demo` reported `"root": "fixtures/trades"` — a path
    /// relative to whatever directory the process happened to be started
    /// from, which the UI then showed as the folder in use.
    ///
    /// `--demo` now loads the Legs CSV rather than the folder, so it is this
    /// path that has to come back absolute and normalised everywhere it is
    /// reported.
    #[test]
    fn the_demo_legs_csv_is_absolute_everywhere_it_is_reported() {
        let csv = demo_legs_csv().expect("the fixtures are in the checkout");
        assert!(csv.is_absolute(), "{}", csv.display());
        assert!(
            !csv.components().any(|c| c.as_os_str() == ".."),
            "{}",
            csv.display()
        );
        assert!(csv.is_file(), "{}", csv.display());
        assert_eq!(
            csv,
            demo::demo_book_root().join("legs.csv"),
            "--demo serves fixtures/demo/legs.csv"
        );
    }

    /// The rot this pins: the demo used to be valued at `Utc::now()`, so the
    /// book the README and `web/e2e` show changed by itself as the calendar
    /// moved — and on 2026-09-18 BSH's near leg would have expired out of the
    /// trade entirely.
    ///
    /// `--demo` therefore pins **both** halves, and this is the assertion
    /// that says so: the valuation date the DTEs are measured from *and* the
    /// clock the surface is solved at, on the fixtures' own day. Setting one
    /// without the other prices a live leg as expired.
    #[tokio::test]
    async fn the_demo_pin_stops_the_valuation_date_and_the_clock_together() {
        let as_of = demo::valuation_date();
        let mut options = AppOptions::default();
        assert_eq!(options.valuation_date, None, "the default is `now`");

        pin_to(&mut options, as_of);
        assert_eq!(options.valuation_date, Some(as_of));
        assert_eq!(options.clock.now().date_naive(), as_of);

        // Well past the fixture pin, and in the other daylight-saving half of
        // the year, so a clock left behind would be obvious.
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        pin_to(&mut options, december);
        assert_eq!(options.valuation_date, Some(december));
        assert_eq!(
            options.clock.now().date_naive(),
            december,
            "the clock stayed on the day the date left behind"
        );

        // And the fixture pin is the day the fixtures are actually read
        // against: six open trades, none of them expired out.
        let config = demo::demo_config(demo::scratch_dir());
        pin_to(&mut options, as_of);
        options.source_override = Some(SourceRef::Csv {
            path: demo::fixtures_root().join("legs.csv"),
        });
        let state = AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        assert_eq!(state.valuation_date(), as_of);
        assert_eq!(state.book().book.open_trades().count(), 6);
    }
}
