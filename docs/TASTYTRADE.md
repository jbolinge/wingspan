# tastytrade integration

Wingspan can price your book against **tastytrade** market data — SPX spot and
SPX/SPXW option quotes — as an alternative to Schwab, from the same fallback
chain and with the same status pill. Only `GET /market-data/by-type` and
`POST /oauth/token` are ever called; the grant is minted `read`-only and no
trading, account or position endpoint exists in this code.

It is **off by default and not in the default fallback chain**. Turning it on
is two deliberate acts: `[tastytrade] enabled = true`, and naming
`"tastytrade"` in `[market] spot_sources` / `option_sources`. Upgrading an
existing install changes nothing until you do both.

## What you need

1. **An OAuth client.** At `my.tastytrade.com`: Manage → My Profile → API →
   OAuth Applications → **+ New OAuth client**. Give it the **`read` scope
   only** — never `trade`; wingspan has no code that could use it. The form
   demands a redirect URI, which a personal grant never uses; anything valid
   will do. **The client secret is shown once** — copy it then, or you will be
   minting a new client.
2. **A personal grant.** On that same client: Manage → **Create Grant**. This
   mints a **refresh token** with no browser round trip and no interactive
   login. A personal-grant refresh token **does not rotate and does not
   expire**.
3. **A funded account**, for live data. A first unfunded account gets 14 days
   of real-time quotes and is delayed after that — see **Delayed data** below,
   because nothing on the wire tells us which one you have.

Wingspan never performs an interactive login and never receives a rotated
refresh token, so — unlike Schwab — there is nothing for it to persist.

## Configuration

The credentials are **values, resolved by variable name**, exactly as
`[schwab]` does it: `wingspan.toml` stores the *names*, so a leaked config
file is not a credential leak.

| config key | default variable name | required |
|---|---|---|
| `client_secret_env` | `TASTYTRADE_CLIENT_SECRET` | yes |
| `refresh_token_env` | `TASTYTRADE_REFRESH_TOKEN` | yes |
| `client_id_env` | `TASTYTRADE_CLIENT_ID` | no — sent only when set |

A variable that is present but blank after trimming counts as **not set**: a
half-finished env file must not send a blank secret to the token endpoint and
earn a failed login we then have to cool down from.

**In the desktop app the credentials file arrives via a native picker.** Data
health → Settings → Market data → **tastytrade** → Credentials file →
Browse… opens a file chooser; after you pick a file, a confirmation dialog
names the full path and says what Wingspan will do with it. Only on
confirmation is the path *string* written to `wingspan.toml` — the file itself
is never opened, copied, chmoded or created by that action. Only the variable
*names* are editable text on the settings form; no control anywhere accepts a
secret value or a path string.

```toml
[tastytrade]
enabled = true
env_file = "/home/trader/.config/tastytrade/secrets.env"  # KEY=VALUE, read-only
client_secret_env = "TASTYTRADE_CLIENT_SECRET"
refresh_token_env = "TASTYTRADE_REFRESH_TOKEN"
client_id_env = "TASTYTRADE_CLIENT_ID"   # optional on the refresh grant
sandbox = false          # api.cert.tastyworks.com serves no market data
delayed = false          # only you can tell us; there is no wire marker
api_version = "20250904" # Accept-Version; empty sends no header

[market]
spot_sources = ["tastytrade", "schwab", "yfinance", "cache"]
option_sources = ["tastytrade", "schwab", "yfinance", "cache", "statement"]
```

