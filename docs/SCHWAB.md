# Schwab integration

Wingspan uses the Schwab Trader API **market data** endpoints for live SPX
spot and option quotes (bid/ask/mark/IV/greeks/underlyingPrice). Trading
endpoints are never called. Live reconciliation against Accounts & Trading
`positions` is a documented extension point.

## What you need

1. A Schwab developer app (app key + secret) with Market Data product access.
2. A token file on the machine that runs wingspan. Interactive login is out
   of scope for the app itself — generate the token once with
   [schwab-py](https://schwab-py.readthedocs.io/)'s
   `client_from_login_flow`, or any tool of your choice.
   (`wingspan schwab login` is a planned extension.)

## Configuration

```toml
[schwab]
token_path = "~/.config/wingspan/schwab_token.json"
app_key_env = "SCHWAB_APP_KEY"       # env var NAMES are configurable,
app_secret_env = "SCHWAB_APP_SECRET" # values come from the environment
refresh = true                        # rewrite the token file on refresh
```

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
implementation) for portability.

- `account_number` is treated as a **secret**: redacted in logs and in
  `GET /api/v1/config`.
- Access-token refresh (`POST /v1/oauth/token`, refresh grant, HTTP basic
  auth with app key/secret) rewrites **the same schema** — updates
  `access_token`/`access_expires_at`, preserves everything else — under an
  advisory file lock, with an atomic rename and a timestamped backup next to
  the file. Set `refresh = false` if another tool owns the file.
- Schwab refresh tokens live **7 days**. Health reporting shows the token age
  from `refresh_created_at` and raises a hard warning from day 6. Re-run your
  login flow weekly (a cron reminder is a good idea).
- The token file should be `0600`; `wingspan doctor` warns if it is
  world-readable.

## Request behaviour

- `GET /marketdata/v1/quotes?symbols=$SPX,SPX%20%20%20270219P07325000,…` —
  OSI option symbols, chunked ≤ 50 per request, with 429 backoff and a
  request budget.
- On 401 the client refreshes once and retries.
- Failures degrade down the fallback chain (`yfinance` → cache → statement
  marks) with the reason shown in the UI status pill.

## Verification

- `wingspan schwab status` — token file, ages, redacted account, a live quote
  round-trip.
- `wingspan doctor` — config, dirs, python/yfinance, token permissions,
  network reachability.
