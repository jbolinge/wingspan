//! Settings, diagnostics, the demo book and the three "open this in the
//! desktop" commands.
//!
//! Split out of [`crate::commands`] only for length; every rule that file's
//! header states holds here word for word, and two more that are specific to
//! what these commands touch:
//!
//! 4. **No command takes a path from JS that changes what the app executes,
//!    or where it reads a secret from.** A JS-writable `[yfinance] python`
//!    would be arbitrary code execution on every refresh tick, and a
//!    JS-writable `[schwab] token_path` would aim the token reader at an
//!    attacker's file. So [`settings_update`] carries
//!    `wingspan_app::settings::SettingsPatch`, which is `deny_unknown_fields`
//!    and has no path key at all, and the four path-shaped settings that can
//!    be changed from the window — `[yfinance] python`, `[schwab] token_path`,
//!    `[schwab] env_file` and `[tastytrade] env_file` — are written only by
//!    [`pick_python`], [`pick_token_file`], [`pick_env_file`] and
//!    [`pick_tastytrade_env_file`] — each of
//!    which opens a **Rust** dialog, shows a **Rust** confirmation naming the
//!    chosen file, and hands the `Path` its own chooser produced to
//!    `wingspan_app::settings`. None of the four accepts an argument.
//!    [`reveal_folder`] does take a string, and answers only for three paths
//!    the application already knows: an XSS must not be able to use it as a
//!    general "open anything" primitive.
//! 5. **Credential and token files are read-only to this application.**
//!    [`pick_token_file`], [`pick_env_file`] and [`pick_tastytrade_env_file`]
//!    write the *path string* into `wingspan.toml` and nothing else — none of
//!    them opens, copies, chmods or creates the file it names. tastytrade has
//!    no token file at all: its refresh token is a static environment variable
//!    and its access token lives only in memory, so there is nothing on disk
//!    for [`tastytrade_status`] to report on.
//!
//! The pickers are `async fn`, for the reason [`crate::picker`]'s header
//! gives in full: a plain `#[tauri::command]` on a sync fn runs *inline on
//! the thread that delivered the IPC request*, which on Linux is the GTK main
//! thread, so every wait in here — the chooser, the confirmation, the rebuild
//! — would be a wait for work only that thread can do, and the application
//! would wedge permanently. Waiting as a task rather than as a thread costs
//! nothing, and is what lets a picker wait for the rebuild its write triggers
//! and answer with whether the running application actually took the file
//! ([`PickedPath`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use wingspan_app::config::{Config, Layers};
use wingspan_app::diagnostics::Grade;
use wingspan_app::error::{ApiError, ErrorKind, ProblemJson};
use wingspan_app::settings::{self, SettingsPatch};
use wingspan_app::state::{AppOptions, AppState, SavedRootPolicy};
use wingspan_app::{dto, service};

use crate::commands::{off_runtime_infallible, spawned, spawned_blocking, Answer};
use crate::runtime::{AppSlot, Runtime};

/// Where the GitHub releases are. `check_for_updates` opens this and nothing
/// else — there is no updater plugin in this application, on purpose.
pub const RELEASES_URL: &str = "https://github.com/jbolinge/wingspan/releases";

// ---------------------------------------------------------------- settings

/// `settings_read`'s body — the SPA's `SettingsRead`.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsRead {
    /// The file the settings were read from, or `None` when none exists yet:
    /// the app is running on built-in defaults and Save will create it.
    pub file: Option<String>,
    /// Built from the resolved [`Config`], so every field is `Some` — the
    /// form shows what the app is *actually* using, not what one layer said.
    pub settings: SettingsPatch,
    pub paths: SettingsPaths,
    /// `None` when no Schwab provider is configured at all. Schwab's, and only
    /// Schwab's — the name is frozen in `docs/IPC.md`; tastytrade reports
    /// through [`SettingsRead::tastytrade_status`].
    pub token_status: Option<TokenStatus>,
    /// `None` when tastytrade is disabled or unconfigured. Never a credential.
    pub tastytrade_status: Option<TastytradeAuthStatus>,
}

