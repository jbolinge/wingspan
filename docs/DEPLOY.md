# Deployment

> **Retired.** This describes the web deployment Wingspan no longer ships;
> the product is the desktop app. Kept for history until the docs rewrite
> removes it.

One binary (SPA embedded) + one config file, run by systemd as user
`jbolinge`, starting at boot and auto-restarting. Steps assume Debian/Ubuntu
x86_64; build on the same architecture and glibc generation as the server
(building on the server itself is simplest).

## 1. Provision the build machine

Skip anything already installed.

```bash
# Build essentials + python for the yfinance bridge venv
sudo apt-get update
sudo apt-get install -y build-essential pkg-config git curl python3 python3-venv

# Rust (rustup respects rust-toolchain.toml in the repo)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"

# Node via nvm (the SPA build wants a current LTS)
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"
nvm install --lts
```

## 2. Build

```bash
git clone git@github.com:jbolinge/wingspan.git && cd wingspan
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"   # if node is nvm-managed
cd web && npm ci && npm run build && cd ..
cargo build --release --features embed-ui
# → target/release/wingspan
```

(With `just` installed, `just release` does the same thing from a fresh
clone: its `web-deps` dependency runs `npm ci` when `web/node_modules` is
missing or older than the lockfile, then it builds the SPA and the release
binary. It does not source nvm for you — keep line 2 above.)

## 3. Install on the server

As the deploy user (`jbolinge`):

```bash
# The binary
install -Dm755 target/release/wingspan ~/.local/bin/wingspan

# The dirs the service is allowed to write — must exist before the first
# systemd start (the unit's ReadWritePaths name them)
install -dm700 ~/.config/wingspan ~/.local/state/wingspan

# The config
install -m600 wingspan.example.toml ~/.config/wingspan/wingspan.toml
```

Edit `~/.config/wingspan/wingspan.toml`:

- `[trades] root` → the folder holding `Open Trades` / `Closed Trades` /
  `Daily Trade Statement`, **if** this machine is meant to hold your trade
  files. Ordinarily leave it unset: the dashboard's **Trades** control
  uploads the folder from the computer you are browsing from, and **clear
  trades** deletes that copy again, so nothing of your book stays here
  between sessions. With it unset and nothing uploaded the service starts
  with an empty book and says so. It is a deployer's setting either way —
  nothing in the UI can change it, and no route lists this machine's folders.
- `bind` stays `"127.0.0.1:8787"` unless the browser is on another machine.
  If it is, read **"Behind HAProxy on the LAN"** below — a non-loopback bind
  is refused at start-up until you have configured a gate.

## 4. Data sources (optional but recommended)

**Schwab (live quotes)** — `docs/SCHWAB.md` has the full walk-through.
Wingspan does **not** create or manage the Schwab token: it is produced and
refreshed by your existing login tooling (schwab-py's `client_from_login_flow`,
a cron login job) and is typically shared with the other applications on this
server. Wingspan is given its path, opens it read-only, and never writes,
moves, chmods, copies or backs it up. A refreshed access token lives only in
the running process.

So step 4 is two pointers, not a setup:

```bash
# 1) Where the token already is (nothing is created here):
#    [schwab] token_path = "/srv/schwab/schwab_token.json"
#    ...or leave it to whatever config file your other tooling already keeps:
#    [schwab] token_config = "/srv/schwab/paths.toml"

# 2) The app key/secret, readable by the service user:
umask 077
cat > ~/.config/wingspan/schwab.env <<'EOF'
SCHWAB_APP_KEY=...
SCHWAB_APP_SECRET=...
EOF
```

Then uncomment the `EnvironmentFile=` line in the unit file (step 6). If
other tooling already owns the secrets and the token, copy `.env.example` to
`.env` and point `WINGSPAN_SCHWAB__ENV_FILE` / `WINGSPAN_SCHWAB__TOKEN_CONFIG`
at those files instead (`docs/CONFIG.md`).

The service user needs **read** access to the token file and its directory —
nothing more. If the file's mode is looser than `0600`, `doctor` warns and
names it; tighten it where the file is produced, not here.

**yfinance (delayed fallback)** — the bridge script is embedded in the
binary and self-installs into the state dir; you only provide an interpreter
with `yfinance`:

```bash
python3 -m venv ~/.local/share/wingspan/venv
~/.local/share/wingspan/venv/bin/pip install yfinance==1.7.0  # pin from bridge/requirements.txt
# then in wingspan.toml:  [yfinance] python = "~/.local/share/wingspan/venv/bin/python"
```

