# Schwab integration

Wingspan uses the Schwab Trader API **market data** endpoints for live SPX
spot and option quotes (bid/ask/mark/IV/greeks/underlyingPrice), and for the
option chain the Model tab shows. Trading
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
- The tokens themselves never reach a log line or a panic dump either: the
  access and refresh tokens redact themselves in `SchwabToken`'s hand-written
  `Debug`, and the copy of the file's refresh token the client keeps in memory
  (to tell whether the file has moved on) is held in the same redacting
  `Secret` newtype tastytrade uses, so `{:?}` on the whole client is safe.
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
- `GET /marketdata/v1/expirationchain` and `GET /marketdata/v1/chains` — the
  option chain behind the Model tab, below. They go through the same
  authenticated GET, so everything on this list applies to them too.
- On 401 the client re-reads the token file, refreshes in memory at most
  once, and retries once.
- Failures degrade down the fallback chain (`yfinance` → cache → statement
  marks) with the reason shown in the UI status pill.

## Option chain (the Model tab)

The Model tab lists expirations and shows a chain, which is two more
market-data reads (still no trading endpoint). Both ride the quotes
endpoint's authenticated GET: the bearer token, the refresh on a `401`, the
`Retry-After`-aware backoff on a `429` and the rolling request budget are one
code path, so a chain refresh spends from the same budget as the position
quotes. Schwab is one rung of the **option** fallback ladder for these calls,
like for quotes; the source that answered, and why any earlier rung did not,
is on the response's `source` / `degraded`.

### Expirations

`GET /marketdata/v1/expirationchain?symbol=%24SPX` answers

```json
{"expirationList": [
  {"expirationDate": "2026-10-02", "daysToExpiration": 1, "expirationType": "W",
   "settlementType": "P", "optionRoots": "SPXW", "standard": true},
  {"expirationDate": "2026-10-16", "daysToExpiration": 15, "expirationType": "M",
   "settlementType": "A", "optionRoots": "SPX,SPXW", "standard": true}
]}
```

Each entry becomes **one expiration per root** in `optionRoots`, so a third
Friday lists the AM-settled `SPX` monthly and the PM-settled `SPXW` weekly
separately.

- **Settlement** comes from the entry's `settlementType` (`A` / `P`) when it
  names a single root. When the roots string carries several, one
  `settlementType` cannot be right for all of them, so each root's own rule
  decides (`SPX` is AM, `SPXW` is PM) and the entry only covers a root the rule
  does not know. Never the calendar.
