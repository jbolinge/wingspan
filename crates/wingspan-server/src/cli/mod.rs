//! The `wingspan` command line.
//!
//! `serve` is the product; the other four are the ones the plan calls the
//! user's sanity checks — `validate` (does the app read my book the way I
//! do?), `risk` (the curve as numbers), `schwab status` and `doctor` (why is
//! the data not live?). All four are deliberately usable with no network and
//! no UI build.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};

pub mod doctor;
pub mod risk;
pub mod schwab;
pub mod validate;

#[derive(Debug, Parser)]
#[command(
    name = "wingspan",
    version,
    about = "SPX options position dashboard & risk analyzer",
    long_about = None,
)]
pub struct Cli {
    /// Config file to merge last (before `WINGSPAN_*` environment overrides).
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// More logging (`-v` = debug, `-vv` = trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Defaults to `serve`.
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the dashboard.
    Serve(ServeArgs),
    /// Parse a trades folder and print everything derived from it.
    Validate(ValidateArgs),
    /// Print a trade's risk curve as a table or CSV.
    Risk(RiskArgs),
    /// Schwab credentials and token diagnostics.
    #[command(subcommand)]
    Schwab(SchwabCommand),
    /// Check config, folders, python, the token file and the providers.
    Doctor(DoctorArgs),
}