/// The read-only paths the settings card displays. None of them is writable
/// from JS; the four path-shaped settings (`[yfinance] python`,
/// `[schwab] token_path`, `[schwab] env_file`, `[tastytrade] env_file`) are
/// changed by the pickers.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsPaths {
    pub trades_root: Option<String>,
    pub token_path: Option<String>,
    /// Schwab's — the name is frozen.
    pub env_file: Option<String>,
    pub python: Option<String>,
    pub tastytrade_env_file: Option<String>,
}

/// What the app can say about the token file **without touching it** beyond
/// the read it already performs.
#[derive(Debug, Clone, Serialize)]
pub struct TokenStatus {
    pub present: bool,
    pub world_readable: bool,
    pub detail: String,
}

/// What the app can say about the tastytrade grant, and nothing more.
///
/// Read-only, and never a credential: whether a grant is configured at all,
/// which environment it points at, whether an access token is in hand, and the
/// sentence the last exchange produced. There is deliberately no variable
/// *value* here, not even a masked one.
#[derive(Debug, Clone, Serialize)]
pub struct TastytradeAuthStatus {
    pub configured: bool,
    pub sandbox: bool,
    pub delayed: bool,
    pub authorized: bool,
    pub detail: String,
}

/// `settings_update`'s body: the file the patch landed in.
#[derive(Debug, Clone, Serialize)]
pub struct SettingsWritten {
    pub file: String,
}

#[tauri::command]
pub async fn settings_read(slot: State<'_, AppSlot>) -> Answer<SettingsRead> {
    let rt = slot.ready().await?;
    spawned(async move { Ok(read_settings(&rt.state).await) }).await
}

/// The whole of `settings_read`, as a plain function of a booted app.
async fn read_settings(state: &AppState) -> SettingsRead {
    let config = &state.config;
    let path = settings::settings_path();
    SettingsRead {
        file: path.is_file().then(|| path.display().to_string()),
        settings: patch_from(config),
        paths: SettingsPaths {
            trades_root: state.root().dir().map(display),
            // `resolve_token_path` reads the external `token_config` TOML when
            // there is one; an unreadable one is a diagnostics row, not a
            // reason for the settings card to fail to render.
            token_path: config
                .schwab
                .resolve_token_path()
                .ok()
                .flatten()
                .map(|p| display(&p)),
            env_file: config.schwab.resolved_env_file().map(|p| display(&p)),
            python: Some(display(&config.yfinance.resolved_python())),
            tastytrade_env_file: config.tastytrade.resolved_env_file().map(|p| display(&p)),
        },
        token_status: token_status(state).await,
        tastytrade_status: tastytrade_status(state),
    }
}

fn display(path: &Path) -> String {
    path.display().to_string()
}

/// The resolved config as a fully populated patch.
///
/// Every field is `Some` on purpose: the form is showing what the running
/// application uses, and a `None` would render an empty input over a value
/// that is very much in force.
fn patch_from(config: &Config) -> SettingsPatch {
    SettingsPatch {
        trades: Some(settings::TradesPatch {
            open_dir: Some(config.trades.open_dir.clone()),
            closed_dir: Some(config.trades.closed_dir.clone()),
            statements_dir: Some(config.trades.statements_dir.clone()),
            statement_timezone: Some(config.trades.statement_timezone.clone()),
            watch_debounce_ms: Some(config.trades.watch_debounce_ms),
            watch_poll_secs: Some(config.trades.watch_poll_secs),
        }),
        schwab: Some(settings::SchwabPatch {
            token_config_key: Some(config.schwab.token_config_key.clone()),
            app_key_env: Some(config.schwab.app_key_env.clone()),
            app_secret_env: Some(config.schwab.app_secret_env.clone()),
            refresh: Some(config.schwab.refresh),
        }),
        tastytrade: Some(settings::TastytradePatch {
            enabled: Some(config.tastytrade.enabled),
            client_id_env: Some(config.tastytrade.client_id_env.clone()),
            client_secret_env: Some(config.tastytrade.client_secret_env.clone()),
            refresh_token_env: Some(config.tastytrade.refresh_token_env.clone()),
            sandbox: Some(config.tastytrade.sandbox),
            delayed: Some(config.tastytrade.delayed),
            api_version: Some(config.tastytrade.api_version.clone()),
        }),
        yfinance: Some(settings::YFinancePatch {
            enabled: Some(config.yfinance.enabled),
        }),
        market: Some(settings::MarketPatch {
            spot_sources: Some(config.market.spot_sources.clone()),
            option_sources: Some(config.market.option_sources.clone()),
            refresh_secs_open: Some(config.market.refresh_secs_open),
            refresh_secs_closed: Some(config.market.refresh_secs_closed),
            pause_when_hidden: Some(config.market.pause_when_hidden),
        }),
        pricing: Some(settings::PricingPatch {
            risk_free_rate: Some(config.pricing.risk_free_rate),
            dividend_yield: Some(config.pricing.dividend_yield),
            weighted_vega_reference: Some(vega_reference(config)),
            weighted_vega_exponent: Some(config.pricing.weighted_vega.exponent),
        }),
        risk_graph: Some(settings::RiskGraphPatch {
            points: Some(config.risk_graph.points),
            range_pct: Some(config.risk_graph.range_pct),
            tplus_preset: Some(config.risk_graph.tplus_preset.clone()),
        }),
    }
}

