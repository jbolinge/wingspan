# Configuration

Config is TOML + environment, merged by figment. Two different resolution
ladders apply, depending on which binary is reading it:

**The desktop app** (the shipped product) reads only:

1. `~/.config/wingspan/wingspan.toml`
2. `WINGSPAN_*` environment variables

It never reads `./wingspan.toml`, `./.env` or `$WINGSPAN_CONFIG` — an app
launched from a `.desktop` entry has a working directory nobody chose, so a
stray file sitting there must not silently override anything.

**`wingspan-cli` and `wingspan-devserver`** (dev-only) honour the fuller
ladder, precedence highest last:

1. `~/.config/wingspan/wingspan.toml`
2. `./wingspan.toml`
3. `$WINGSPAN_CONFIG` (path)
4. `--config <path>`
5. `WINGSPAN_*` environment variables (e.g. `WINGSPAN_TRADES__TAB`)

Start from `wingspan.example.toml` (kept exhaustive and in sync with the
code; run `wingspan-cli doctor` after editing).

## [trades]
| key | default | meaning |
|---|---|---|
| `sheet_url` | `""` | deployer default: a Google Sheet share link. Bearer-equivalent, masked everywhere; the source you import in the app outranks it |
| `csv_path` | `""` | alternative deployer default: a local CSV with the same columns. Picker-written (`pick_csv_file`) |
| `tab` | `"Legs"` | which tab of the spreadsheet to read |
| `poll_secs` | `0` | `0` = manual Reload only; any other value is floored at 60 |
| `fetch_timeout_secs` | `20` | config-file only |
| `cache` | `true` | keep the last fetch on disk so an offline launch still shows a book |
| `timezone` | resolves to `"America/Chicago"` | the clock leg dates and statement rows are read in; was `statement_timezone` |
| `root` | — (deprecated) | typed but unread |

`poll_secs` of `0` means manual Reload only, and any other value you set is
floored at 60 — this fetches a spreadsheet, not a local file watch, so nothing
polls faster than once a minute. `cache = false` opts out of the on-disk copy
(`<state_dir>/trade_source_cache.csv` + `.json`) — which is made for an
imported CSV file just as much as for a fetched sheet; with it off, a launch
with no network (or no CSV) starts with an empty book instead of a stale
one.

`sheet_url` is a **deployer default only** — bearer-equivalent, exactly like
`[schwab] token_path`: masked in `app_config`, in `Display`/`Debug`, in every
DTO and in the log, never accepted on a settings patch, and outranked the
moment you import a source in the app.

`timezone` was spelled `statement_timezone`; the old spelling is still
accepted for one release. Neither key is written to a fresh config file —
both are absent/unset, and `"America/Chicago"` is what the resolver falls
back to when it finds neither. Saving settings from the app writes the
`timezone` key and, in the same write, removes a legacy `statement_timezone`
from the file, so the two spellings can never sit side by side disagreeing
about which one is live.

`root` is typed but no longer read. An existing `wingspan.toml` that still
carries it loads unchanged, and the app says so once in its boot notes:
"`[trades] root` is no longer read; import a Legs sheet or CSV".

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
reason in the status pill — and `wingspan-cli doctor` reports them.

Only variable *names* live in `wingspan.toml` — never a credential. A variable
that is set but **blank after trimming counts as not set**, for `[schwab]` and
`[tastytrade]` alike: a half-finished env file is not a credential, and sending
an empty app key to the token endpoint would replace the actionable
"`$SCHWAB_APP_KEY` is not set" with whatever 4xx the server answers.

Both `env_file`s are read with conventional dotenv lexing, which matters when
the file is hand-written: a whole line starting with `#` is a comment, and on
an **unquoted** value a `#` that follows whitespace ends the value
(`SCHWAB_APP_KEY=abc  # from my account` is `abc`). A `#` with nothing before
it is an ordinary character (`abc#def` stays whole), and quotes protect
everything inside them (`"a # b"`), with a comment allowed after the closing
quote.

