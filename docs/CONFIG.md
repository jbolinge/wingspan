# Configuration

Config is TOML + environment, merged by figment. Two different resolution
ladders apply, depending on which binary is reading it:

**The desktop app** (the shipped product) reads only:

1. `~/.config/wingspan/wingspan.toml`
2. `WINGSPAN_*` environment variables

It never reads `./wingspan.toml`, `./.env` or `$WINGSPAN_CONFIG`.

**`wingspan-cli` and `wingspan-devserver`** (dev-only) honour the fuller
ladder, precedence highest last:

1. `~/.config/wingspan/wingspan.toml`
2. `./wingspan.toml`
3. `$WINGSPAN_CONFIG` (path)
4. `--config <path>`
5. `WINGSPAN_*` environment variables (e.g. `WINGSPAN_TRADES__OPEN_DIR`)

Start from `wingspan.example.toml` (kept exhaustive and in sync with the
code; run `wingspan-cli doctor` after editing).

## [trades]
| key | default | meaning |
|---|---|---|
| `root` | — (optional) | folder holding the three subfolders; discovery descends one level. Usually unset: the folder you choose in the app outranks it — see "Which book is loaded" |
| `open_dir` | `"Open Trades"` | subfolder name, case-insensitive |
| `closed_dir` | `"Closed Trades"` | |
| `statements_dir` | `"Daily Trade Statement"` | |
| `statement_timezone` | `"America/Chicago"` | statement times are Central |
| `watch_debounce_ms` | `750` | quiet period before a burst of file events becomes one reload (floor 50 ms) |
| `watch_poll_secs` | `0` | `0` = the platform notifier; `n > 0` polls the folder every `n` seconds, for network/sync mounts (Dropbox, Syncthing, SMB, NFS) where inotify never sees a remote write |

## [schwab]
`token_path`, `app_key_env` (default `SCHWAB_APP_KEY`), `app_secret_env`
(default `SCHWAB_APP_SECRET`), `refresh` (default `true`) — see
`docs/SCHWAB.md`.

`token_path` names an **existing** token file that other tooling produces and
refreshes. Wingspan opens it read-only and never writes, creates, renames,
chmods, locks, copies or backs it up; a refreshed access token is kept only in
memory. `refresh` decides whether wingspan may run the refresh grant itself
(`false` leaves that to the other applications and limits wingspan to
re-reading the file) — neither value ever writes the file.

Production indirection (when other tooling owns the secrets):

| key | default | meaning |
|---|---|---|
| `env_file` | unset | external `KEY=VALUE` file holding the app key/secret; loaded at startup, existing env always wins |
| `token_config` | unset | external TOML file that points at the token JSON |
| `token_config_key` | `"paths.token_file"` | dotted key inside `token_config` holding the path |

An explicit `token_path` wins over `token_config`. Errors resolving the
indirection (missing file, missing key) degrade the Schwab provider with the
reason in the status pill — and `wingspan doctor` reports them.

## .env

Read by `wingspan-cli` and `wingspan-devserver` only, never by the desktop
app. A `./.env` file (KEY=VALUE, `#` comments, optional `export`, quotes) is
loaded before the config is read, so it may carry any `WINGSPAN_*` override —
including the two pointers above. A variable already set in the real
environment always wins. Start from `.env.example`.

## [yfinance]
`enabled` (default `true`), `python` (path to the venv python with yfinance
installed; default: `python3` on PATH).

The bridge script itself (`bridge/yfinance_bridge.py`) is compiled into the
`wingspan` binary and self-installs a copy into `[market] state_dir` the
first time it runs — a single-binary deploy never needs the `bridge/`
directory on the target machine, only a Python interpreter with `yfinance`
installed (`docs/DEPLOY.md`). Set `script` to point at a different bridge
script (e.g. while hacking on it); `wingspan doctor` reports which
interpreter and version it found.

## [market]
| key | default |
|---|---|
| `spot_sources` | `["schwab", "yfinance", "cache"]` |
| `option_sources` | `["schwab", "yfinance", "cache", "statement"]` |
| `refresh_secs_open` / `refresh_secs_closed` | `15` / `300` |
| `state_dir` | `~/.local/share/com.jbolinge.wingspan/state` |