/// `[pricing.weighted_vega] reference` in the one spelling
/// `settings::apply` parses back — `"front_expiry"` or `"<n>d"`.
///
/// `VegaReference`'s own `Display`, which is also what its `Serialize` writes
/// and what its `FromStr` reads, so the value this hands the form round-trips
/// through `settings_update` unchanged.
fn vega_reference(config: &Config) -> String {
    config.pricing.weighted_vega.reference.to_string()
}

/// Read-only, and never a token: only whether the file is there, whether its
/// mode is wider than its owner, and the sentence the last read produced.
async fn token_status(state: &AppState) -> Option<TokenStatus> {
    let client = state.schwab.as_ref()?;
    let status = client.probe().await;
    let path = status.token_path.display();
    let detail = if !status.token_loaded {
        format!(
            "{path} — {}",
            status
                .last_error
                .clone()
                .unwrap_or_else(|| "could not be read".to_string())
        )
    } else if status.token_world_readable {
        format!(
            "{path} is readable beyond its owner — tighten it where it is produced \
             (Wingspan only ever reads it)"
        )
    } else {
        format!("{path} — read, and readable only by you")
    };
    Some(TokenStatus {
        present: status.token_loaded,
        world_readable: status.token_world_readable,
        detail,
    })
}

/// Read-only, and never a token: what the running client already knows.
///
/// Synchronous — no `await`, no I/O, and above all **no token exchange**.
/// `Tastytrade` deliberately has no `probe()`: the only thing this provider
/// could probe is the token exchange, and tastytrade blocks an IP for hours
/// after repeated failed logins, so a card that re-authenticated every time it
/// rendered would be the fastest way into that block. The client's own
/// cooldown is what it reports here instead.
fn tastytrade_status(state: &AppState) -> Option<TastytradeAuthStatus> {
    let status = state.tastytrade.as_ref()?.status();
    // Through the injected clock, and through `auth_cooldown_active` rather
    // than the raw field: the deadline outlives the hold, and the diagnostics
    // row has to agree with this one.
    let detail = if let Some(until) = status.auth_cooldown_active(state.now()) {
        format!(
            "not retrying the token exchange until {} — repeated failed logins are blocked \
             for hours",
            until.format("%H:%M:%SZ")
        )
    } else if let Some(err) = &status.last_error {
        err.clone()
    } else if status.authorized {
        if status.sandbox {
            "authorized against api.cert.tastyworks.com (sandbox — no market data there)"
                .to_string()
        } else {
            "authorized — an access token is held in memory and refreshed as it expires".to_string()
        }
    } else {
        "no access token yet — one is fetched on the first quote".to_string()
    };
    Some(TastytradeAuthStatus {
        configured: true,
        sandbox: status.sandbox,
        delayed: status.delayed,
        authorized: status.authorized,
        detail,
    })
}

