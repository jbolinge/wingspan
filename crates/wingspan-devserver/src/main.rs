//! `wingspan-devserver` — the dev-only HTTP binary.
//!
//! Not the product: the shipped app is the Tauri desktop build in
//! `web/src-tauri`. This exists so the SPA can be developed in a browser and
//! so `web/e2e` has a process to drive. It binds 127.0.0.1 and nothing else.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use chrono::NaiveDate;
use clap::{Args, Parser, Subcommand};

use wingspan_app::config::{self, Layers, Loaded};
use wingspan_app::demo::{demo_root, pin_to};
use wingspan_app::state::{AppOptions, AppState};
use wingspan_devserver::serve;

/// The port `serve` listens on when `--port` is not given.
pub const DEFAULT_PORT: u16 = 8787;

#[derive(Debug, Parser)]
#[command(
    name = "wingspan-devserver",
    version,
    about = "Wingspan's dev-only HTTP server (the product is the desktop app)",
    long_about = None,
)]
struct Cli {
    /// Config file to merge last (before `WINGSPAN_*` environment overrides).
    #[arg(long, global = true, value_name = "PATH")]
    config: Option<PathBuf>,

    /// More logging (`-v` = debug, `-vv` = trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Serve the API (and, with `embed-ui`, the SPA) on 127.0.0.1.
    Serve(ServeArgs),
}

#[derive(Debug, Args)]
struct ServeArgs {
    /// Serve the bundled, wholly synthetic demo book (`fixtures/demo`) with the
    /// synthetic price source: no network, no account data.
    #[arg(long)]
    demo: bool,
    /// Serve this trades folder instead of `[trades] root` — or, with
    /// `--demo`, instead of the bundled demo book (how the e2e suite runs the
    /// anonymised test fixtures through the demo pin).
    #[arg(long, value_name = "DIR")]
    root: Option<PathBuf>,
    /// Do not serve the SPA; Vite does that on :5173 and proxies `/api` here.
    #[arg(long)]
    dev: bool,
    /// Value the book on this date instead of today — DIT/DTE *and* the prices,
    /// which have to agree; `--demo` pins it to the fixtures' own date unless
    /// this says otherwise.
    #[arg(long, value_name = "YYYY-MM-DD")]
    as_of: Option<NaiveDate>,
    /// Port on 127.0.0.1 to serve on.
    #[arg(long, value_name = "PORT", default_value_t = DEFAULT_PORT)]
    port: u16,
}

/// `default_value_t` is clap's default, not Rust's: `ServeArgs::default()` —
/// which every test builds from — has to agree with the command line, or a
/// test would prove nothing about the real port.
impl Default for ServeArgs {
    fn default() -> Self {
        Self {
            demo: false,
            root: None,
            dev: false,
            as_of: None,
            port: DEFAULT_PORT,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.verbose);
    let loaded = match config::load(cli.config.as_deref(), Layers::all()) {
        Ok(loaded) => loaded,
        Err(err) => {
            eprintln!("wingspan-devserver: {err:#}");
            return ExitCode::FAILURE;
        }
    };
    let Command::Serve(args) = cli.command;
    match serve(loaded, args).await {
        Ok(code) => code,
        Err(err) => {
            // A failure here is a sentence, not a backtrace.
            eprintln!("wingspan-devserver: {err:#}");
            ExitCode::FAILURE
        }
    }
}

async fn serve(loaded: Loaded, args: ServeArgs) -> anyhow::Result<ExitCode> {
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
        // Not an error: a fresh checkout has not been shown a folder yet, so
        // this is the ordinary first run. Say what will happen rather than
        // refusing to start.
        println!(
            "no trades loaded: the dashboard is empty until you choose a folder on this \
             computer from the status bar in the desktop app (or pass --root DIR / set \
             [trades] root / WINGSPAN_TRADES__ROOT, or run `wingspan-devserver serve \
             --demo`)."
        );
    }
    // `--dev` and a missing UI build are the same thing to the router.
    config.validate()?;
    let state = AppState::build(config, options).await?;
    serve::run(args.port, state).await?;
    Ok(ExitCode::SUCCESS)
}

