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

## [yfinance]
`enabled` (default `true`), `python` (path to the venv python with yfinance
installed; default: `python3` on PATH).

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