**One env file may serve both providers.** `[tastytrade] env_file` and
`[schwab] env_file` are merged into a single map keyed by variable name, and
the **first file to supply a name wins** (Schwab's is read first) — dotenv
semantics, where a file supplies defaults and never overrides. The two
providers' default names do not collide. A variable already set in the **real
environment always wins** over both. A missing or unreadable `env_file` is a
sentence in Data health, not a refusal to start (`wingspan-cli` and
`wingspan-devserver`, where the file is exported into the process environment
instead, treat a missing path on the command line as the typo it is and
refuse). Neither happens while `[tastytrade] enabled = false`: the file is only
read for a provider that is going to be built.

## No token file

This is the section `docs/SCHWAB.md` spends on a token file's schema, age and
permissions. There is nothing here to put in it, and that is the point:

- The refresh token **does not rotate and does not expire**, so it is a static
  string in an environment variable. There is nothing on disk for wingspan to
  read, and therefore nothing it could ever be tempted to write.
- The **15-minute access token lives only in memory**, for the life of the
  process. It is never serialized, never logged and never written anywhere.
- The credentials file the picker points at is **read-only to this
  application**: wingspan reads it and never writes, creates, renames,
  truncates, chmods, locks, copies or backs up anything in its directory.
  There is no code path that can.
- The client secret and refresh token are a `Secret` newtype with no
  `Serialize`, no `Display` of the value and redacting `Debug`, so they cannot
  reach a log line or a panic dump even by accident. The type is shared with
  the Schwab client, which holds its in-memory copy of the token file's
  refresh token in it for the same reason. The status the CLI, the
  Diagnostics card and `settings_read` publish carries **no credential field
  at all** — not even a masked one.

If tastytrade ever did return a rotated refresh token (a personal grant does
not), it is adopted **in memory for this session only**, logged without the
value, and nothing is written.

## Request behaviour

```
GET {base}/market-data/by-type?index=SPX&equity-option=SPXW%20%20260918P05900000&…
Authorization: Bearer <access token>      (marked sensitive)
Accept: application/json
Accept-Version: 20250904
User-Agent: wingspan/<version>
```

- **Symbols are 21-character OCC/OSI strings**, exactly what the app already
  builds for Schwab — `SPXW  260918P05900000` (SPXW, 2026-09-18, put, 5900) and
  `SPX   261218C07000000` (SPX, 2026-12-18, call, 7000). Both SPX and SPXW go
  under `equity-option`; only the cash index goes under `index`, as a bare
  `SPX` with no `$` and no `.`. The dxfeed *streamer* spelling
  (`.SPXW260918P5900`) belongs to the DXLink websocket, which this provider
  deliberately does not speak, and is never constructed or sent.
- **Repeated query keys, not comma-joined**, and every value percent-encoded
  over the unreserved set, so a padding space travels as `%20` and never `+`.
- **≤ 100 symbols per request**, tastytrade's own cap, counted across every
  type parameter. Spot and option quotes are separate trait calls and so
  separate requests: 1 + ⌈N/100⌉ per refresh.
- **The `User-Agent` is mandatory.** A request without one is answered with an
  nginx HTML 401, not a JSON error. It is set on the client, so
  `/oauth/token` carries it too.
- **`Accept-Version` is pinned** so a server-side rename surfaces as a
  deliberate 406 naming the setting rather than a silently missing field in a
  live dashboard. Clearing `api_version` opts out of the header entirely.
- **429** backs off with capped exponential delay plus jitter (tastytrade
  sends no `Retry-After`; wingspan consults it anyway, harmlessly), up to three
  retries, then reports the attempt count. A 5xx backs off the same way.
- A **rolling request budget** (120 per 60 s) is taken from before *every*
  call, `/oauth/token` included, so a storm cannot route around it through the
  token endpoint.
- **401 → refresh once → retry once.** A second 401 after that refresh is
  reported, never retried again.
- **403** is reported as a grant missing the ``read`` scope, and is not
  retried.
- Failures degrade down the fallback chain with the reason in the status pill.
  Zero usable quotes is a hard error, never an empty success — an empty
  success would stop the chain at tastytrade and leave the book priced off the
  fallback vol. A chunk that fails after others succeeded keeps what worked and
  says how many contracts were not quoted.

### The failed-login cooldown

tastytrade blocks an IP for roughly **eight hours** after repeated failed
logins, and the blocked requests then *time out* rather than answering 401 —
so the failure that provokes a retry loop is also the one that hides it. Every
path that could spend a token exchange goes through a guard first:

- A failed exchange sets a cooldown, and every later attempt returns the stored
  sentence **without touching the network** until it passes.
- The penalty starts at 60 s and **doubles on each consecutive failure**, up to
  15 minutes. Any success clears it.
- An **auth-shaped** rejection — HTTP 401 or 403, or an `error.code` of
  `invalid_grant` / `invalid_client` / `invalid_credentials` / `unauthorized` —
  jumps straight to the cap and marks the provider `Unavailable`. It is not
  permanent: changing settings rebuilds the client, and a server that was
  wrong recovers on its own.
- A **timeout on the token endpoint specifically** is treated as a possible IP
  block and enters the cooldown too, with its own sentence saying so.

This is why neither `wingspan-cli doctor` nor the Diagnostics card runs a token
exchange (see **Verification**).

## No greeks, no implied volatility

The REST market-data response carries **neither** — verified against the
market-data OpenAPI schema; they live on the DXLink `Greeks` event, which this
provider does not speak. `iv_hint` and `greeks_hint` are therefore always
`None`, and `wingspan-pricing` solves IV from the marks and bumps the greeks
itself, exactly as it does for every other rung. A hint here would be a lie.

A dead strike comes back with no bid, ask or mark. That is a quote without a
mark, not a dropped contract and not a failed batch.

## Sandbox

`sandbox = true` points the client at `api.cert.tastyworks.com`. Auth works
there, but **every `/market-data` call answers 502** and the whole environment
resets every 24 hours, so it cannot smoke-test quotes. Sandbox credentials are
not interchangeable with production ones. Wingspan reports the environment
honestly rather than pretending: the provider is `Degraded` from construction
with the reason `tastytrade sandbox: api.cert.tastyworks.com serves no market
data`, and a 502 there is reported as permanent and not retried (a production
502 still backs off).

There is deliberately **no `base_url` key**. The only two hosts the client can
be aimed at are compile-time constants, so no config file — hand-edited or
otherwise — can point a bearer token at someone else's server.

## Delayed data

There is **no per-response delayed marker**, and off-hours quote age is
indistinguishable from a delayed feed, so an automatic detector would cry wolf
every evening. `[tastytrade] delayed` is therefore a statement *you* make when
you know the account is unfunded past its trial, or professional without the
real-time opt-in. When it is true the provider reports `Degraded`, the pill
reads `DELAYED · tastytrade`, and the doctor prints a warning row. **This is
the one thing the app cannot work out for you.**

## Health, and what each state means

| state | when |
|---|---|
| `Ready` | production host, not marked delayed, no error recorded |
| `Degraded` — the last error | a request or an exchange failed; the sentence says what and, if the cooldown is holding, for how long |
| `Degraded` — sandbox | `sandbox = true`, which serves no market data |
| `Degraded` — delayed | `delayed = true` |
| `Unavailable` | the credentials were rejected (`auth_failed`) — loud, and self-healing after the cooldown if the server was wrong |

A real failure is shown before the standing sandbox/delayed conditions,
because it is the sentence you have to act on first.

## Verification

`wingspan-cli doctor` (from a terminal) and the desktop app's Diagnostics card
(Data health → Run checks, or Help → Diagnostics) print the **same
Tastytrade rows**:

```
Tastytrade
  [ok  ] secrets env            /home/trader/.config/tastytrade/secrets.env
  [ok  ] credentials            client secret from $TASTYTRADE_CLIENT_SECRET, refresh token from $TASTYTRADE_REFRESH_TOKEN
  [    ] client id              $TASTYTRADE_CLIENT_ID is not set (optional on the refresh grant)
  [    ] environment            production api.tastyworks.com
  [    ] api version            Accept-Version: 20250904
  [    ] access token           not obtained yet (it is fetched on the first quote)
  [    ] budget                 0 of 120 request(s) used in the window
  [ok  ] health                 ready
```

With `[tastytrade] enabled = false` the whole section is one quiet note line,
`not enabled ([tastytrade] enabled = false)`, and contributes no warning and no
problem count.

**Neither of them runs a token exchange.** The section reads the running
client's last known status and reaches no network at all: `diagnostics` builds
a fresh client on every call, so an exchange there would run on every press of
**Run checks** and the cooldown that exists to prevent an hours-long IP block
would be exactly what the button defeated. The end-to-end check lives where the
operator has to ask for it — `doctor --probe`, or **Run checks**, whose Live
probe section exercises the whole rung including the exchange. Names only ever
appear in these rows; never a token, never a secret, never a client-id value.

## Troubleshooting

| what you see | what it means |
|---|---|
| `not enabled ([tastytrade] enabled = false)` | the default. Set `enabled = true` **and** name `tastytrade` in `[market] spot_sources`/`option_sources`. |
| `$TASTYTRADE_CLIENT_SECRET and $TASTYTRADE_REFRESH_TOKEN are not set` | neither the real environment nor any `env_file` supplied them — or they are set but blank. |
| `[tastytrade] env_file … does not exist` | the picker wrote a path that has since moved. The credentials may still resolve from the real environment or from `[schwab] env_file`. Picking the file again rebuilds the app and the row goes away — the sentence describes the config the running app holds, not the one it booted on. |
| `tastytrade rejected our credentials (…)`, provider `Unavailable` | an auth-shaped rejection. Check the client secret and the refresh token; if the grant was deleted at `my.tastytrade.com`, create a new one. The cooldown is at its cap — do not retry in a loop. |
| `… — if your grant requires a client id, set [tastytrade] client_id_env` | the exchange failed 4xx while no client id was configured. Set `TASTYTRADE_CLIENT_ID` to the OAuth client's id and try once. |
| `the token endpoint did not answer — repeated failed logins are blocked for hours` | most likely an IP block from earlier failed logins. Fix the credentials **before** retrying; time is the only cure. |
| `not retrying the token exchange until HH:MM:SSZ` | the cooldown is holding, on purpose. Nothing is being sent. The line is shown only while that deadline is still ahead: once it passes, the next quote tries again and the row goes back to the failure that armed it. |
| ``HTTP 403 — the OAuth grant is missing the `read` scope`` | the client was minted without `read`. Create a new client with the scope. |
| `the pinned [tastytrade] api_version "…" is no longer supported` (406) | pick the version tastytrade now documents, or clear `api_version` to send no header. |
| `the certification environment serves no market data — clear [tastytrade] sandbox to use live data` | `sandbox = true`. It is not a transient failure; there are no quotes there at all. |
| `DELAYED · tastytrade` on the pill | `[tastytrade] delayed = true` — your own statement, not a detection. |
| `tastytrade has no data for any of the N requested contracts (not returned: …)` | the server returned nothing for those symbols. The chain falls through to the next rung; check entitlement and the expiry/strike. |

## Known unknowns

Everything below is an unresolved question about the API, with what the code
does about it. None of them can break a run; each degrades to a missing field
or an honest sentence.

- **Is `client_id` really optional on a personal refresh grant?** Unverified.
  It is omitted **entirely** when unset — never sent as `null` — and a 4xx
  while it was absent appends the "set `[tastytrade] client_id_env`" sentence.
- **kebab-case or camelCase on the wire?** The OpenAPI document read back
  camelCase in one place. Every field lookup takes a list of candidate keys,
  kebab-case first with camelCase as an alias.
- **`trading-halted` or `is-trading-halted`?** Both spellings are read. The
  value is informational and never decides whether a quote is published.
- **Comma-delimited or repeated query keys?** The docs say comma-delimited;
  the Python SDK emits repeated keys. OCC symbols contain literal spaces and
  comma-splitting around one is untested, so repeated keys it is — isolated in
  one function so switching is a one-line change.
- **The rate-limit thresholds are unpublished**, and a 429 carries no
  documented body or headers. Only the status is branched on, and wingspan
  keeps its own conservative budget rather than probing for the real ceiling.
- **A 401 body may be nginx HTML rather than JSON.** The error envelope is
  parsed only when the body parses as JSON; otherwise the body is truncated to
  200 characters. Nothing unwraps a JSON body.
- **Does the response preserve request order, or return an item per requested
  symbol?** Assume neither. Items are matched back by `symbol` (padded and
  unpadded), an item for a symbol we did not ask for is ignored, and the ones
  that never came back are named in the degraded reason rather than invented.
- **SPX index entitlement beyond funding** is not something a response states.
  A zero or absent index price is treated as no price at all, and the chain
  falls through — never a published `0`.
- **Greeks and IV are absent** as far as the schema shows, which is an
  absence-of-evidence conclusion. They are hard-coded `None` rather than
  guessed at, and `wingspan-pricing` solves both.
- **Real-time entitlement cannot be detected from a response**, which is why
  `delayed` is a setting and not a sensor.