- **Kind** is `M` monthly, `W` weekly, `Q` quarterly; anything else
  (Schwab's `S`, "standard", for one) is `unknown`, an open set the SPA shows
  as it is. `daysToExpiration` is not read — `dte` is computed against the
  market date, and expired dates are dropped by the fallback chain, not here.
- An entry with no readable date, no root, or a root with no settlement is
  skipped (counted in a debug log line, never a panic). If the list has entries
  and none can be read, that is a decode error naming the count, so the
  degraded line says so rather than showing an empty list.

### Chain

`GET /marketdata/v1/chains?symbol=%24SPX&contractType=ALL&fromDate=2026-10-16&toDate=2026-10-16&includeUnderlyingQuote=false`
returns every contract of one date in a single response (the window the
service computes is ignored: one call costs the same whole or in part):

```json
{"symbol": "$SPX", "status": "SUCCESS",
 "callExpDateMap": {"2026-10-16:15": {"7500.0": [
   {"putCall": "CALL", "symbol": "SPX   261016C07500000",
    "bid": 260.1, "ask": 262.4, "last": 261.0, "mark": 261.25,
    "totalVolume": 1203, "openInterest": 8812, "volatility": 17.125,
    "delta": 0.61, "gamma": 0.0009, "theta": -1.9, "vega": 6.1,
    "quoteTimeInLong": 1790870000000, "strikePrice": 7500.0,
    "expirationDate": "2026-10-16T00:00:00.000+00:00", "settlementType": "A"},
   {"putCall": "CALL", "symbol": "SPXW  261016C07500000", "…": "…"}
 ]}}},
 "putExpDateMap": {"2026-10-16:15": {"7500.0": [ "…" ]}}}
```

- **Identity.** A contract is its OSI `symbol`, parsed like a position leg's
  (`OptionContract::from_osi`); a symbol that lost its padding still reads. Only
  when the symbol is unusable does the entry fall back to `putCall`,
  `strikePrice`, `expirationDate`, `optionRoot` and `settlementType`, with
  the map's key supplying the date and the strike if the entry does not carry
  them. An entry that yields no contract is skipped and counted.
- **Both roots in one array.** On a third Friday the SPX and SPXW contracts
  share the key and the strike arrays. Only contracts of the requested
  `(expiry, root)` are kept; the other root is not an error.
- **Prices.** `bid` / `ask` / `last` / `mark` as sent; the mark is the
  provider's when positive, else the mid of a sane (uncrossed) market, else
  none — the same rule as the quotes path. A negative price is a sentinel and
  is dropped; a `last` of 0 means "has not traded" and is dropped.
- **`volatility` is in percent** — divided by 100 for the hint, and rejected
  when `<= 0`, above 500 or not a number. It is only a diagnostic hint: the
  chain's IVs and greeks are our own solves.
- **Sentinels.** Schwab marks "no value" as `-999` or the string `"NaN"`; each
  hinted greek (`delta`, `gamma`, `theta`, `vega`) rejects `-999` (a negative
  greek is legitimate otherwise). A bad hint never costs the quote.
- `totalVolume` and `openInterest` become `volume` / `open_interest`;
  `quoteTimeInLong` (epoch ms) is the quote time, the fetch time if absent.

An empty chain (`status: "FAILED"` with empty maps, the shape for a date with
nothing listed) is a `no data` failure; a body with neither map is a decode
error. Both let the fallback chain move on to the next source.

### Untested against the live API

This code was written, and its tests run, without a Schwab token: the payloads
in the unit tests are **built from the endpoint's documented field list, not
captured from the service**, and the wiremock tests prove the request shape and
the shared retry/budget behaviour only. A first live run should confirm each
of these assumptions (the chain's `degraded` line, shown on the Model tab, names
the failure when one is wrong):

1. `expirationchain?symbol=$SPX` accepts the percent-encoded `%24SPX` and lists
   `SPX` and `SPXW` together in `optionRoots` as a comma-separated string.
2. `expirationType` is `M` / `W` / `Q` / `S`; what a third-Friday SPX monthly
   and a quarter-end SPXW report (we expect `M` or `S` and `Q`); and that
   `settlementType` is `A` / `P`.
3. `chains` without `strikeCount` or `range` returns **every** strike of the
   date, not a default band around the money — otherwise the table silently
   shows only that band.
4. `fromDate == toDate` selects exactly that date, and both roots come back in
   the same strike arrays.
5. Symbols arrive as 21-character OSI (`SPXW  261016C07500000`), and
   `putCall` is `CALL` / `PUT`.
6. "No value" is the **string** `"NaN"` (or `-999`). A bare, unquoted `NaN`
   token is not valid JSON and would fail the whole response as a decode error.
7. `volatility` is in percent, and the response size for a busy third Friday
   (a thousand-odd contracts) fits the 10 s request timeout.
8. `quoteTimeInLong` is present on each contract, and `mark` is populated
   outside regular hours (otherwise the mid is used).

## Verification

`wingspan-cli doctor` (from a terminal) and the desktop app's Diagnostics
card (Data Health → Run checks, or Help → Diagnostics) print the **same
checklist** — config, dirs, python/yfinance, token presence/schema/age and a
permissions *warning*, and (with `--probe` on the CLI, or the Run checks
button in the app) the interpreter itself and network reachability. Neither ever repairs, chmods or
rewrites the token file — both only read and report.