/// `{ request: SettingsPatch }` — validate, write, then rebuild the whole
/// application on the file that was just written.
///
/// The patch is validated *before* the file is touched
/// (`settings::write_patch` applies it to the running config and calls
/// `Config::validate`), so a risk-free rate of `NaN` is a sentence and an
/// unchanged `wingspan.toml`, never a file the next launch refuses to parse.
#[tauri::command]
pub async fn settings_update<R: tauri::Runtime>(
    app: AppHandle<R>,
    slot: State<'_, AppSlot>,
    request: SettingsPatch,
) -> Answer<SettingsWritten> {
    let rt = slot.ready().await?;
    let slot = slot.inner().clone();
    spawned(async move {
        let current = Arc::clone(&rt.state.config);
        let patch = request;
        let file = spawned_blocking(move || {
            settings::write_patch(&current, &patch).map_err(|reason| unprocessable(&reason))
        })
        .await?;
        // Re-read from disk rather than trusting the in-memory apply: the
        // file is what the next launch will read, and `WINGSPAN_*` still
        // overrides it.
        //
        // The write has already landed by here — a patch that validates but
        // will not *build* (a `[market] spot_sources` naming a provider that
        // cannot be constructed, say) leaves the file changed and the old
        // application running. `boot::rebuild`'s own sentence says only that
        // the change could not be applied, which would leave the user with a
        // file they cannot see and a session that disagrees with it, so this
        // one says where the change went and when it will take effect.
        if let Err(err) = reload_and_rebuild(&app, &slot, &rt).await {
            return Err(ProblemJson::internal(&format!(
                "your settings were written to {}, but this session could not be rebuilt on \
                 them, so the previous settings are still in force and the file takes effect \
                 the next time Wingspan starts: {}",
                file.display(),
                err.detail
            )));
        }
        Ok(SettingsWritten {
            file: display(&file),
        })
    })
    .await
}

/// `config::load` + [`crate::boot::rebuild`] — what every writer of
/// `wingspan.toml` does once its write has landed.
async fn reload_and_rebuild<R: tauri::Runtime>(
    app: &AppHandle<R>,
    slot: &AppSlot,
    rt: &Runtime,
) -> Result<(), ProblemJson> {
    // `user_only`, exactly as `main` boots: an app launched from a `.desktop`
    // entry has a working directory nobody chose.
    let loaded = tauri::async_runtime::spawn_blocking(|| {
        wingspan_app::config::load(None, Layers::user_only())
    })
    .await
    .map_err(|err| ProblemJson::internal(&format!("cancelled: {err}")))?
    .map_err(|err| unprocessable(&format!("the settings file could not be re-read: {err}")))?;

    let options = AppOptions {
        // The credentials travel as values; a re-read `[schwab]` or
        // `[tastytrade]` `env_file` is the whole reason this is not
        // `rt.state.options.secrets`.
        secrets: loaded.env_values.clone(),
        ..rt.state.options.clone()
    };
    crate::boot::rebuild(app, slot, loaded, options).await?;
    Ok(())
}

fn unprocessable(detail: &str) -> ProblemJson {
    ApiError::new(ErrorKind::Unprocessable, "settings", detail).problem()
}

// ----------------------------------------------------------------- pickers

/// Which of the four path settings a picker writes.
#[derive(Debug, Clone, Copy)]
enum PathSetting {
    Python,
    EnvFile,
    TokenFile,
    TastytradeEnvFile,
}

impl PathSetting {
    /// The dialog title.
    fn title(self) -> &'static str {
        match self {
            PathSetting::Python => "Choose a Python interpreter",
            PathSetting::EnvFile => "Choose your Schwab credentials file",
            PathSetting::TokenFile => "Choose your Schwab token file",
            PathSetting::TastytradeEnvFile => "Choose your tastytrade credentials file",
        }
    }

    /// The sentence the confirmation shows, naming the full path **and** what
    /// the application will do with it. This is the whole security control:
    /// the user is told, in the operating system's own dialog, that a program
    /// is about to be run or a secret about to be read.
    fn confirmation(self, path: &Path) -> String {
        let path = path.display();
        match self {
            PathSetting::Python => {
                format!("Wingspan will run this interpreter for delayed quotes: {path}. Continue?")
            }
            PathSetting::EnvFile => {
                format!("Wingspan will read your Schwab credentials from: {path}. Continue?")
            }
            PathSetting::TokenFile => format!(
                "Wingspan will read (never write) your Schwab token from: {path}. Continue?"
            ),
            PathSetting::TastytradeEnvFile => format!(
                "Wingspan will read your tastytrade client secret and refresh token from: \
                 {path}. Continue?"
            ),
        }
    }

    /// Where the chooser opens.
    fn start_dir(self, config: &Config) -> Option<PathBuf> {
        let current = match self {
            PathSetting::Python => Some(config.yfinance.resolved_python()),
            PathSetting::EnvFile => config.schwab.resolved_env_file(),
            PathSetting::TokenFile => config.schwab.resolve_token_path().ok().flatten(),
            PathSetting::TastytradeEnvFile => config.tastytrade.resolved_env_file(),
        };
        let parent = current
            .as_deref()
            .and_then(Path::parent)
            .filter(|dir| dir.is_dir())
            .map(Path::to_path_buf);
        parent.or_else(|| match self {
            // The venv `DESKTOP.md` tells the user to build, if it is there.
            PathSetting::Python => {
                let venv = dirs::data_dir()?.join("wingspan").join("venv").join("bin");
                venv.is_dir().then_some(venv)
            }
            PathSetting::EnvFile | PathSetting::TokenFile | PathSetting::TastytradeEnvFile => {
                dirs::home_dir()
            }
        })
    }

    /// The `wingspan_app::settings` writer for this field. `TokenFile`,
    /// `EnvFile` and `TastytradeEnvFile` write the **path string** and nothing
    /// else — the file itself is never opened here.
    fn persist(self, path: &Path) -> Result<PathBuf, String> {
        match self {
            PathSetting::Python => settings::write_python(path),
            PathSetting::EnvFile => settings::write_env_file(path),
            PathSetting::TokenFile => settings::write_token_path(path),
            PathSetting::TastytradeEnvFile => settings::write_tastytrade_env_file(path),
        }
    }
}

