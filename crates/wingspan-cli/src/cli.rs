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
use wingspan_app::state::AppOptions;

use crate::{doctor, dump_contract, risk, validate};

// `demo_root` and `pin_to` live beside the fixtures they describe, in
// `wingspan_app::demo`: the desktop app probes the same three checkout
// candidates before falling back to its bundled resources, and the dev
// server pins the same day. Re-exported so every `crate::cli::` call site
// here reads as it always did.
pub use wingspan_app::demo::{demo_root, pin_to};

#[derive(Debug, Parser)]
#[command(
    name = "wingspan-cli",
    version,
    about = "Wingspan's headless checks: validate, risk, doctor, dump-contract",
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
    /// Parse a trades folder and print everything derived from it.
    Validate(ValidateArgs),
    /// Print a trade's risk curve as a table or CSV.
    Risk(RiskArgs),
    /// Check config, folders, python, the token file and the providers.
    Doctor(DoctorArgs),
    /// Dump every API response for the demo book as JSON (the frozen contract
    /// web/e2e/fixtures/contract holds; CI re-dumps and diffs it).
    DumpContract(DumpContractArgs),
}

#[derive(Debug, Args)]
pub struct ValidateArgs {
    /// Folder holding `Open Trades` / `Closed Trades` / `Daily Trade
    /// Statement`; defaults to `[trades] root`.
    pub dir: Option<PathBuf>,
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
        Command::Validate(args) => validate::run(&loaded.config, &args),
        Command::Risk(args) => risk::run(&loaded.config, &args).await,
        Command::Doctor(args) => doctor::run(&loaded, &args).await,
        Command::DumpContract(args) => dump_contract::run(&args).await,
    }
}

/// Is there a book to read at all?
///
/// The three sources `docs/CONFIG.md` lists, in any order: `[trades] root`,
/// the root the process was started with (`--demo` or `--root`), and the
/// folder the user chose in the app. That last one is why this is not two
/// `is_none()` checks — after a folder is chosen and the app restarted, the
/// record in the state directory is the *only* root, and
/// [`wingspan_app::state::AppState::build`] boots on it happily.
///
/// A chosen folder that has since gone away does not count: dropping it for
/// the run is exactly what boot does.
pub(crate) fn has_a_trades_root(config: &Config, options: &AppOptions) -> bool {
    if config.trades.root.is_some() || options.root_override.is_some() {
        return true;
    }
    saved_trades_root(config).is_some()
}

/// The folder chosen in the app, if it is still a trades folder.
///
/// The discovery re-check is not decoration: without it `wingspan-cli risk M1`
/// would silently draw the demo book whenever the chosen folder had been
/// unmounted, and `validate` with no argument would report on the fixtures
/// rather than saying it has nothing to read.
pub(crate) fn saved_trades_root(config: &Config) -> Option<PathBuf> {
    wingspan_app::saved_root::read(&config.market.resolved_state_dir())
        .ok()
        .flatten()
        .filter(|saved| {
            wingspan_core::discovery::find_root_with(saved, &config.trades.dir_names()).is_ok()
        })
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
    use wingspan_app::saved_root;
    use wingspan_app::state::AppState;

    /// The record choosing a folder in the app leaves behind.
    fn chosen(root: std::path::PathBuf) -> saved_root::Record {
        saved_root::Record::local(root, chrono::Utc::now(), Vec::new())
    }

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn validate_takes_a_directory_and_an_as_of() {
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
                assert_eq!(args.dir.unwrap(), PathBuf::from("fixtures/trades"));
                assert_eq!(args.as_of, NaiveDate::from_ymd_opt(2026, 8, 31));
            }
            other => panic!("parsed {other:?}"),
        }
    }

    /// The regression: choose a folder in the app -> quit -> `wingspan-cli
    /// validate` (and `risk`) used to ignore it entirely, even though the
    /// saved folder is exactly what the app itself boots on.
    #[test]
    fn the_folder_chosen_in_the_app_counts_as_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let mut config = Config::default();
        config.market.state_dir = Some(state_dir.clone());
        let options = AppOptions::default();

        assert!(
            !has_a_trades_root(&config, &options),
            "nothing configured, nothing saved"
        );

        saved_root::write(&state_dir, &chosen(demo::fixtures_root())).unwrap();
        assert!(
            has_a_trades_root(&config, &options),
            "the folder chosen in the app was ignored"
        );

        // Renamed or unmounted since it was chosen: boot drops it, so the
        // commands must fall back rather than fail on discovery.
        saved_root::write(&state_dir, &chosen(dir.path().join("unmounted"))).unwrap();
        assert!(!has_a_trades_root(&config, &options));

        // And the two ordinary sources still count on their own.
        saved_root::remove(&state_dir).unwrap();
        config.trades.root = Some(demo::fixtures_root());
        assert!(has_a_trades_root(&config, &options));
        config.trades.root = None;
        assert!(has_a_trades_root(
            &config,
            &AppOptions {
                root_override: Some(demo::fixtures_root()),
                ..AppOptions::default()
            }
        ));
    }

    /// The regression: `--demo` reported `"root": "fixtures/trades"` — a path
    /// relative to whatever directory the process happened to be started
    /// from, which the UI then showed as the folder in use.
    #[tokio::test]
    async fn the_demo_root_is_absolute_everywhere_it_is_reported() {
        let root = demo_root().expect("the fixtures are in the checkout");
        assert!(root.is_absolute(), "{}", root.display());
        assert!(
            !root.components().any(|c| c.as_os_str() == ".."),
            "{}",
            root.display()
        );
        assert_eq!(root, demo::demo_book_root(), "--demo serves fixtures/demo");

        // And what the app answers with is that same absolute path.
        let mut config = demo::demo_config(demo::scratch_dir());
        config.trades.root = None;
        let options = AppOptions {
            root_override: Some(root.clone()),
            ..demo::demo_options()
        };
        let state = AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let active = state.root();
        assert_eq!(active.source, wingspan_app::state::RootSource::Demo);
        assert_eq!(active.dir(), Some(root.as_path()));
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
        options.root_override = Some(demo::fixtures_root());
        let state = AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        assert_eq!(state.valuation_date(), as_of);
        assert_eq!(state.book().book.open_trades().count(), 6);
    }
}
