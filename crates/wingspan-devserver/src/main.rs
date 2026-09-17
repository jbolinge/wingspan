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
use wingspan_app::source::SourceRef;
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
    /// Serve this Legs CSV instead of `[trades] csv_path` — the new shape of
    /// `--root`, which is going away.
    #[arg(long, value_name = "CSV")]
    legs: Option<PathBuf>,
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
            legs: None,
            dev: false,
            as_of: None,
            port: DEFAULT_PORT,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    init_tracing();
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
            "no trades loaded: the dashboard is empty until you import a Legs sheet or CSV \
             from the status bar in the desktop app (or pass --legs <csv> / --root DIR / set \
             [trades] sheet_url or csv_path / WINGSPAN_TRADES__ROOT, or run `wingspan-devserver \
             serve --demo`)."
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
///
/// `--legs` and `--root` are mutually exclusive: both are "start this run on
/// this trade source instead of whatever `wingspan.toml` says", and picking
/// one silently over the other would be a coin flip nobody asked for.
fn serve_options(args: &ServeArgs) -> anyhow::Result<(AppOptions, Option<String>)> {
    if args.legs.is_some() && args.root.is_some() {
        anyhow::bail!("pass --legs or --root, not both");
    }
    let mut options = AppOptions {
        demo: args.demo,
        dev: args.dev,
        ..AppOptions::default()
    };
    let mut banner = None;
    if args.demo {
        // `--legs`/`--root` replace the *bundled* demo book but keep the pin
        // and the demo wiring — this is how the e2e suite serves the
        // anonymised fixtures (as a folder) through the same pin a plain
        // `--demo` uses.
        let (source, shown) = if let Some(legs) = &args.legs {
            let path = absolute("--legs", legs)?;
            (SourceRef::Csv { path: path.clone() }, path)
        } else if let Some(root) = &args.root {
            let path = absolute("--root", root)?;
            (SourceRef::Folder { path: path.clone() }, path)
        } else {
            let path = demo_root()?;
            (SourceRef::Folder { path: path.clone() }, path)
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
            shown.display()
        ));
        options.source_override = Some(source);
    } else {
        if let Some(legs) = &args.legs {
            options.source_override = Some(SourceRef::Csv {
                path: absolute("--legs", legs)?,
            });
        } else if let Some(root) = &args.root {
            options.source_override = Some(SourceRef::Folder {
                path: absolute("--root", root)?,
            });
        }
        if let Some(as_of) = args.as_of {
            pin_to(&mut options, as_of);
        }
    }
    Ok((options, banner))
}

/// `--legs`/`--root` as the app reports them: canonical, for the same reason
/// as `demo_root` — the SPA shows this path as the source in use, and it is
/// the same sentence the picker gives for a path it cannot open. `flag` names
/// whichever one was passed, so the error says which flag to fix.
fn absolute(flag: &str, path: &Path) -> anyhow::Result<PathBuf> {
    wingspan_app::paths::canonical(path).map_err(|msg| anyhow::anyhow!("{flag} {msg}"))
}

/// Is there a book to serve at all?
///
/// The sources `docs/CONFIG.md` lists, in any order: a configured
/// `[trades] sheet_url` or `csv_path`, the deprecated `[trades] root`, the
/// source the process was started with (`--demo`, `--legs` or `--root`), and
/// whatever was chosen from the app and saved to `trade_source.json`. That
/// last one is why this is not a handful of `is_none()` checks — after a
/// source is chosen and the app restarted, the record in the state directory
/// is the *only* source, and `AppState::build` boots on it happily.
///
/// A saved sheet or CSV is trusted without re-running discovery — that is
/// exactly what boot itself will do, and re-fetching a sheet just to print a
/// banner would be a second network round trip for no reason. A saved
/// **folder** is different: it is transitional and a stale one is common (a
/// TOS export folder renamed or removed between runs), so its liveness is
/// still checked here, the same way it always was.
fn has_a_trades_root(config: &wingspan_app::config::Config, options: &AppOptions) -> bool {
    if config.trades.root.is_some()
        || !config.trades.sheet_url.is_empty()
        || config.trades.resolved_csv_path().is_some()
        || options.source_override.is_some()
    {
        return true;
    }
    match wingspan_app::saved_source::read(&config.market.resolved_state_dir()) {
        Ok(Some(wingspan_app::source::SourceRequest::Folder { path })) => {
            wingspan_core::discovery::find_root_with(&path, &config.trades.dir_names()).is_ok()
        }
        Ok(Some(_)) => true,
        _ => false,
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let default = "wingspan_devserver=info,wingspan_app=info,wingspan_marketdata=info,warn";
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
        options.source_override = Some(SourceRef::Folder {
            path: demo::fixtures_root(),
        });
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
        assert_eq!(
            options.source_override,
            Some(SourceRef::Folder {
                path: demo::demo_book_root()
            })
        );
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
        assert_eq!(
            options.source_override,
            Some(SourceRef::Folder {
                path: fixtures.clone()
            })
        );
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
        assert_eq!(
            options.source_override,
            Some(SourceRef::Folder { path: fixtures })
        );
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

    /// `--legs` is the CSV sibling of `--root`: it sets a `Csv` override
    /// instead of a `Folder` one, and the two flags refuse to be combined.
    #[test]
    fn serve_takes_legs() {
        let csv = demo::fixtures_root()
            .join("Open Trades")
            .join("BSH July 14 2026");
        let cli = Cli::try_parse_from([
            "wingspan-devserver",
            "serve",
            "--legs",
            csv.to_str().unwrap(),
        ])
        .unwrap();
        let Command::Serve(args) = cli.command;
        assert_eq!(args.legs.as_deref(), Some(csv.as_path()));

        let (options, banner) = serve_options(&args).unwrap();
        assert_eq!(
            options.source_override,
            Some(SourceRef::Csv { path: csv.clone() })
        );
        assert!(banner.is_none(), "no --demo, no banner");

        // Under --demo, --legs replaces the bundled book but keeps the pin
        // and names the CSV in the banner, exactly as --root does today.
        let (options, banner) = serve_options(&ServeArgs {
            demo: true,
            legs: Some(csv.clone()),
            ..ServeArgs::default()
        })
        .unwrap();
        assert_eq!(
            options.source_override,
            Some(SourceRef::Csv { path: csv.clone() })
        );
        assert_eq!(options.valuation_date, Some(demo::valuation_date()));
        assert!(
            banner.is_some_and(|line| line.contains(&csv.display().to_string())),
            "the CSV in use is named in the banner"
        );

        let missing = ServeArgs {
            legs: Some(demo::scratch_dir().join("no-such-file.csv")),
            ..ServeArgs::default()
        };
        let err = serve_options(&missing).unwrap_err().to_string();
        assert!(err.contains("--legs"), "{err}");
    }

    /// `--legs` and `--root` are two ways of saying "start on this source
    /// instead", so passing both is a mistake to catch, not a coin flip.
    #[test]
    fn legs_and_root_together_is_an_error() {
        let args = ServeArgs {
            legs: Some(demo::fixtures_root().join("some.csv")),
            root: Some(demo::fixtures_root()),
            ..ServeArgs::default()
        };
        let err = serve_options(&args).unwrap_err().to_string();
        assert!(err.contains("--legs"), "{err}");
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
        assert!(options.source_override.is_none());
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