## 5. Verify before daemonizing

```bash
~/.local/bin/wingspan doctor      # config, dirs, python/yfinance, token file
                                  # (presence, schema, perms, token age — all
                                  # read-only; it never repairs the token);
                                  # add --probe to also resolve providers once
~/.local/bin/wingspan validate ~/Trades   # trades, P&L, reconciliation
~/.local/bin/wingspan serve       # → http://127.0.0.1:8787, Ctrl-C when happy
```

Fix everything `doctor` flags before moving on.

## 6. systemd: start at boot, restart on crash

`docs/wingspan-system.service` is a system-wide unit that runs as
`User=jbolinge` with `Restart=always`, starts at boot with no login, and is
sandboxed: `UMask=0077` keeps everything it writes private, the filesystem is
read-only except the state and config dirs, and privilege escalation is off
(`NoNewPrivileges`). If your deploy user differs, edit the three `jbolinge`
paths plus `User=`/`Group=` before installing.

The Schwab token directory does **not** belong in `ReadWritePaths=`. Wingspan
only reads that file, so read access is enough — and under `ProtectHome=read-only`
a token anywhere below the user's home is already readable with no unit change
at all. A `[schwab] token_path` (or `token_config`) outside the home tree —
`/srv/schwab/…`, say — is readable under this unit as well: `ProtectSystem=strict`
only takes write access away, it does not hide reads. Uncommenting
`ReadOnlyPaths=/srv/schwab` in the copied unit is optional belt-and-braces, not
something wingspan needs in order to see the file. Granting *write* access
there would be actively wrong: three other applications own that file.

```bash
sudo cp docs/wingspan-system.service /etc/systemd/system/wingspan.service
# If Schwab is configured, uncomment EnvironmentFile= in the copy:
sudo systemctl daemon-reload
sudo systemctl enable --now wingspan
systemctl status wingspan                 # active (running)?
journalctl -u wingspan -f                 # logs
```

Reboot once and confirm it came back: `systemctl status wingspan`.

**Alternative — user service** (no root, but only starts at boot if
lingering is on): `docs/wingspan.service`:

```bash
mkdir -p ~/.config/systemd/user
cp docs/wingspan.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wingspan
loginctl enable-linger "$USER"     # start at boot / keep running logged out
journalctl --user -u wingspan -f   # logs
```

Install one or the other, not both — they'd fight over the port.

## Behind HAProxy on the LAN

The dashboard shows your whole book to anyone who can open it, so the server
refuses a non-loopback bind until something is gating the requests. The gate
that works for a *browser* is `lan_only`: the SPA has no way to send a bearer
token, so `auth_token` only helps scripts.

The shape this is built for: HAProxy on another box terminates TLS, is itself
reachable only from the LAN, and forwards to wingspan over plain HTTP.

**wingspan** (`~/.config/wingspan/wingspan.toml`):

```toml
[server]
# Or the machine's own LAN address, e.g. "192.168.1.50:8787" — then you can
# skip listing it in allowed_hosts below.
bind = "0.0.0.0:8787"
allow_remote = true

# The gate: every request must come from the LAN.
lan_only = true
# Optional. Empty = the built-in private set (10/8, 172.16/12, 192.168/16,
# 127/8, ::1, fe80::/10, fc00::/7). Tailscale/CGNAT (100.64.0.0/10) is NOT in
# it — list it here if you use Tailscale. An explicit list replaces the
# built-in one.
lan_networks = ["192.168.1.0/24"]

# Only this peer may name a client through X-Forwarded-For. Anything else that
# reaches the port speaks only for itself, whatever headers it sends.
trusted_proxies = ["192.168.1.10"]

# `0.0.0.0` names every interface and therefore no host, so the name the
# browser actually asks for has to be listed or every request is 403
# untrusted-host. Include the LAN address too if you sometimes bypass the proxy.
allowed_hosts = ["wingspan.example.com", "192.168.1.50"]
```

**HAProxy** (`/etc/haproxy/haproxy.cfg` on the proxy box):