#[derive(Debug, Default, Args)]
pub struct ServeArgs {
    /// Serve the bundled fixtures with the synthetic price source: no network,
    /// no account data.
    #[arg(long)]
    pub demo: bool,
    /// Do not serve the SPA; Vite does that on :5173 and proxies `/api` here.
    #[arg(long)]
    pub dev: bool,
    /// Value the book on this date instead of today — DIT/DTE *and* the prices,
    /// which have to agree; `--demo` pins it to the fixtures' own date unless
    /// this says otherwise.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub as_of: Option<NaiveDate>,
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

#[derive(Debug, Subcommand)]
pub enum SchwabCommand {
    /// Token file, refresh-token age and request budget.
    Status,
}

#[derive(Debug, Default, Args)]
pub struct DoctorArgs {
    /// Also resolve the provider chain once (this touches the network).
    #[arg(long)]
    pub probe: bool,
}

/// Parse the command line and run it.
pub async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    match dispatch(cli).await {
        Ok(code) => code,
        Err(err) => {
            // A CLI failure is a sentence, not a backtrace.
            eprintln!("wingspan: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(cli: Cli) -> anyhow::Result<ExitCode> {
    let loaded = crate::config::load(cli.config.as_deref())?;
    match cli.command.unwrap_or(Command::Serve(ServeArgs::default())) {
        Command::Serve(args) => serve(loaded, args).await,
        Command::Validate(args) => validate::run(&loaded.config, &args),
        Command::Risk(args) => risk::run(&loaded.config, &args).await,
        Command::Schwab(SchwabCommand::Status) => schwab::status(&loaded.config).await,
        Command::Doctor(args) => doctor::run(&loaded, &args).await,
    }
}

async fn serve(loaded: crate::config::Loaded, args: ServeArgs) -> anyhow::Result<ExitCode> {
    let config = loaded.config;
    let (options, banner) = serve_options(&args)?;
    if let Some(banner) = banner {
        println!("{banner}");
    }
    if args.dev {
        println!(
            "--dev: this process serves /api only; run `npm run dev` in web/ and \
             browse http://localhost:5173"
        );
    }
    if !has_a_trades_root(&config, &options) {
        anyhow::bail!(
            "[trades] root is not set. Put one in wingspan.toml, set \
             WINGSPAN_TRADES__ROOT, or run `wingspan serve --demo`."
        );
    }
    // `--dev` and a missing UI build are the same thing to the router.
    config.validate()?;
    let state = crate::state::AppState::build(config.clone(), options).await?;
    crate::serve::run(&config, state).await?;
    Ok(ExitCode::SUCCESS)
}

/// The options `serve` starts the app with, and the one line `--demo` prints.
///
/// Split out of [`serve`] because the flags interact: `--demo` pins the
/// valuation day, `--as-of` outranks that pin, and both have to move the
/// pricing clock with them. Only a function that returns the options can be
/// tested on the *combination*; parsing the flags proves nothing about it.
fn serve_options(args: &ServeArgs) -> anyhow::Result<(crate::state::AppOptions, Option<String>)> {
    let mut options = crate::state::AppOptions {
        demo: args.demo,
        dev: args.dev,
        ..crate::state::AppOptions::default()
    };
    let mut banner = None;
    if args.demo {
        let root = demo_root()?;
        // The fixtures are a dated snapshot (see `pin_to`); an explicit
        // `--as-of` still outranks their own day, because the flag's whole
        // point is to ask "what did this look like on…".
        let as_of = args.as_of.unwrap_or_else(crate::testing::valuation_date);
        pin_to(&mut options, as_of);
        banner = Some(format!(
            "demo mode: fixtures at {}, valued as of {as_of}",
            root.display()
        ));
        options.root_override = Some(root);
    } else if let Some(as_of) = args.as_of {
        pin_to(&mut options, as_of);
    }
    Ok((options, banner))
}

/// Is there a book to serve at all?
///
/// The three sources `docs/CONFIG.md` lists, in any order: `[trades] root`,
/// the root the process was started with (`--demo`), and the folder the user
/// picked in the status bar. That last one is why this is not two
/// `is_none()` checks — after `serve --demo`, picking a real folder, and a
/// restart as plain `serve`, the override is the *only* root, and
/// [`crate::state::AppState::build`] would boot on it happily.
///
/// A saved folder that has since gone away does not count: dropping it is
/// exactly what boot does, so refusing here keeps the friendly sentence
/// instead of a discovery dump.
fn has_a_trades_root(config: &crate::config::Config, options: &crate::state::AppOptions) -> bool {
    if config.trades.root.is_some() || options.root_override.is_some() {
        return true;
    }
    crate::root_override::read(&config.market.resolved_state_dir())
        .ok()
        .flatten()
        .is_some_and(|saved| {
            wingspan_core::discovery::find_root_with(&saved, &config.trades.dir_names()).is_ok()
        })
}

/// Stop the clock on `as_of`.
///
/// Two fields, always together. `valuation_date` is what DIT/DTE and "has this
/// leg expired" are measured against; `clock` is what the surface solves and
/// prices at. Setting one without the other is the bug this function exists to
/// make impossible: a leg between the two dates is counted as live by the
/// first and priced as expired by the second, so the position renders with a
/// leg whose value is structurally zero.
///
/// `--demo` pins to the fixtures' own day for a second reason.
/// `fixtures/trades` is a dated snapshot: its statements end on 2026-08-28 and
/// its nearest expiry is weeks after that. Priced against a real `now`, a
/// `--demo` run silently changes as the calendar moves — DIT and DTE drift,
/// the T+x ladder collapses, and once a real date passes a fixture expiry the
/// leg drops out of the position altogether and BSH becomes a different trade.
/// That is the demo the README's Quick Start shows a new user and the one
/// `web/e2e` measures against a committed screenshot, so it pins exactly what
/// the Rust harness pins ([`crate::testing`]).
fn pin_to(options: &mut crate::state::AppOptions, as_of: NaiveDate) {
    options.valuation_date = Some(as_of);
    options.clock = crate::testing::clock_at(as_of);
}

/// Where `--demo` finds `fixtures/trades`: next to the working directory when
/// running from a checkout, else next to the binary, else the source tree.
///
/// Canonical, because this is the root the whole app then reports:
/// `GET /api/v1/trades/root` and `/api/v1/config` used to answer
/// `"fixtures/trades"`, which is not a folder anyone can find from the
/// browser — and is a different folder to any process with another working
/// directory.
pub fn demo_root() -> anyhow::Result<PathBuf> {
    let mut candidates = vec![PathBuf::from("fixtures/trades")];
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("fixtures/trades"));
        }
    }
    candidates.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/trades"));
    candidates
        .into_iter()
        .find(|path| path.is_dir())
        .map(|path| std::fs::canonicalize(&path).unwrap_or(path))
        .ok_or_else(|| anyhow::anyhow!("could not find fixtures/trades for --demo"))
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default = match verbose {
        0 => "wingspan_server=info,wingspan_marketdata=info,warn",
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

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_subcommand_means_serve() {
        let cli = Cli::try_parse_from(["wingspan"]).unwrap();
        assert!(cli.command.is_none());
    }

    #[test]
    fn validate_takes_a_directory_and_an_as_of() {
        let cli = Cli::try_parse_from([
            "wingspan",
            "validate",
            "fixtures/trades",
            "--as-of",
            "2026-08-31",
        ])
        .unwrap();
        match cli.command {
            Some(Command::Validate(args)) => {
                assert_eq!(args.dir.unwrap(), PathBuf::from("fixtures/trades"));
                assert_eq!(args.as_of, NaiveDate::from_ymd_opt(2026, 8, 31));
            }
            other => panic!("parsed {other:?}"),
        }
    }

    /// The regression: `serve --demo` -> pick a real folder in the status bar
    /// -> quit -> plain `wingspan serve` used to refuse to start ("[trades]
    /// root is not set") even though the saved folder is exactly what boot
    /// would have used.
    #[test]
    fn the_folder_picked_in_the_app_counts_as_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let mut config = crate::config::Config::default();
        config.market.state_dir = Some(state_dir.clone());
        let options = crate::state::AppOptions::default();

        assert!(
            !has_a_trades_root(&config, &options),
            "nothing configured, nothing saved"
        );

        crate::root_override::write(&state_dir, &crate::testing::fixtures_root()).unwrap();
        assert!(
            has_a_trades_root(&config, &options),
            "the folder picked in the app was ignored"
        );

        // Gone since it was picked: boot drops it, so `serve` must still say
        // the friendly sentence rather than start and fail on discovery.
        crate::root_override::write(&state_dir, &dir.path().join("unmounted")).unwrap();
        assert!(!has_a_trades_root(&config, &options));

        // And the two ordinary sources still count on their own.
        crate::root_override::remove(&state_dir).unwrap();
        config.trades.root = Some(crate::testing::fixtures_root());
        assert!(has_a_trades_root(&config, &options));
        config.trades.root = None;
        assert!(has_a_trades_root(
            &config,
            &crate::state::AppOptions {
                root_override: Some(crate::testing::fixtures_root()),
                ..crate::state::AppOptions::default()
            }
        ));
    }