/// The options `serve` starts the app with, and the one line `--demo` prints.
///
/// Split out of [`serve`] because the flags interact: `--demo` pins the
/// valuation day, `--as-of` outranks that pin, and both have to move the
/// pricing clock with them. Only a function that returns the options can be
/// tested on the *combination*; parsing the flags proves nothing about it.
fn serve_options(args: &ServeArgs) -> anyhow::Result<(AppOptions, Option<String>)> {
    let mut options = AppOptions {
        demo: args.demo,
        dev: args.dev,
        ..AppOptions::default()
    };
    let mut banner = None;
    if args.demo {
        let root = match &args.root {
            Some(root) => absolute(root)?,
            None => demo_root()?,
        };
        // The fixtures are a dated snapshot (see `pin_to`); an explicit
        // `--as-of` still outranks their own day, because the flag's whole
        // point is to ask "what did this look like on…".
        let as_of = args
            .as_of
            .unwrap_or_else(wingspan_app::demo::valuation_date);
        pin_to(&mut options, as_of);
        banner = Some(format!(
            "demo mode: fixtures at {}, valued as of {as_of}",
            root.display()
        ));
        options.root_override = Some(root);
    } else {
        if let Some(root) = &args.root {
            options.root_override = Some(absolute(root)?);
        }
        if let Some(as_of) = args.as_of {
            pin_to(&mut options, as_of);
        }
    }
    Ok((options, banner))
}

/// `--root` as the app reports it: canonical, for the same reason as
/// `demo_root` — the SPA shows this path as the folder in use, and it is the
/// same sentence the folder picker gives for a path it cannot open.
fn absolute(root: &Path) -> anyhow::Result<PathBuf> {
    wingspan_app::paths::canonical(root).map_err(|msg| anyhow::anyhow!("--root {msg}"))
}

/// Is there a book to serve at all?
///
/// The three sources `docs/CONFIG.md` lists, in any order: `[trades] root`,
/// the root the process was started with (`--demo` or `--root`), and the
/// folder chosen in the app. That last one is why this is not two `is_none()`
/// checks — after a folder is chosen and the app restarted, the record in the
/// state directory is the *only* root, and `AppState::build` boots on it
/// happily.
///
/// A chosen folder that has since gone away does not count: dropping it for
/// the run is exactly what boot does.
fn has_a_trades_root(config: &wingspan_app::config::Config, options: &AppOptions) -> bool {
    if config.trades.root.is_some() || options.root_override.is_some() {
        return true;
    }
    wingspan_app::saved_root::read(&config.market.resolved_state_dir())
        .ok()
        .flatten()
        .is_some_and(|saved| {
            wingspan_core::discovery::find_root_with(&saved, &config.trades.dir_names()).is_ok()
        })
}

