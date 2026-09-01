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

(With `just` installed, `just release` runs the same two steps.)

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

**Schwab (live quotes)** — `docs/SCHWAB.md` has the full walk-through:
generate the token file with schwab-py, set `[schwab] token_path`, and put
the app key/secret where the service can read them:

```bash
umask 077
cat > ~/.config/wingspan/schwab.env <<'EOF'
SCHWAB_APP_KEY=...
SCHWAB_APP_SECRET=...
EOF
chmod 600 ~/.config/wingspan/schwab_token.json   # doctor checks this
```

Then uncomment the `EnvironmentFile=` line in the unit file (step 6). If
other tooling already owns the secrets and the token, copy `.env.example` to
`.env` and point `WINGSPAN_SCHWAB__ENV_FILE` / `WINGSPAN_SCHWAB__TOKEN_CONFIG`
at those files instead (`docs/CONFIG.md`).

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
                                  # (presence, schema, 0600 perms, token age);
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
  for remote access.
- Config, env file and token file: `0600`, owned by the service user
  (`wingspan doctor` checks the token file). State/config dirs: `0700`.
- Tokens are never logged; `GET /api/v1/config` redacts secrets; token files
  are rewritten atomically with a timestamped backup.
- State (snapshot cache, uploaded trade snapshots, bridge script) lives
  under `$XDG_STATE_HOME/wingspan` (default `~/.local/state/wingspan`).

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