/// Ask for a file, confirm it by name, persist it, then rebuild.
///
/// `async` — see the module docs and [`crate::picker`]: a sync command runs
/// inline on the GTK main thread, where waiting for a dialog is waiting for
/// the very thread that has to build it. `Ok(None)` is a cancel at either
/// step, not an error.
async fn pick_path<R: tauri::Runtime>(
    app: AppHandle<R>,
    which: PathSetting,
) -> Answer<Option<PickedPath>> {
    let slot = app.state::<AppSlot>().inner().clone();
    // `peek` rather than `ready`: the chooser opens now rather than pending
    // on boot, because a boot that is failing on one of these very paths is
    // when a picker is most needed. Before boot finishes there is no config
    // to seed the chooser from, which costs a starting directory and nothing
    // else.
    let config = slot.peek().map_or_else(
        || Arc::new(Config::default()),
        |rt| Arc::clone(&rt.state.config),
    );

    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title(which.title());
    if let Some(start) = which.start_dir(&config) {
        dialog = dialog.set_directory(start);
    }
    dialog.pick_file(move |picked| {
        // The receiver is only gone if this command was abandoned.
        let _ = tx.send(picked);
    });
    let Some(picked) = rx
        .await
        .map_err(|_| ProblemJson::internal("the file chooser closed without answering"))?
    else {
        return Ok(None);
    };
    let Ok(path) = picked.into_path() else {
        return Ok(None);
    };

    // The plugin's dialog with a callback, not `rfd`'s blocking `show()`:
    // the same GTK dialog with the same Ok/Cancel, but marshalled onto the
    // main thread and answered on a channel, so nothing waits on a thread.
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .message(which.confirmation(&path))
        .title("Wingspan")
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancel)
        .show(move |confirmed| {
            let _ = tx.send(confirmed);
        });
    let confirmed = rx
        .await
        .map_err(|_| ProblemJson::internal("the confirmation closed without answering"))?;
    if !confirmed {
        return Ok(None);
    }

    let written = which
        .persist(&path)
        .map_err(|reason| unprocessable(&reason))?;
    tracing::info!("settings: wrote a path setting to {}", written.display());

    // **Wait for the rebuild, and say how it went.** Answering `Ok` while the
    // rebuild fails in a detached task is how a credentials file this process
    // cannot read ends up permanently in `wingspan.toml` — `boot::rebuild`
    // puts the *old* runtime back, so `settings_read` re-reads the old value,
    // the card redraws with the old path, and the only trace of the failure
    // is a line in a log file the user has no reason to open. The dialog is
    // already closed by now, every other command pends through the swap
    // anyway, and this is a task, not a thread: awaiting it costs nothing.
    let handle = app.clone();
    let rebuild = tauri::async_runtime::spawn(async move {
        match slot.ready().await {
            Ok(rt) => reload_and_rebuild(&handle, &slot, &rt).await,
            Err(err) => Err(err),
        }
    });
    let applied = match tokio::time::timeout(APPLY_TIMEOUT, rebuild).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(err))) => Err(err.detail),
        // The rebuild task panicked: the same sentence a dropped responder
        // gets in `commands::joined`, for the same reason.
        Ok(Err(_)) => Err("the request did not finish".to_string()),
        // Still running. Dropping the handle detaches the task, it does not
        // cancel it: it will finish, or fail into the log, long after this.
        // Say so rather than claim either outcome.
        Err(_) => Err(format!(
            "it is taking longer than {} s to apply — if the window does not \
             change in a moment, restart Wingspan",
            APPLY_TIMEOUT.as_secs()
        )),
    };
    if let Err(detail) = &applied {
        tracing::error!(
            "the new setting was saved to {} but not applied: {detail}",
            written.display()
        );
    }
    Ok(Some(PickedPath {
        path: display(&path),
        applied: applied.is_ok(),
        detail: applied.err(),
    }))
}