```haproxy
frontend wingspan_https
    bind 192.168.1.10:443 ssl crt /etc/haproxy/certs/wingspan.pem
    # LAN-only at the proxy as well — defence in depth, not a substitute for
    # lan_only: this list is the proxy's, wingspan enforces its own.
    acl from_lan src 192.168.1.0/24
    http-request deny unless from_lan
    default_backend wingspan_app

backend wingspan_app
    # SET, not append: one value, from the connection HAProxy actually has.
    # `option forwardfor` appends to whatever the client sent, leaving an
    # attacker-controlled entry in the chain. Wingspan reads the chain
    # right-to-left and so survives either, but there is nothing to reason
    # about with set-header.
    http-request set-header X-Forwarded-For %[src]
    # The name wingspan must be told about (`allowed_hosts` above). Leave it
    # alone to pass the frontend's Host through, or rewrite it here.
    server wingspan 192.168.1.50:8787 check
```

The SSE stream is a long-lived response; give the backend a `timeout tunnel`
(or a generous `timeout server`) so HAProxy does not cut it every minute.

**Firewall the port to the proxy.** `lan_only` is a gate, not a substitute for
one — anything that can open a TCP connection to `:8787` from a LAN address is
inside it:

```bash
sudo ufw allow from 192.168.1.10 to any port 8787 proto tcp
sudo ufw deny 8787/tcp
```

Then check it from the server:

```bash
~/.local/bin/wingspan doctor    # prints bind, gate, lan networks, trusted proxies
```

`doctor` fails the `bind` line with the same sentence `serve` would refuse to
start with, and warns when `allowed_hosts` is empty on a `0.0.0.0` bind.

## Security & permissions summary

- The server binds loopback by default. Binding beyond loopback is refused
  unless `[server] allow_remote = true` **and** a gate is set — the page shows
  your whole book. The gates are `lan_only = true` (the client's address must
  be on the LAN; the only one a browser can satisfy) and `auth_token` (a bearer
  on every `/api` request; for scripts). An SSH tunnel
  (`ssh -L 8787:127.0.0.1:8787 server`) still needs neither. See "Behind
  HAProxy on the LAN" above and `docs/CONFIG.md`.
- With `lan_only`, `X-Forwarded-For` is read **only** from a peer listed in
  `[server] trusted_proxies`; from anyone else it is ignored, so a host off the
  LAN cannot claim to be on it. A trusted proxy that forwards nothing is
  refused rather than believed (403 `missing-forwarded-for`). Keep
  `trusted_proxies` to the exact proxy addresses and firewall the port to them.
- The `Host` header is checked on **every** bind (403 `untrusted-host`
  otherwise). Loopback names and the literal `bind` address are accepted
  automatically — but `0.0.0.0` names no host, so **the LAN address or the
  proxy's public name must be in `[server] allowed_hosts`** or every request is
  refused. `doctor` warns when that is the case.
- Config and env file: `0600`, owned by the service user. State/config dirs:
  `0700`. The Schwab token file should be `0600` too, but it is owned by the
  tooling that produces it, so `wingspan doctor` only *warns* about a looser
  mode — fix it at the source.
- Tokens are never logged; `GET /api/v1/config` redacts secrets. The Schwab
  token file is **read-only to wingspan**: it is opened for reading and never
  written, created, renamed, chmodded, locked, copied or backed up, and it
  needs no write access in the unit. A refreshed access token exists only in
  the running process's memory; nothing about it reaches disk.
- State (snapshot cache, the uploaded trade snapshot, bridge script) lives
  under `$XDG_STATE_HOME/wingspan` (default `~/.local/state/wingspan`). The
  uploads tree there holds a whole copy of your book, so the app creates every
  directory in it `0700` **itself** rather than relying on `UMask=0077`: a
  manual `wingspan serve`, a container, or a `state_dir` on a shared volume is
  as private as the systemd unit is.
- **Uploaded trades are transient.** They live in `<state_dir>/uploads/` only
  while they are being analysed. The dashboard's **clear trades** deletes the
  whole tree, the `trades_root.json` pointing at it and the cached market
  snapshot derived from it, so nothing taken from your files (trade ids,
  symbols, execution refs, account numbers, P&L) is left on the server. A
  process killed mid-upload cannot clean up after itself, so the next start
  sweeps whatever it abandoned. `closes.json` survives a clear: it holds SPX
  index closes from the market, not anything read out of your book.

## Upgrades

```bash
cd wingspan && git pull
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"
(cd web && npm ci && npm run build) && cargo build --release --features embed-ui
install -Dm755 target/release/wingspan ~/.local/bin/wingspan
sudo systemctl restart wingspan           # or: systemctl --user restart wingspan
```

The config format is additive; new keys get defaults and appear in
`wingspan.example.toml`.
