# Schwab integration

Wingspan uses the Schwab Trader API **market data** endpoints for live SPX
spot and option quotes (bid/ask/mark/IV/greeks/underlyingPrice). Trading
endpoints are never called. Live reconciliation against Accounts & Trading
`positions` is a documented extension point.

## What you need

1. A Schwab developer app (app key + secret) with Market Data product access.
2. An **existing** token file on the machine that runs wingspan, produced by
   your own login tooling — [schwab-py](https://schwab-py.readthedocs.io/)'s
   `client_from_login_flow`, a cron login job, or whatever already keeps your
   other Schwab applications authorised.

Wingspan never creates that file. Interactive login is out of scope for the
app itself, and even once it exists the file stays entirely the other
tooling's: see **Read-only** below.

## Configuration

**In the desktop app the token file arrives via a native picker** — a path
the window itself is never allowed to invent. Settings → Market data → Browse…
opens a file chooser; after you pick a file, a confirmation dialog names the
full path and states plainly what Wingspan is about to do with it. Only on
confirmation is the path string written to `wingspan.toml` — the token file
itself is never opened, copied, chmoded or created by that action. The
result looks like this (a Linux path, under the user's own home):

```toml
[schwab]
token_path = "/home/trader/.tokens/schwab_token.json"  # an existing file, read-only
app_key_env = "SCHWAB_APP_KEY"       # env var NAMES are configurable,
app_secret_env = "SCHWAB_APP_SECRET" # values come from the environment
refresh = true                        # may wingspan run the refresh grant?
```

`refresh = true` lets wingspan run the refresh grant itself and keep the
result **in memory**; `refresh = false` leaves every refresh to the other
applications and limits wingspan to re-reading the file. Neither setting ever
writes it.

The credentials and the token are usually already owned by other applications;
point wingspan at them instead of duplicating anything. In the desktop app,
`env_file` is set the same way as the token path — Settings → Market data →
**Credentials file** → Browse…, its own confirmation dialog, then a write of
the path string. Only the two environment variable *names* (`app_key_env`,
`app_secret_env`) are editable text fields on the settings form; the
*values* the file holds never reach it.

```toml
[schwab]
env_file = "/home/trader/.config/schwab/secrets.env"  # holds SCHWAB_APP_KEY/SECRET
token_config = "/home/trader/.config/schwab/paths.toml"  # [paths] token_file = "…"
token_config_key = "paths.token_file"        # the default
```

`env_file` is loaded at startup with dotenv semantics (the real environment
always wins); `token_config` is read on every reload, so a rotated token
path is picked up without editing wingspan's own config.

## Token file schema (native)

```json
{
  "access_token": "…",
  "refresh_token": "…",
  "token_type": "Bearer",
  "access_expires_at": "2026-08-31T15:12:00Z",
  "refresh_created_at": "2026-08-27T09:00:00Z",
  "account_number": "…"
}
```

A schwab-py-format token file is also accepted (second `TokenStore`
implementation) for portability — including a file that other applications
keep in that shape.

- `account_number` is treated as a **secret**: it masks itself in its own
  `Display`, so it is never logged, and it never appears in `app_config` or
  the Data Health card — it lives only in the token file, which is not part
  of the config body at all.
- Schwab refresh tokens live **7 days**. Health reporting shows the token age
  from `refresh_created_at` and raises a hard warning from day 6. Re-run your
  login flow weekly (a cron reminder is a good idea).

## Read-only

The token file is produced, refreshed and owned by other tooling; wingspan is
given a path and nothing more. It opens that path read-only and **never**
writes, creates, renames, truncates, chmods, locks (no `.lock` sidecar),
copies or backs up anything in the token's directory. There is no code path
that can. The token exists only where your login tooling put it.

Refresh therefore happens entirely in memory:

1. When the access token is inside its expiry slack, or a request comes back
   `401`, wingspan **re-reads the file first** — a sibling application may
   have refreshed it, or someone may have logged in again, seconds ago.
2. If the file now holds an unexpired access token, that one is used and no
   grant is spent. A file that has moved forward (a later `refresh_created_at`,
   or a refresh token the file did not carry when we last read it) always wins
   over what is in memory.
3. Otherwise — and only with `refresh = true` — wingspan runs
   `POST /v1/oauth/token` (refresh grant, HTTP basic auth with app key/secret)
   using the file's refresh token, and keeps the new access token, its expiry
   and any rotated refresh token **only in this process**. Nothing is written
   back; a restart simply reads whatever the owning tooling has left on disk.

Because of step 3 the in-memory token is deliberately *ahead of* the file from
the first refresh onward. That is normal, not an error, and the status line
says so. It is also why a stale re-read is never adopted: taking the file back
would retry a token Schwab already rejected and then spend a refresh grant it
may have rotated away.

The file's permissions are the other tooling's business too. `wingspan-cli
doctor` and the Diagnostics card **warn** (never fail, and never `chmod`)
when the file is readable beyond its owner; tighten it where it is
produced.

## Request behaviour

- `GET /marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…` —
  OSI option symbols, chunked ≤ 50 per request, with 429 backoff and a
  request budget.
- On 401 the client re-reads the token file, refreshes in memory at most
  once, and retries once.
- Failures degrade down the fallback chain (`yfinance` → cache → statement
  marks) with the reason shown in the UI status pill.

## Verification

`wingspan-cli doctor` (from a terminal) and the desktop app's Diagnostics
card (Data Health → Run checks, or Help → Diagnostics) print the **same
checklist** — config, dirs, python/yfinance, token presence/schema/age and a
permissions *warning*, and (with `--probe` on the CLI, or the Run checks
button in the app) the interpreter itself and network reachability. Neither ever repairs, chmods or
rewrites the token file — both only read and report.