/// How long a picker waits for the application to come back before it answers
/// "saved, but not applied yet". Longer than the ~40 s a cold Schwab launch
/// can take, so the ordinary slow case is still reported as applied.
const APPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// A picker's answer: the file that was chosen, and whether the running
/// application actually took it.
#[derive(Debug, Clone, Serialize)]
pub struct PickedPath {
    pub path: String,
    /// `false` when `wingspan.toml` was written but the rebuild refused the
    /// result: the previous settings are still in force in this session, and
    /// the file is what the next launch will read.
    pub applied: bool,
    /// The sentence to show when `applied` is false; `None` when it is true.
    pub detail: Option<String>,
}

// `AppHandle` by value is the only shape Tauri accepts for a command
// argument; it is a cheap clone of a handle, not the application. Each is
// `async fn` and goes through `spawned` for the two reasons the module docs
// give: off the GTK main thread, and behind a `JoinHandle` so a panic is a
// 500 rather than a promise that never settles.
#[tauri::command]
pub async fn pick_python<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<Option<PickedPath>> {
    spawned(pick_path(app, PathSetting::Python)).await
}

#[tauri::command]
pub async fn pick_env_file<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<Option<PickedPath>> {
    spawned(pick_path(app, PathSetting::EnvFile)).await
}

#[tauri::command]
pub async fn pick_token_file<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<Option<PickedPath>> {
    spawned(pick_path(app, PathSetting::TokenFile)).await
}

#[tauri::command]
pub async fn pick_tastytrade_env_file<R: tauri::Runtime>(
    app: AppHandle<R>,
) -> Answer<Option<PickedPath>> {
    spawned(pick_path(app, PathSetting::TastytradeEnvFile)).await
}

// -------------------------------------------------------------- diagnostics

/// `probe_python`'s body.
#[derive(Debug, Clone, Serialize)]
pub struct PythonProbe {
    pub ok: bool,
    pub detail: String,
}

/// Run the checklist's own yfinance row against the configured interpreter.
///
/// It runs `diagnostics` and reads the row back rather than shelling out a
/// second time, so there is exactly one probe in the codebase and the Test
/// button can never disagree with the Diagnostics card beside it.
///
/// **Warn, never fail.** A machine with no `yfinance` (or no Python at all)
/// is a supported configuration — the provider chain degrades to
/// schwab → cache → statement — so a missing interpreter answers
/// `{ ok: false, detail }` and never an `Err`.
#[tauri::command]
pub async fn probe_python(slot: State<'_, AppSlot>) -> Answer<PythonProbe> {
    let rt = slot.ready().await?;
    spawned(async move {
        // `true`: the checklist only spawns the interpreter when it is asked
        // to, and this button *is* the asking. `diagnostics(_, false)` would
        // answer "not checked; press Run checks", which is the one sentence
        // the Test button must never return.
        let rows = wingspan_app::diagnostics::diagnostics(&rt.loaded, true).await;
        let row = rows
            .iter()
            .find(|check| check.section == "yfinance bridge" && check.name == "yfinance");
        Ok(row.map_or_else(
            || PythonProbe {
                ok: false,
                detail: "the yfinance check did not run".to_string(),
            },
            |row| PythonProbe {
                ok: row.grade == Grade::Ok,
                detail: row.detail.clone(),
            },
        ))
    })
    .await
}

/// `{ request: { probe } }` — `probe` re-runs the live checks.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiagnosticsRequest {
    #[serde(default)]
    pub probe: bool,
}