    /// The regression: `serve --demo` reported `"root": "fixtures/trades"` —
    /// a path relative to whatever directory the server happened to be
    /// started from, which the SPA then showed as the folder in use.
    #[tokio::test]
    async fn the_demo_root_is_absolute_everywhere_it_is_reported() {
        let root = demo_root().expect("the fixtures are in the checkout");
        assert!(root.is_absolute(), "{}", root.display());
        assert!(
            !root.components().any(|c| c.as_os_str() == ".."),
            "{}",
            root.display()
        );

        // And what the API answers with is that same absolute path.
        let mut config = crate::testing::demo_config(crate::testing::scratch_dir());
        config.trades.root = None;
        let options = crate::state::AppOptions {
            root_override: Some(root.clone()),
            ..crate::testing::demo_options()
        };
        let state = crate::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        let active = state.root();
        assert_eq!(active.source, crate::state::RootSource::Demo);
        assert_eq!(active.root.root, root);
    }

    /// The rot this pins: `serve --demo` used to value the fixtures at
    /// `Utc::now()`, so the demo the README and `web/e2e` run changed by
    /// itself as the calendar moved — and on 2026-09-18 BSH's near leg would
    /// have expired out of the trade entirely.
    #[tokio::test]
    async fn the_demo_is_valued_on_the_fixtures_own_day() {
        let as_of = crate::testing::valuation_date();
        let (mut options, banner) = serve_options(&ServeArgs {
            demo: true,
            ..ServeArgs::default()
        })
        .expect("the fixtures are in the checkout");
        assert_eq!(options.valuation_date, Some(as_of));
        // The prices have to be solved on that same day, or a leg would be
        // counted as live by DTE and priced as expired by the surface.
        assert_eq!(options.clock.now().date_naive(), as_of);
        assert!(
            banner.is_some_and(|line| line.contains(&as_of.to_string())),
            "the pinned day is printed, so nobody wonders why the DTEs differ"
        );

        // And it is the day the fixtures are actually read against.
        let config = crate::testing::demo_config(crate::testing::scratch_dir());
        options.root_override = Some(crate::testing::fixtures_root());
        let state = crate::state::AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        assert_eq!(state.valuation_date(), as_of);
        assert_eq!(state.book().book.open_trades().count(), 6);
    }

    /// A plain `serve` is valued at whatever today is — the pin is `--demo`'s
    /// and `--as-of`'s, never the default.
    #[test]
    fn a_plain_serve_is_not_pinned_to_anything() {
        let (options, banner) = serve_options(&ServeArgs::default()).unwrap();
        assert_eq!(options.valuation_date, None, "the default is `now`");
        assert!(banner.is_none());
        assert!(options.root_override.is_none());
    }

    /// `--as-of` is the escape hatch, and it outranks the demo pin.
    ///
    /// The regression: it used to set `valuation_date` alone, leaving the
    /// clock on the fixture instant — so `serve --demo --as-of 2026-12-01`
    /// measured DTE from December while the surface still priced in August,
    /// which is exactly the disagreement [`pin_to`] exists to prevent.
    #[test]
    fn serve_takes_an_as_of() {
        let cli =
            Cli::try_parse_from(["wingspan", "serve", "--demo", "--as-of", "2026-08-20"]).unwrap();
        match cli.command {
            Some(Command::Serve(args)) => {
                assert!(args.demo);
                assert_eq!(args.as_of, NaiveDate::from_ymd_opt(2026, 8, 20));
            }
            other => panic!("parsed {other:?}"),
        }
        let plain = Cli::try_parse_from(["wingspan", "serve"]).unwrap();
        match plain.command {
            Some(Command::Serve(args)) => assert_eq!(args.as_of, None),
            other => panic!("parsed {other:?}"),
        }

        // Well past the fixture pin, and in the other daylight-saving half of
        // the year, so a clock left behind would be obvious.
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        for demo in [true, false] {
            let (options, _) = serve_options(&ServeArgs {
                demo,
                dev: false,
                as_of: Some(december),
            })
            .expect("the fixtures are in the checkout");
            assert_eq!(options.valuation_date, Some(december), "demo={demo}");
            assert_eq!(
                options.clock.now().date_naive(),
                december,
                "demo={demo}: the clock stayed on the day the date left behind"
            );
        }
    }

    #[test]
    fn schwab_status_parses() {
        let cli = Cli::try_parse_from(["wingspan", "schwab", "status"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Command::Schwab(SchwabCommand::Status))
        ));
    }
}
