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
| `root` | — (optional) | folder holding the three subfolders; discovery descends one level. Leave it unset for an upload-only install — see "Which book is loaded" |
| `open_dir` | `"Open Trades"` | subfolder name, case-insensitive |
| `closed_dir` | `"Closed Trades"` | |
| `statements_dir` | `"Daily Trade Statement"` | |
| `statement_timezone` | `"America/Chicago"` | statement times are Central |

## [server]
| key | default | meaning |
|---|---|---|
| `bind` | `"127.0.0.1:8787"` | non-loopback refused unless `allow_remote` **and** a gate are set |
| `allow_remote` | `false` | the opt-in a non-loopback bind needs |
| `auth_token` | unset | one gate: bearer token required on `/api` when remote |
| `lan_only` | `false` | the other gate: every request must come from `lan_networks` |
| `lan_networks` | `[]` | IPs/CIDRs that count as the LAN; empty = the built-in private set |
| `trusted_proxies` | `[]` | IPs/CIDRs whose `X-Forwarded-For` names the client |
| `allowed_hosts` | `[]` | extra `Host`/`Origin` names this instance answers to |

### The bind policy

A loopback bind needs nothing. A **non-loopback** bind is refused at start-up
unless `allow_remote = true` *and* at least one gate is in force:

| gate | what it checks | who it is for |
|---|---|---|
| `auth_token = "…"` | `Authorization: Bearer` on every `/api` request | scripts, `curl`, another service |
| `lan_only = true` | the client's IP address is inside `lan_networks` | **a browser** — the SPA has no way to send a bearer |

Both may be set; either alone is enough. `wingspan doctor` prints which one is
in force, and the refusal sentence names both ways out.

### `lan_only`, `lan_networks`, `trusted_proxies`

With `lan_only = true` **every** request — `/api`, the SSE stream and the SPA
itself — is refused with **403 `outside-lan`** unless the client's address is
inside `lan_networks`. Left empty, `lan_networks` means the built-in private
set:

```
10.0.0.0/8   172.16.0.0/12   192.168.0.0/16   127.0.0.0/8
::1/128      fe80::/10       fc00::/7
```

`100.64.0.0/10` — carrier-grade NAT, which is also what **Tailscale** hands
out — is deliberately *not* in that set; add it explicitly if you want it. An
explicit `lan_networks` **replaces** the built-in list rather than adding to
it, so include loopback yourself if you still want it. Entries are IPs or
CIDRs; a bare address is its own `/32` (`/128`). Anything that is not one is a
config error naming the key, and the server refuses to start.

#### How the client address is resolved

1. The **TCP peer** — what the kernel reports, which nobody can forge. An
   IPv4-mapped IPv6 peer (`::ffff:192.168.1.20`, what a dual-stack listener
   reports for an IPv4 connection) is folded back to IPv4 first.
2. If that peer is **not** in `trusted_proxies`, it *is* the client and any
   `X-Forwarded-For` header is ignored outright. A host on the internet cannot
   promise it is on your LAN.
3. If the peer **is** a trusted proxy, `X-Forwarded-For` is walked **right to
   left** — the rightmost entry is the one the nearest proxy appended, and
   therefore the only one it vouches for — skipping any entry that is itself a
   trusted proxy. The first address left is the client. Ports (`10.0.0.9:53124`,
   `[2001:db8::1]:443`) and quotes are handled; repeated headers are one list.
4. A trusted proxy that forwards nothing usable is **refused**, not believed:
   403 `missing-forwarded-for`. An entry that is not an address (RFC 7239's
   `unknown` or `_obfuscated`) stops the walk with 403 `untrusted-proxy`,
   rather than being stepped over to reach whatever is to its left.

`trusted_proxies` is therefore the whole security boundary of the header: keep
it to the exact addresses of your reverse proxies, and firewall the port so
nothing else can reach it directly.

#### HAProxy

Configure HAProxy to **overwrite** the header rather than append to it:

```haproxy
http-request set-header X-Forwarded-For %[src]
```

`option forwardfor` *appends* the source address to whatever the client sent,
which leaves an attacker-controlled value to the left of the real one. That is
safe here — the right-to-left walk takes the address HAProxy appended and
ignores the client's claim — but `set-header` is one value instead of a chain
and leaves nothing to reason about. Use `option forwardfor` only if something
downstream needs the full chain.

Full deployment example, including the frontend and the firewall rule:
`docs/DEPLOY.md`, "Behind HAProxy on the LAN".

### `allowed_hosts`