/// One checklist row. `grade` is the lowercase string the SPA switches on.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsRow {
    pub grade: &'static str,
    pub section: &'static str,
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsResponse {
    pub rows: Vec<DiagnosticsRow>,
}

/// `Grade` as the SPA's four strings. Not a `Serialize` impl on `Grade`
/// itself, because that type belongs to `wingspan-app` and this spelling is
/// this transport's contract.
fn grade(grade: Grade) -> &'static str {
    match grade {
        Grade::Ok => "ok",
        Grade::Warn => "warn",
        Grade::Fail => "fail",
        Grade::Note => "note",
    }
}

/// The same checklist `wingspan-cli doctor` prints, as data.
///
/// One implementation, two renderers: this calls
/// `wingspan_app::diagnostics::diagnostics` on the very `Loaded` the running
/// application was built from, so a row can never differ between the terminal
/// and the window. `tests/commands.rs` asserts that row for row.
#[tauri::command]
pub async fn diagnostics(
    slot: State<'_, AppSlot>,
    request: DiagnosticsRequest,
) -> Answer<DiagnosticsResponse> {
    let rt = slot.ready().await?;
    spawned(async move {
        let rows = wingspan_app::diagnostics::diagnostics(&rt.loaded, request.probe).await;
        Ok(DiagnosticsResponse {
            rows: rows
                .into_iter()
                .map(|check| DiagnosticsRow {
                    grade: grade(check.grade),
                    section: check.section,
                    name: check.name,
                    detail: check.detail,
                })
                .collect(),
        })
    })
    .await
}

// --------------------------------------------------------------- demo book

/// Show the bundled synthetic book.
///
/// **R14, and the reason `SavedRootPolicy` exists.** `AppState::build` reads
/// `trades_root.json` *before* it looks at `root_override`, so without
/// [`SavedRootPolicy::Ignore`] a user who has chosen a folder and clicks
/// "Load demo book" would get their **own** positions priced off the
/// synthetic fixture provider, measured from a fixture day, and reported as
/// `source: "local"` — fabricated marks on a real book, from a menu item.
/// Nothing is persisted: `trades_root.json` is not read, written or removed,
/// so [`leave_demo`] puts the real folder straight back.
#[tauri::command]
pub async fn load_demo<R: tauri::Runtime>(
    app: AppHandle<R>,
    slot: State<'_, AppSlot>,
) -> Answer<dto::TradesRootDto> {
    let rt = slot.ready().await?;
    let slot = slot.inner().clone();
    spawned(async move {
        let root = crate::paths::demo_root(&app).ok_or_else(|| {
            unprocessable(
                "the demo book is not installed with this copy of Wingspan (fixtures/demo)",
            )
        })?;
        let valuation_date = wingspan_app::demo::valuation_date();
        let options = AppOptions {
            demo: true,
            root_override: Some(root),
            saved_root: SavedRootPolicy::Ignore,
            valuation_date: Some(valuation_date),
            // Both together, always: a date without its clock renders a leg
            // that is live by the date and worthless by the clock.
            clock: wingspan_app::demo::clock_at(valuation_date),
            ..rt.state.options.clone()
        };
        let next = crate::boot::rebuild(&app, &slot, rt.loaded.clone(), options).await?;
        Ok(service::trades_root(&next.state))
    })
    .await
}

/// Put the real book back: the saved folder is honoured again, the clock is
/// the system clock, and the credentials are the ones the process booted with.
#[tauri::command]
pub async fn leave_demo<R: tauri::Runtime>(
    app: AppHandle<R>,
    slot: State<'_, AppSlot>,
) -> Answer<dto::TradesRootDto> {
    let rt = slot.ready().await?;
    let slot = slot.inner().clone();
    spawned(async move {
        // The base is what `main` would boot with *now* — a pure function of
        // `WINGSPAN_AS_OF`, so a session launched pinned comes back pinned.
        // `AppOptions::default()` here would silently swap the pinned clock
        // for the system one: every DIT/DTE, every expiry test and the
        // surface's pricing clock would move, with nothing on screen saying
        // why. Unpinned, it *is* the system clock, which is the other half of
        // what leaving the demo means.
        let options = AppOptions {
            demo: false,
            root_override: None,
            saved_root: SavedRootPolicy::Honour,
            ..crate::boot::app_options(rt.loaded.env_values.clone())
        };
        let next = crate::boot::rebuild(&app, &slot, rt.loaded.clone(), options).await?;
        Ok(service::trades_root(&next.state))
    })
    .await
}

