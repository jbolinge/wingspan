# Deployment

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
  `Daily Trade Statement` (or leave it unset and pick the folder from the
  dashboard's **Trades** control later).
- Leave `bind = "127.0.0.1:8787"` — see the security notes below before
  binding anything else.

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
`/srv/schwab/…`, say — is blocked by `ProtectSystem=strict` for reading too, so
add its directory to `ReadOnlyPaths=` in the copied unit file. Granting write
access there would be actively wrong: three other applications own that file.

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

## Security & permissions summary

- The server binds loopback by default. Binding beyond loopback is refused
  unless `[server] allow_remote = true` **and** `auth_token` is set — the
  page shows your whole book. Prefer an SSH tunnel
  (`ssh -L 8787:127.0.0.1:8787 server`) or a reverse proxy with its own auth
  for remote access. A loopback bind answers only to loopback `Host` names
  (403 `untrusted-host` otherwise), so **a proxy that forwards its own `Host`
  needs that name in `[server] allowed_hosts`** — otherwise every proxied
  request is refused. A proxy configured to rewrite `Host` to `127.0.0.1:8787`
  needs no key at all. See `docs/CONFIG.md`.
- Config and env file: `0600`, owned by the service user. State/config dirs:
  `0700`. The Schwab token file should be `0600` too, but it is owned by the
  tooling that produces it, so `wingspan doctor` only *warns* about a looser
  mode — fix it at the source.
- Tokens are never logged; `GET /api/v1/config` redacts secrets. The Schwab
  token file is **read-only to wingspan**: it is opened for reading and never
  written, created, renamed, chmodded, locked, copied or backed up, and it
  needs no write access in the unit. A refreshed access token exists only in
  the running process's memory; nothing about it reaches disk.
- State (snapshot cache, uploaded trade snapshots, bridge script) lives
  under `$XDG_STATE_HOME/wingspan` (default `~/.local/state/wingspan`). The
  uploads tree there holds a whole copy of your book, so the app creates every
  directory in it `0700` **itself** rather than relying on `UMask=0077`: a
  manual `wingspan serve`, a container, or a `state_dir` on a shared volume is
  as private as the systemd unit is.

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
