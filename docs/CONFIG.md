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
2. the root the process was started with (`serve --demo`'s fixtures)
3. `[trades] root`

`GET /api/v1/trades/root` reports which of the three won as `source`
(`"override"`, `"demo"`, `"config"`). `--demo` chooses where *prices* come
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