fn init_tracing(verbose: u8) {
    use tracing_subscriber::{fmt, EnvFilter};

    let default = match verbose {
        0 => "wingspan_devserver=info,wingspan_app=info,wingspan_marketdata=info,warn",
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
    use wingspan_app::state::RootSource;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    /// The rot this pins: `serve --demo` used to value the fixtures at
    /// `Utc::now()`, so the demo the README and `web/e2e` run changed by
    /// itself as the calendar moved — and on 2026-09-18 BSH's near leg would
    /// have expired out of the trade entirely.
    #[tokio::test]
    async fn the_demo_is_valued_on_the_fixtures_own_day() {
        let as_of = demo::valuation_date();
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
        let config = demo::demo_config(demo::scratch_dir());
        options.root_override = Some(demo::fixtures_root());
        let state = AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        assert_eq!(state.valuation_date(), as_of);
        assert_eq!(state.book().book.open_trades().count(), 6);
    }

    /// What `--demo` serves is the made-up book, not the anonymised account
    /// data: the strangle and the condor, nothing from `fixtures/trades`.
    #[tokio::test]
    async fn the_demo_serves_the_synthetic_book() {
        let (options, banner) = serve_options(&ServeArgs {
            demo: true,
            ..ServeArgs::default()
        })
        .expect("the demo book is in the checkout");
        assert_eq!(options.root_override, Some(demo::demo_book_root()));
        assert!(banner.is_some_and(|line| line.contains("fixtures/demo")));

        let config = demo::demo_config(demo::scratch_dir());
        let state = AppState::build(config, options)
            .await
            .expect("the demo book builds");
        let mut names: Vec<_> = state
            .book()
            .book
            .open_trades()
            .map(|trade| trade.trade.name.clone())
            .collect();
        names.sort();
        assert_eq!(names, ["Nov Condor", "Oct Strangle"]);
    }

    /// `--root` is the folder to serve. With `--demo` it replaces the bundled
    /// book but keeps the pin and the `demo` source — the e2e suite serves
    /// `fixtures/trades` that way; without, it outranks `[trades] root` for
    /// this process.
    #[tokio::test]
    async fn serve_takes_a_root() {
        let fixtures = demo::fixtures_root();
        let cli = Cli::try_parse_from([
            "wingspan-devserver",
            "serve",
            "--demo",
            "--root",
            fixtures.to_str().unwrap(),
        ])
        .unwrap();
        let Command::Serve(args) = cli.command;
        assert_eq!(args.root.as_deref(), Some(fixtures.as_path()));

        let (options, banner) = serve_options(&args).unwrap();
        assert_eq!(options.root_override, Some(fixtures.clone()));
        assert_eq!(options.valuation_date, Some(demo::valuation_date()));
        assert!(banner.is_some_and(|line| line.contains(&fixtures.display().to_string())));
        let config = demo::demo_config(demo::scratch_dir());
        let state = AppState::build(config, options)
            .await
            .expect("the fixture book builds");
        assert_eq!(state.root().source, RootSource::Demo);
        assert_eq!(state.book().book.open_trades().count(), 6);

        let (options, banner) = serve_options(&ServeArgs {
            root: Some(fixtures.clone()),
            ..ServeArgs::default()
        })
        .unwrap();
        assert_eq!(options.root_override, Some(fixtures));
        assert_eq!(options.valuation_date, None, "no pin without --demo");
        assert!(banner.is_none());
        assert!(!options.demo);

        let missing = ServeArgs {
            root: Some(demo::scratch_dir().join("no-such-folder")),
            ..ServeArgs::default()
        };
        let err = serve_options(&missing).unwrap_err().to_string();
        assert!(err.contains("--root"), "{err}");
    }

    #[test]
    fn serve_takes_a_port_and_defaults_to_8787() {
        let cli = Cli::try_parse_from(["wingspan-devserver", "serve", "--port", "9001"]).unwrap();
        let Command::Serve(args) = cli.command;
        assert_eq!(args.port, 9001);

        let plain = Cli::try_parse_from(["wingspan-devserver", "serve"]).unwrap();
        let Command::Serve(args) = plain.command;
        assert_eq!(args.port, DEFAULT_PORT);
        // The struct default is the flag default: every test that builds
        // `ServeArgs::default()` must be talking about the real port.
        assert_eq!(ServeArgs::default().port, DEFAULT_PORT);
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
    /// which is exactly the disagreement `pin_to` exists to prevent.
    #[test]
    fn serve_takes_an_as_of() {
        let cli = Cli::try_parse_from([
            "wingspan-devserver",
            "serve",
            "--demo",
            "--as-of",
            "2026-08-20",
        ])
        .unwrap();
        let Command::Serve(args) = cli.command;
        assert!(args.demo);
        assert_eq!(args.as_of, NaiveDate::from_ymd_opt(2026, 8, 20));

        let plain = Cli::try_parse_from(["wingspan-devserver", "serve"]).unwrap();
        let Command::Serve(args) = plain.command;
        assert_eq!(args.as_of, None);

        // Well past the fixture pin, and in the other daylight-saving half of
        // the year, so a clock left behind would be obvious.
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        for demo in [true, false] {
            let (options, _) = serve_options(&ServeArgs {
                demo,
                as_of: Some(december),
                ..ServeArgs::default()
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
}