**There are now five path-shaped settings, not four — `[yfinance] python`,
`[schwab] token_path`, `[schwab] env_file`, `[tastytrade] env_file` and
`[trades] csv_path` — each written by a native picker** (Data health →
Settings → Market data → Browse…, or the Trade source control's **Pick a
CSV file…**, each behind a
confirmation dialog naming the file and what it will be used for), and they
are keys nothing the SPA sends can set: `settings_update` refuses a payload
that names any path key, because one decides what the app *executes*, some
decide where it reads a *secret* from, and the last decides which file it
parses as your book. Editing them here by hand is still fine — the rule is
about what the window may write, not about your text editor.

Numbers Settings can write are bounded where a bad one breaks the app rather
than merely annoys: `points` ≥ 2, `range_pct` > 0, `refresh_secs_open` and
`refresh_secs_closed` ≥ 1. Save refuses the whole patch with a sentence
naming the key and writes nothing; a file hand-edited to one of those values
does not load at all, and the app boots on the built-in defaults with the
same sentence in the dialog.

Two numbers are deliberately *not* on that list. `[trades] poll_secs` is
floored, never refused: Settings and a hand-edited file both accept any
value, and anything between 1 and 59 simply becomes 60 (`0` stays "manual
Reload only"). `[trades] fetch_timeout_secs` is not something Settings can
write at all — it is config-file only, below — and it is refused at *load*,
not at Save: a value outside `1..=120` stops the file from loading, with the
app booting on the built-in defaults and the sentence in the dialog.

`[schwab] token_config`, `[yfinance] script`, `[market] state_dir`, `[trades]
fetch_timeout_secs` and `[trades] sheet_url` are the opposite: config-file
only, with no picker and no settings-card field, because one names a second
file of paths, one changes which script runs, one changes where the app's own
state lives, one changes how long a fetch may take, and the last is a share
link — bearer-equivalent, exactly like a token path — see `docs/DESKTOP.md`.

## [tastytrade]
A second live source, alongside Schwab — SPX spot and SPX/SPXW option quotes
over an OAuth2 personal grant. See `docs/TASTYTRADE.md` for how to mint the
grant and what each health sentence means.

**Off by default, and not in the `[market]` ladders.** Turning it on is two
deliberate acts: `enabled = true` here, *and* naming `"tastytrade"` in
`spot_sources`/`option_sources`. An upgrade changes nothing until you do both.

| key | default | meaning |
|---|---|---|
| `enabled` | `false` | build the provider at all |
| `env_file` | unset | external `KEY=VALUE` file holding the variables below; **picker-written** |
| `client_secret_env` | `"TASTYTRADE_CLIENT_SECRET"` | name of the variable holding the OAuth client secret |
| `refresh_token_env` | `"TASTYTRADE_REFRESH_TOKEN"` | name of the variable holding the personal-grant refresh token |
| `client_id_env` | `"TASTYTRADE_CLIENT_ID"` | optional on the refresh grant; sent only when the variable is set |
| `sandbox` | `false` | use `api.cert.tastyworks.com` |
| `delayed` | `false` | this account is not entitled to real-time data |
| `api_version` | `"20250904"` | sent as `Accept-Version`; empty sends no header |

Only variable *names* live here — never a credential. A variable that is set
but blank after trimming counts as **not set**, and the `env_file` is lexed the
same way `[schwab] env_file` is (see the `#`-comment rule there).

**There is no `token_path` twin of `[schwab]`'s, and there never will be.** A
personal-grant refresh token does not rotate and does not expire, so it is a
static string in an environment variable: there is nothing on disk for wingspan
to read, and the 15-minute access token lives only in memory.

`env_file` **may be the same file `[schwab] env_file` names.** The two are
merged into one map keyed by variable name and the **first file to supply a
name wins** (Schwab's is read first) — dotenv semantics, where a file supplies
defaults and never overrides. The two providers' default names do not collide.
A variable already set in the **real environment always wins** over both. In
the desktop app a missing or unreadable `env_file` is a sentence in Data health
rather than a refusal to start — one re-derived from the configuration the
running app was built from, so choosing a file with the picker clears it
without a restart. On the command line, where the file is exported into the
process environment instead, a path that does not exist is the typo it is and
the run refuses. Both of those are gated on `enabled`: while
`[tastytrade] enabled = false` the file is never opened at all, so a stale path
left behind by a provider you switched off costs nothing and no secret is read
for a provider that will not be built.

`sandbox = true` is honestly degraded, not silently broken: auth works at
`api.cert.tastyworks.com` but every `/market-data` call answers 502 and the
environment resets every 24 hours, so the provider reports `Degraded` from
construction. There is deliberately **no `base_url` key** — the only two hosts
the client can be aimed at are compile-time constants, so no config file can
point a bearer token at someone else's server.

`delayed` is a statement *you* make. There is no marker on the wire, and
off-hours staleness is indistinguishable from a delayed feed, so an automatic
detector would cry wolf every evening.

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
first time it runs — an install never needs the `bridge/` directory on the
machine, only a Python interpreter with `yfinance` installed
(`docs/DESKTOP.md`). Set `script` to point at a different bridge
script (e.g. while hacking on it); `wingspan-cli doctor` reports which
interpreter and version it found. It is entirely optional: with no working
interpreter Wingspan says so ("Delayed quotes unavailable — you lose only
the middle rung") and keeps going on the cache and the statement marks.

## [market]
| key | default |
|---|---|
| `spot_sources` | `["schwab", "yfinance", "cache"]` |
| `option_sources` | `["schwab", "yfinance", "cache", "statement"]` |
| `refresh_secs_open` / `refresh_secs_closed` | `15` / `300` (at least `1`) |
| `pause_when_hidden` | `false` (stop fetching while the desktop window is hidden or minimised — losing keyboard focus is *not* that: a dashboard behind another window, or on a second monitor, keeps refreshing; regaining focus asks for a fresh snapshot, debounced to one request per 5 s) |
| `state_dir` | `~/.local/share/com.jbolinge.wingspan/state` |

The legal source names are `schwab`, `tastytrade`, `yfinance` and `cache`,
best first; `option_sources` also takes `statement`. Statement marks are
option-only — they are the marks off your own statements, and a statement
carries no index quote — so naming `statement` in `spot_sources` is refused.
`fixture` is not a legal name in either list: the synthetic surface exists
only for `--demo`, which writes its own ladders and never reads `[market]`, so
naming it here could only ever produce "no spot source named `fixture` is
configured".
`tastytrade` is deliberately absent from both defaults — adding it is one edit (`spot_sources = ["tastytrade",
"schwab", "yfinance", "cache"]`), and it does nothing until `[tastytrade]
enabled = true` as well.

The two ladders are **config-file only**: like `[market] state_dir`, they have
no settings-card field, so today you change them by hand in `wingspan.toml`.
That is the asymmetry to watch when turning tastytrade on — `[tastytrade]
enabled` *is* a card toggle, but naming `"tastytrade"` in the ladders is not.

A source list **carried by a `settings_update` patch** that is **empty**, or
that names something which is not a provider, is refused with one sentence and
`wingspan.toml` is left untouched — `[market] spot_sources names "tastytrde",
which is not a provider — pick from schwab, tastytrade, yfinance, cache`, or
`[market] option_sources is empty — an empty ladder has nothing to fall back
to`. The list of names in that sentence is the one legal for the key it names,
so `statement` appears in it only for `option_sources`. Nothing in the window
sends those keys yet; the check is what keeps a future card (or a hand-crafted
IPC call) from writing a ladder the chain cannot use. The check reads only the
list the patch carries: a **hand-edited** file with the same mistake still
loads, still boots and does not block your other saves (refusing would cost you
every other setting you have); `wingspan-cli doctor` and the Diagnostics card
warn about the unknown name instead.

## [pricing]
`risk_free_rate`, `dividend_yield` (fallbacks when forward calibration has no
ATM pair), and `weighted_vega { reference = "30d" (default) | "<n>d" |
"front_expiry", exponent = 0.5 }` — the reference in use is always shown next
to the column. Every positions-table row is measured against the whole book's
reference, so the W.V column adds up to the All total in either mode.

## [risk_graph]
`points` (241, at least 2), `range_pct` (0.10, positive), `tplus_preset` (`"quarters"` =
T+0, T+¼·DTE, T+½·DTE, T+¾·DTE, T+(DTE−1), plus the front expiry).

## The valuation date: `--as-of`, `WINGSPAN_AS_OF` and the demo book

Nothing in `wingspan.toml` sets the day the book is valued on — it is today,
unless something moves it.

| how | effect |
|---|---|
| `wingspan-devserver serve --as-of YYYY-MM-DD` | value the dev server's book on that date instead of today |
| `wingspan-cli validate [SOURCE] --as-of YYYY-MM-DD` | the same, for the one-shot report |
| "Load demo book" (menu, or `wingspan-devserver serve --demo`) | pins to the demo book's own day, `2026-08-31` |
| `WINGSPAN_AS_OF=YYYY-MM-DD` | read by the **desktop app's** boot only; an unparseable value is logged and ignored rather than refusing to start, and the effective date is always visible in Data Health |
| `wingspan-devserver serve --legs FILE` | serve that Legs CSV instead of `[trades] csv_path` / `[trades] sheet_url` (with `--demo`: instead of the bundled book) |

`--as-of` moves **two things together**, and that is the point of having one
flag for it: the *valuation date* that DIT, DTE and "has this leg expired" are
measured against, and the *pricing clock* (noon America/New_York on that date)
that the vol surface is solved and priced at. Setting one without the other
gives a leg that is live to the first and expired to the second. With a live
provider it also means the quotes that arrive are interpreted against that
day, not today — so `--as-of` is for asking "what did this look like on…",
not for back-dating a live session.

"Load demo book" (and `wingspan-devserver serve --demo`) pins the same pair
to `2026-08-31` and (the dev server) prints

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
`wingspan-devserver serve --demo --as-of 2026-12-01` overrides the pin; the
`app_config` command's (or `GET /api/v1/config`'s) `resolved.valuation_date`
always reports what won.

`--demo --legs FILE` keeps the pin and the synthetic prices but serves that
CSV instead of the bundled book — how the e2e suite runs the anonymised
fixtures, now `fixtures/trades/legs.csv`. Without `--demo`, `--legs FILE`
simply outranks `[trades] csv_path` / `[trades] sheet_url` for this process
(source `"config"`); like those two keys it is a deployer's choice and cannot
be changed from the app.

## Inspecting the resolved config
The `app_config` command (`GET /api/v1/config` in dev) — shown in the Data
Health card — and `wingspan-cli doctor` show this whole structure with
secrets redacted, plus a `resolved` block: `source: { kind, label, tab,
fetched_at, stale }` in place of the old folder paths, `state_dir`, today's
`valuation_date`, whether the demo book is active, and any provider notes
(e.g. "demo mode: prices are synthetic, not market data"). The sheet id is
never present there, in any form — only the masked `label`. `account_number`
never appears there either — it lives only in the Schwab token file, which
wingspan reads and never writes (`docs/SCHWAB.md`).

## Which book is loaded

**You import a sheet or a CSV, and Wingspan reads it — the sheet over HTTPS,
the CSV where it sits.** Nothing of yours is moved, renamed, changed or
deleted — not when you import a source, not when you clear it, not ever.
Wingspan does keep a copy of the rows it read, inside its own `0600` state
directory (`trade_source_cache.csv`, below), so the book still opens with no
network and no disk; `cache = false` turns that off and Clear deletes it.

Which source that is gets recorded next to the snapshot cache, never in
`wingspan.toml`:

```jsonc
<state_dir>/trade_source.json
{
  "source": { "kind": "sheet", "url": "https://docs.google.com/spreadsheets/d/1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd/edit#gid=123456789", "tab": "Legs" },
  "chosen_at": "2026-09-16T15:04:00Z",
  "recent": [ { "kind": "sheet", "label": "docs.google.com/…/1AbCdE…abcd · Legs", "tab": "Legs" } ]  // cap 5, most recent first
}
```

Written atomically at mode `0600`; an unusable record is dropped and never
fatal.

Precedence for the active source, highest first:

1. `trade_source.json` — what you imported in the app
2. the source the process was started with (the demo book, or `--legs`)
3. `[trades] sheet_url` / `[trades] csv_path`
4. nothing at all

**Nothing configured and nothing chosen is not an error.** It is the
ordinary state of a fresh install: `kind` is `"empty"`, `label` is `null`,
the dashboard is empty, and the app starts and says so.

**"Load demo book" is the one exception, and deliberately so: it ignores the
saved source entirely** — it starts a run that does not read
`trade_source.json` at all, because pricing your real positions off
synthetic fixtures would be a lie the app told you from a menu item. "Leave
demo" puts the saved source straight back.

A saved source that cannot be reached is **dropped from display, never from
the record**: the cached book is published with `stale: true`, the reason
goes in `provider_notes` (so `app_config` and the UI show it), and
`wingspan-cli doctor` prints it on the Trades section's `source` line —
because the network may be back tomorrow.

No user-typed string is ever fetched verbatim: Rust extracts the spreadsheet
id from an `/edit` link, an export/gviz URL, a published `/d/e/<pubid>/pub`
link or a bare id and builds the request URL itself, and only
`docs.google.com` is reachable; anything else answers `invalid-trade-source`.
The webview never talks to Google at all — `tauri.conf.json`'s CSP
`connect-src` forbids it.

### Errors you can act on

| status | slug | when |
|---|---|---|
| 422 | `sheet-not-shared` | the fetch was redirected to `accounts.google.com`, or answered 401/403 — the sheet is not shared with anyone-with-the-link |
| 422 | `sheet-tab-not-found` | the fetch answered 200 with gviz's own error payload — no tab of that name in that spreadsheet |
| 422 | `sheet-unreachable` | a network error, a timeout, a 5xx, or a response past the size limit; the previous book stays on screen |
| 422 | `invalid-trade-source` | a URL or path Wingspan will not use (bad shape, a host that is not `docs.google.com`, a CSV path the app's own picker never confirmed), a spreadsheet id `docs.google.com` answers `404` for, a CSV it cannot parse, or a required column the header does not carry |

## Importing a trade source

This is how a book gets loaded, and it is a two-step choice, not a single
picker action. The Trade source control opens a dialog that takes a sheet
link (or **Pick a CSV file…**); **Check** probes it and shows which columns
were found, which expected ones are absent, which required ones are
missing and which are ignored — **before** anything changes — alongside the
sharing and tab sentences. **Load & analyze** then commits. See
`docs/DESKTOP.md` §5 for the full walkthrough.

```
<state_dir>/trade_source.json        0600   what you imported, plus recent
<state_dir>/trade_source_cache.csv   0600   the bytes last read (fetched, or off the CSV)
<state_dir>/trade_source_cache.json  0600   { source, fetched_at, bytes, sha256 }
```

These three files, inside the `0700` state directory, are the only things
written.

Two consequences worth knowing:

* **It is a snapshot, not a live feed.** Nothing watches the sheet or the CSV
  for changes: the book refreshes on Reload, on a source change, and on
  `[trades] poll_secs` if you set one. Boot publishes the cached book first
  and refetches in the background, emitting `trades_changed { "reason":
  "fetch" }` only when the digest changed.
* **Clear stops showing the trades. It deletes nothing of yours (D23).**
  `clear_trade_source` forgets the pointer, deletes Wingspan's own cached copy
  (`trade_source_cache.csv` + `.json`) and drops the cached market snapshot
  (whose quote universe was that book's own contracts). **No byte of your
  sheet or your CSV file is read, moved, renamed or removed** — the whole
  command touches only `<state_dir>`. The cleared source goes to the front of
  `recent`, so loading it again is one click. Clearing with nothing loaded is
  a success that changes nothing.

The daily index closes in `closes.json` are unaffected by any of this: they
are SPX levels from the market, not anything read out of your sheet or CSV.
