# Deployment

One static-ish binary + one config file. Build on the target architecture
(x86_64 Linux assumed below).

## Build

```bash
export NVM_DIR="$HOME/.nvm" && . "$NVM_DIR/nvm.sh"   # if node is nvm-managed
cd web && npm ci && npm run build && cd ..
cargo build --release --features embed-ui
# → target/release/wingspan
```

## First-run checklist

1. `install -Dm755 target/release/wingspan ~/.local/bin/wingspan`
2. `cp wingspan.example.toml ~/.config/wingspan/wingspan.toml` and edit:
   `[trades] root` → your Trades folder; leave `bind = "127.0.0.1:8787"`.
3. Schwab (optional, live data): see `docs/SCHWAB.md` — generate the token
   file, export the app key/secret env vars, set `[schwab] token_path`.
4. yfinance fallback (optional, delayed data):
   ```bash
   python3 -m venv ~/.local/share/wingspan/venv
   ~/.local/share/wingspan/venv/bin/pip install -r bridge/requirements.txt
   # set [yfinance] python to that venv's python
   ```
5. `wingspan doctor` — verifies config, directories, python/yfinance, token
   file (including permissions), network. Fix everything it flags.
6. `wingspan validate <trades-root>` — confirm trades, P&L, warnings and the
   reconciliation table look right before trusting the dashboard.
7. `wingspan serve` → http://127.0.0.1:8787

## systemd (user service)

`docs/wingspan.service` — install with:

```bash
mkdir -p ~/.config/systemd/user
cp docs/wingspan.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now wingspan
loginctl enable-linger "$USER"     # keep it running when logged out
journalctl --user -u wingspan -f   # logs
```

## Security notes

- The server binds loopback by default. Binding beyond loopback is refused
  unless `[server] allow_remote = true` **and** `auth_token` is set — the
  page shows your whole book. Prefer an SSH tunnel or a reverse proxy with
  its own auth for remote access.
- Tokens are never logged; `GET /api/v1/config` redacts secrets.
- State (cached snapshots) lives under `$XDG_STATE_HOME/wingspan` (default
  `~/.local/state/wingspan`).
- Keep the token file `0600`; `wingspan doctor` checks.

## Upgrades

`git pull && just release`, replace the binary, restart the service. The
config format is additive; new keys get defaults and appear in
`wingspan.example.toml`.