Loopback is not a boundary a browser respects: a page on `evil.example` whose
DNS record is re-pointed at `127.0.0.1` reaches this process same-origin, with
your whole book behind it. What such a page cannot forge is the *name* it asks
for, so the server answers only to names it knows are its own and refuses
everything else with **403 `untrusted-host`** before a route or even the SPA is
reached.

The names it knows are its own without being told:

- loopback names — `localhost` and any loopback IP;
- the literal address in `bind`, when it is a real one. `0.0.0.0` and `[::]`
  name *every* interface and therefore no host, so **this machine's own LAN
  address is not known to it** — nor is the public name HAProxy answers on.

`allowed_hosts` is the list of extra names that are also yours: the LAN address
you browse to, the name on the proxy's certificate, an `/etc/hosts` alias.

```toml
[server]
allowed_hosts = ["wingspan.example.com", "192.168.1.50", "trades.lan"]
```

A port in an entry is ignored (only the host name is compared,
case-insensitively). The list applies on **every** bind, loopback or not. It
used to be waved through beyond loopback, on the grounds that the bearer token
was the gate there — but `lan_only` gates the client's *address*, which a
rebound name satisfies perfectly (a LAN browser pointed at `evil.example` is
still a LAN browser), so the name is now checked everywhere. The same list is
what a state-changing request's `Origin` is checked against (**403
`cross-origin`** otherwise); an `https://` origin from a TLS-terminating proxy
matches on its host name exactly like an `http://` one.

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

Trade files are **not stored on the server**. They are uploaded from the
browser, analysed, and deleted again when the user clears them — so there is
no way to point a running server at a folder on its own disk, and no route
that lists one. `[trades] root` remains a *deployer's* setting for a machine
that does hold the files (and for `wingspan validate`, `serve --root` and
`serve --demo`); it cannot be changed at runtime.

Which book is in force is recorded next to the snapshot cache, never in
`wingspan.toml`:

```
<state_dir>/trades_root.json  {"root": "…/uploads/current",
                               "uploaded_at": "2026-08-31T21:04:00Z"}
```

Precedence for the active root, highest first:

1. `trades_root.json` — the book uploaded from the browser
   (`POST /api/v1/trades/upload`)
2. the root the process was started with (`serve --demo`'s book, or `--root`)
3. `[trades] root`
4. nothing at all

`GET /api/v1/trades/root` reports which won as `source` (`"uploaded"`,
`"demo"`, `"config"`, `"empty"`). `--demo` chooses where *prices* come from,
not which book is loaded, so a demo run still adopts an upload — the source
stops being `"demo"` as soon as one arrives.

**Nothing configured and nothing uploaded is not an error.** It is the
ordinary state of an upload-only install: `source` is `"empty"`, `root` is
`null`, the dashboard is empty, and `wingspan serve` starts and says so. The
folder is discovered with the same rules as `[trades] root` (case-insensitive
names from `open_dir` / `closed_dir` / `statements_dir`, one level of
descent), and adopting a book reloads it, re-solves the statement source and
moves the file watcher exactly as a restart would.

An uploaded copy that has since been swept, deleted or emptied is **dropped,
never fatal**: the server falls back to `[trades] root` (or to nothing), logs
a warning and publishes it in `provider_notes` (so `GET /api/v1/config` and
the UI show it). `wingspan doctor` prints the same thing on an
`uploaded book` line. A `trades_root.json` written by an older version that
still had the server-side folder picker (no `uploaded_at`) is ignored the
same way rather than re-adopting a folder on this machine.

## The trades folder on the viewing computer

This is how a book gets loaded. The Trades control reads the folder you pick
*in the browser* and uploads a snapshot (`POST /api/v1/trades/upload`); the
server keeps it in the state directory beside everything else, and only until
you clear it:

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
* **Clear deletes the copy** (`DELETE /api/v1/trades/root`). The whole
  `<state_dir>/uploads` tree goes — the copy in force, any staging directory,
  and anything a killed process left under a `.trash-` name — along with
  `trades_root.json` and the cached market snapshot, whose quote universe is
  the uploaded book's own contracts. The book then falls back to `[trades]
  root`, to `--demo`'s book, or to nothing; with nothing to fall back to the
  clear still succeeds, because the point of it is that the files go.
  Nothing derived from them (trade ids, symbols, execution refs, account
  numbers, P&L) is left under the state directory afterwards. The daily index
  closes in `closes.json` stay: they are SPX levels from the market, not
  anything read out of your files.
