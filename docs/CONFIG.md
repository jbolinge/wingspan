# Configuration

Config is TOML + environment, merged by figment. Precedence (highest last):

1. `~/.config/wingspan/wingspan.toml`
2. `./wingspan.toml`
3. `$WINGSPAN_CONFIG` (path)
4. `--config <path>`
5. `WINGSPAN_*` environment variables (e.g. `WINGSPAN_SERVER__BIND`)

Start from `wingspan.example.toml` (kept exhaustive and in sync with the
code; run `wingspan doctor` after editing).

## [trades]
| key | default | meaning |
|---|---|---|
| `root` | — (required) | folder holding the three subfolders; discovery descends one level |
| `open_dir` | `"Open Trades"` | subfolder name, case-insensitive |
| `closed_dir` | `"Closed Trades"` | |
| `statements_dir` | `"Daily Trade Statement"` | |
| `statement_timezone` | `"America/Chicago"` | statement times are Central |

## [server]
| key | default | meaning |
|---|---|---|
| `bind` | `"127.0.0.1:8787"` | non-loopback refused unless the two below are set |
| `allow_remote` | `false` | with `auth_token`, permits non-loopback bind |
| `auth_token` | unset | bearer token required on `/api` when remote |
| `allowed_hosts` | `[]` | extra `Host`/`Origin` names accepted on a **loopback** bind |

### `allowed_hosts`

Loopback is not a boundary a browser respects: a page on `evil.example` whose
DNS record is re-pointed at `127.0.0.1` reaches this process same-origin, with
your whole book behind it. What such a page cannot forge is the *name* it asks
for, so a loopback bind answers only to loopback names — `localhost` and any
loopback IP — and refuses everything else with **403 `untrusted-host`** before
a route or even the SPA is reached.

`allowed_hosts` is the list of extra names that are also yours: what a reverse
proxy in front of `127.0.0.1:8787` puts in the header when it forwards its own
`Host`, or an `/etc/hosts` alias you reach the app by.

```toml
[server]
allowed_hosts = ["wingspan.internal", "trades.lan"]
```

A port in an entry is ignored (only the host name is compared, case-
insensitively). Loopback names always work and need no entry, and the key has
**no effect on a non-loopback bind** — there the instance is reached by
whatever name the operator's DNS gives it and the bearer token is the gate, so
there is nothing to allow-list. The same list is what a state-changing request's
`Origin` is checked against (**403 `cross-origin`** otherwise).

## [schwab]
`token_path`, `app_key_env` (default `SCHWAB_APP_KEY`), `app_secret_env`
(default `SCHWAB_APP_SECRET`), `refresh` (default `true`) — see
`docs/SCHWAB.md`.

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

A `./.env` file (KEY=VALUE, `#` comments, optional `export`, quotes) is
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
| `state_dir` | `$XDG_STATE_HOME/wingspan` |

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
`[trades] root` for this process (source `"config"`), like a folder picked in
the status bar does but without being remembered.

## Inspecting the resolved config
`GET /api/v1/config` (and `wingspan doctor`) show this whole structure with
secrets redacted, plus a `resolved` block: the discovered `trades_root`/
`open_dir`/`closed_dir`/`statements_dir`, `state_dir`, today's
`valuation_date`, whether `--demo` is active, and any provider notes (e.g.
"demo mode: prices are synthetic, not market data"). `account_number` never
appears there — it lives only in the Schwab token file (`docs/SCHWAB.md`).

## The trades folder picked in the app

The status bar's **Trades** control changes the folder the book is read from
without touching any config file. `wingspan.toml` is *yours*; the choice is a
runtime preference, so it is stored next to the snapshot cache:

```
<state_dir>/trades_root.json      {"root": "/home/user/Trades"}
```

Precedence for the active root, highest first:

1. `trades_root.json` — the folder picked in the app (`PUT /api/v1/trades/root`)
   or the snapshot uploaded from the browser (`POST /api/v1/trades/upload`)
2. the root the process was started with (`serve --demo`'s book, or `--root`)
3. `[trades] root`

`GET /api/v1/trades/root` reports which won as `source`
(`"override"`, `"uploaded"`, `"demo"`, `"config"`). `--demo` chooses where *prices* come
from, not which book is loaded, so a demo run can still be pointed at a real
folder — the source stops being `"demo"` as soon as one is picked.

There is **no config key for any of this**: the folder is discovered with the
same rules as `[trades] root` (case-insensitive names from `open_dir` /
`closed_dir` / `statements_dir`, one level of descent), and a change reloads
the book, re-solves the statement source and moves the file watcher exactly as
a restart would.

A saved folder that has since been renamed, unmounted or emptied is **dropped,
never fatal**: the server falls back to `[trades] root`, logs a warning and
publishes it in `provider_notes` (so `GET /api/v1/config` and the UI show it).
`wingspan doctor` prints the same thing on a `saved root` line. Delete the file
(or `DELETE /api/v1/trades/root`) to forget the choice; with no configured
root and no demo fixtures behind it, that delete is refused with a 404 rather
than leaving the app with no book.

The picker itself reads `GET /api/v1/fs/dirs`, which lists **directory names
only** — never a file name and never any file's contents. It is registered in
the same `/api/v1` router as every other route, so on a non-loopback bind it
is behind the same `[server] auth_token` bearer; there is nothing extra to
configure or to remember to lock down.

## The trades folder on the viewing computer

When you browse the SPA from a laptop while the server runs elsewhere, your
trade files are on the laptop and the picker above cannot see them. The
Trades dialog's **On this device** mode reads the folder in the browser and
uploads a snapshot (`POST /api/v1/trades/upload`); the server keeps it in the
state directory beside everything else:

```
<state_dir>/uploads/current/      the copy the book is read from
<state_dir>/uploads/.staging-*/   one upload being received (deleted after)
```

`trades_root.json` then points at `uploads/current` and carries the
`uploaded_at` stamp that makes `source` read `"uploaded"`, so the snapshot
survives a restart:

```
<state_dir>/trades_root.json  {"root": "…/uploads/current",
                               "uploaded_at": "2026-08-31T21:04:00Z"}
```

Again there is **no config key**. The fixed limits, which a 413 names when one
is hit, are 400 files, 4 MiB per file and 32 MiB per upload; only files inside
the three layout folders (same case-insensitive names, same one level of
descent) are written at all, and the rest are counted as `skipped` and
dropped. An upload that contains no trades layout is a 422 and changes
nothing.

Two consequences worth knowing:

* **It is a snapshot, not a mount.** The server cannot watch your laptop's
  disk, so edits there show up on the next upload — the dialog offers a
  re-sync, and each one replaces the whole copy rather than merging into it.
* **`DELETE /api/v1/trades/root` deletes the copy.** Dropping the uploaded
  root removes `uploads/current` from the state directory, so telling the app
  to stop using your book also stops it from keeping one.