// ----------------------------------------------------------------- openers

/// `{ request: { path } }` — one of three paths, and no other.
#[derive(Debug, Clone, Deserialize)]
pub struct RevealRequest {
    pub path: String,
}

/// Open a folder in the desktop's file manager.
///
/// The allow-list is the point. `tauri_plugin_opener` is used through its
/// **Rust** API and has no JS permission, so this command is the only way
/// into it — and it answers for exactly three paths the application already
/// knows: the trades root in force, the log directory, and the directory
/// holding `wingspan.toml`. Anything else is refused, so an XSS in the
/// webview cannot turn it into a general "open anything" primitive. The
/// comparison is canonical, so `…/Trades/../Trades` and a symlinked home
/// are the same folder rather than a bypass.
#[tauri::command]
pub async fn reveal_folder<R: tauri::Runtime>(
    app: AppHandle<R>,
    slot: State<'_, AppSlot>,
    request: RevealRequest,
) -> Answer<()> {
    let rt = slot.ready().await?;
    off_runtime_infallible(move || {
        let asked = PathBuf::from(&request.path);
        if !revealable(&rt.state)
            .iter()
            .any(|allowed| same(allowed, &asked))
        {
            return Err(unprocessable(&format!(
                "{} is not a folder Wingspan can open from here",
                asked.display()
            )));
        }
        open_path(&app, &asked)
    })
    .await?
}

/// The three paths [`reveal_folder`] will open.
fn revealable(state: &AppState) -> Vec<PathBuf> {
    let mut allowed = Vec::with_capacity(3);
    if let Some(root) = state.root().dir() {
        allowed.push(root.to_path_buf());
    }
    allowed.push(crate::paths::log_dir());
    if let Some(dir) = settings::settings_path().parent() {
        allowed.push(dir.to_path_buf());
    }
    allowed
}

/// Same folder, canonically — a path that does not exist can still match
/// itself, so a log directory that has not been created yet is not a hole.
fn same(a: &Path, b: &Path) -> bool {
    a == b
        || match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
}

#[tauri::command]
pub async fn open_log_folder<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<()> {
    off_runtime_infallible(move || open_path(&app, &crate::paths::log_dir())).await?
}

/// Open `wingspan.toml` itself, or the directory it would live in when there
/// is no file yet — an editor cannot open a file that does not exist.
#[tauri::command]
pub async fn open_config_file<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<()> {
    off_runtime_infallible(move || {
        let file = settings::settings_path();
        let target = if file.is_file() {
            file.clone()
        } else {
            file.parent().unwrap_or(&file).to_path_buf()
        };
        open_path(&app, &target)
    })
    .await?
}

#[tauri::command]
pub async fn check_for_updates<R: tauri::Runtime>(app: AppHandle<R>) -> Answer<()> {
    off_runtime_infallible(move || {
        opener(&app)?
            .open_url(RELEASES_URL, None::<&str>)
            .map_err(|err| ProblemJson::internal(&format!("could not open {RELEASES_URL}: {err}")))
    })
    .await?
}

fn open_path<R: tauri::Runtime>(app: &AppHandle<R>, path: &Path) -> Answer<()> {
    opener(app)?
        .open_path(path.display().to_string(), None::<&str>)
        .map_err(|err| ProblemJson::internal(&format!("could not open {}: {err}", path.display())))
}

/// The opener plugin, or a sentence.
///
/// `OpenerExt::opener` **panics** when the plugin was not initialised, and a
/// panic inside a command is the one failure the SPA cannot render usefully.
/// `main` does initialise it; the command surface `tests/commands.rs` drives
/// deliberately does not (it registers no plugin at all), and a build that
/// somehow dropped the line should say so rather than abort.
fn opener<R: tauri::Runtime>(
    app: &AppHandle<R>,
) -> Result<&tauri_plugin_opener::Opener<R>, ProblemJson> {
    app.try_state::<tauri_plugin_opener::Opener<R>>()
        .map(|state| state.inner())
        .ok_or_else(|| ProblemJson::internal("this build of Wingspan cannot open files or folders"))
}