## [pricing]
`risk_free_rate`, `dividend_yield` (fallbacks when forward calibration has no
ATM pair), and `weighted_vega { reference = "front_expiry" | "<n>d",
exponent = 0.5 }` — the reference in use is always shown next to the column.

## [risk_graph]
`points` (241), `range_pct` (0.10), `tplus_preset` (`"quarters"` =
T+0, T+¼·DTE, T+½·DTE, T+¾·DTE, T+(DTE−1), plus the front expiry).

## The valuation date: `--as-of` and `--demo`

Nothing in `wingspan.toml` sets the day the book is valued on — it is today,
unless a flag moves it.

| flag | effect |
|---|---|
| `serve --as-of YYYY-MM-DD` | value the book on that date instead of today |
| `validate --as-of YYYY-MM-DD` | the same, for the one-shot report |
| `serve --demo` | pins to the demo book's own day, `2026-08-31` |
| `serve --root DIR` | serve `DIR` instead of `[trades] root` (with `--demo`: instead of the bundled book) |

`--as-of` moves **two things together**, and that is the point of having one
flag for it: the *valuation date* that DIT, DTE and "has this leg expired" are
measured against, and the *pricing clock* (noon America/New_York on that date)
that the vol surface is solved and priced at. Setting one without the other
gives a leg that is live to the first and expired to the second. With a live
provider it also means the quotes that arrive are interpreted against that
day, not today — so `--as-of` is for asking "what did this look like on…",
not for back-dating a live session.

`serve --demo` pins the same pair to `2026-08-31` and prints

```
demo mode: fixtures at /…/fixtures/demo, valued as of 2026-08-31
```

because `fixtures/demo` — the made-up book `scripts/make_demo_book.py` writes:
a 16 OCT 26 strangle, a 20 NOV 26 iron condor and a closed September put
spread — is a dated snapshot: its statements end 2026-08-26 and its nearest
expiry is weeks after that. Valued against a real `now` the demo would silently
rot — DIT and DTE drift, the T+x ladder collapses, and once the calendar passes
an expiry the leg leaves the position altogether. So **a new user following the
Quick Start sees DTEs measured from 2026-08-31, not from their own calendar.**
`serve --demo --as-of 2026-12-01` overrides the pin; `GET /api/v1/config`'s
`resolved.valuation_date` always reports what won.

`--demo --root DIR` keeps the pin and the synthetic prices but serves `DIR`
instead of the bundled book — how the e2e suite runs the anonymised test
fixtures in `fixtures/trades`. Without `--demo`, `--root DIR` simply outranks
`[trades] root` for this process (source `"config"`); like `[trades] root` it
is a deployer's choice and cannot be changed from the app.

## Inspecting the resolved config
`GET /api/v1/config` (and `wingspan doctor`) show this whole structure with
secrets redacted, plus a `resolved` block: the discovered `trades_root`/
`open_dir`/`closed_dir`/`statements_dir`, `state_dir`, today's
`valuation_date`, whether `--demo` is active, and any provider notes (e.g.
"demo mode: prices are synthetic, not market data"). `account_number` never
appears there — it lives only in the Schwab token file, which wingspan reads
and never writes (`docs/SCHWAB.md`).

## Which book is loaded

**You choose a folder on this computer, and it is read where it is.** Nothing
of yours is copied, moved, renamed or deleted — not when you load a folder,
not when you clear it, not ever. `[trades] root` remains a *deployer's*
setting for an install that should always read the same folder (and for
`wingspan validate`, `serve --root` and `serve --demo`); it cannot be changed
at runtime, and the folder you choose in the app outranks it.

Which folder that is gets recorded next to the snapshot cache, never in
`wingspan.toml`:

```jsonc
<state_dir>/trades_root.json
{
  "root": "/home/trader/TOS Exports/Trades",  // canonical, ~ expanded on read
  "kind": "local",                            // the only value today; reserved
  "chosen_at": "2026-09-03T15:04:00Z",
  "recent": ["/home/trader/Archive/2025"]     // cap 5, most recent first
}
```

Precedence for the active root, highest first:

1. `trades_root.json` — the folder you chose in the app
2. the root the process was started with (`serve --demo`'s book, or `--root`)
3. `[trades] root`
4. nothing at all

`GET /api/v1/trades/root` reports which won as `source` (`"local"`, `"demo"`,
`"config"`, `"empty"`). `--demo` chooses where *prices* come from, not which
book is loaded, so a chosen folder outranks it too — the source stops being
`"demo"` as soon as one is picked. ("Load demo book" is the exception, and
deliberately so: it starts a run that does not read `trades_root.json` at all,
because pricing your real positions off synthetic fixtures would be a lie the
app told you from a menu item.)

**Nothing configured and nothing chosen is not an error.** It is the ordinary
state of a fresh install: `source` is `"empty"`, `root` is `null`, the
dashboard is empty, and the desktop app (or `wingspan-devserver serve --demo`
in dev) starts and says so. A folder is
discovered with the same rules as `[trades] root` (case-insensitive names from
`open_dir` / `closed_dir` / `statements_dir`, one level of descent), and
loading one reloads the book, re-solves the statement source, moves the file
watcher and asks for a fresh set of quotes — exactly as a restart would.

A chosen folder that has since been renamed, unmounted or emptied is
**dropped, never fatal**: the app falls back to `[trades] root` (or to
nothing), logs a warning and publishes it in `provider_notes` (so
`GET /api/v1/config` and the UI show it). `wingspan doctor` prints the same
thing on a `chosen folder` line. The record itself is left alone, because the
folder may be back tomorrow. A `trades_root.json` written by an older version
— a bare `{"root": …}` with no timestamp — is **adopted**: choosing a folder
on this computer is exactly what the app does now.

Two folders are refused before discovery even walks them, both with a 422 you
can act on: one inside `<state_dir>` (reading our own cache as a book is
nobody's intention), and one holding a symbolic link whose target leaves it
(so that "read this folder" cannot quietly become "read that one"). A folder
that *is* a link resolves and loads normally — that is how a synced folder
usually looks.

## The trades folder on this computer

This is how a book gets loaded. The Trades control opens a folder chooser, you
pick a folder, and the app shows you what is in it — how many open and closed
trades, how many statements — **before** anything changes. "Load & analyze"
then commits to it.

```
/home/trader/TOS Exports/Trades/Open Trades/…          your files, read in place
/home/trader/TOS Exports/Trades/Closed Trades/…
/home/trader/TOS Exports/Trades/Daily Trade Statement/…
<state_dir>/trades_root.json                           the only thing written
```

Three consequences worth knowing:

* **It is live, not a snapshot.** The folder is watched recursively, so
  saving a trade file, or moving one from `Open Trades` to `Closed Trades`,
  reaches the dashboard on its own after `watch_debounce_ms` of quiet. If the
  watch could not be installed — an unreadable folder, or an exhausted
  `fs.inotify.max_user_watches` — `GET /api/v1/trades/root` reports
  `"watching": false` and the reason lands in `provider_notes`, rather than
  the app pretending a stale book is a live one. On a network or sync mount,
  set `watch_poll_secs`.
* **Clear stops showing the trades. It deletes nothing.**
  `DELETE /api/v1/trades/root` forgets the pointer, drops the cached market
  snapshot (whose quote universe was that book's own contracts) and falls back
  to `[trades] root`, to `--demo`'s book, or to nothing. **No file, directory
  or byte inside your folder is read, moved, renamed or removed** — the whole
  command touches only `<state_dir>`. The folder you cleared goes to the front
  of `recent`, so loading it again is one click. Clearing with nothing loaded
  is a success that changes nothing.
* **Upgrading from the LAN build sets its copy aside.** That build kept an
  uploaded copy of the book in `<state_dir>/uploads`. The first boot after the
  upgrade **renames** it to `<state_dir>/uploads.discarded-<YYYYMMDD>` — never
  deletes it, because it may be the only copy left of a book uploaded from a
  machine you no longer have — forgets the record and says where it went.
  `wingspan doctor` lists it on a `discarded uploads` line, and Diagnostics
  offers an explicit, confirmed delete when you no longer need it. That delete
  is the only recursive delete in the whole application, and it will only ever
  touch a `uploads.discarded-*` directory inside the state directory.

The daily index closes in `closes.json` are unaffected by any of this: they
are SPX levels from the market, not anything read out of your files.
