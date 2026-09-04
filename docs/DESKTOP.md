# Wingspan desktop

Everything you need to install, run and configure Wingspan on your own
machine, with nothing else read first.

## 1. What Wingspan is

Wingspan is a desktop application: one window, on one machine, that reads
your thinkorswim trade files and account statements from a folder you point
it at — **in place**, on this computer's own disk. Nothing is uploaded,
nothing is copied onto a server, and there is no server: the process that
draws the window is the same process that parses your trades and prices
them. Ubuntu 26.04 LTS is the only platform this is built and shipped for.

## 2. Prerequisites

The `.deb` declares three runtime dependencies and installs them for you if
they are missing:

- `libwebkit2gtk-4.1-0` — the web renderer the window is built from
- `libgtk-3-0` — the native window, menu and file choosers
- `xdg-desktop-portal` — the folder/file picker dialogs go through this

You do not need Rust, Node or Python to *run* the app — only to build it
from source (§13).

## 3. Install

1. Download `wingspan_<version>_amd64.deb` and `SHA256SUMS` from the
   [Releases page](https://github.com/jbolinge/wingspan/releases).
2. Verify the download:
   ```sh
   sha256sum -c SHA256SUMS
   ```
3. Install it:
   ```sh
   sudo dpkg -i wingspan_*.deb
   ```
   If `dpkg` reports missing dependencies:
   ```sh
   sudo apt-get -f install
   ```

The `.deb` is **unsigned** — there is no code-signing key and no updater.
`SHA256SUMS`, published alongside every release, is the whole integrity
check: it proves the file you installed is the file the release build
produced, nothing more. "Check for updates" (in the Help menu) opens the
Releases page in your browser; it does not download or install anything
itself.

## 4. First run

Launch **Wingspan** from your application menu (or run `wingspan` in a
terminal). The window opens dark, 1600×1000, centered.

With nothing configured yet, a folder dialog opens once, offering two
choices:

- **Choose folder…** — pick the folder holding your trade files (§5).
- **Load demo book** — a bundled, wholly synthetic book, so you can see the
  dashboard working before pointing it at anything real.

## 5. Choosing a trades folder

Open the **Trades** menu (or the status bar's Trades control) → **Choose
folder…**. This is a two-step choice:

1. **Probe.** The OS folder chooser hands back a path; Wingspan looks at
   what is in it — how many open trades, closed trades and statements — and
   shows you a summary, without loading anything yet. If your files are one
   level down (say, you picked the parent of the actual export), the probe
   says so.
2. **Load & analyze.** Only this step commits: the folder becomes the active
   book, the market data provider chain is re-solved, and a refresh is
   requested.

Wingspan is looking for three sub-folders, case-insensitively, either
directly inside the folder you chose or one level further down:

```
Open Trades/
Closed Trades/
Daily Trade Statement/
```

Once loaded, the folder is watched **live**: saving a trade file, or moving
one from `Open Trades` to `Closed Trades`, reaches the dashboard on its own
after a short quiet period (`watch_debounce_ms`, default 750 ms), using the
operating system's own file-change notifier.

That notifier does not see everything, though. A folder synced by Dropbox,
Syncthing, or mounted over SMB/NFS may never generate the events the
notifier expects, because the write happens somewhere else and only
reappears on disk. For a folder like that, set `[trades] watch_poll_secs` in
`wingspan.toml` to poll it every N seconds instead. And on Linux, a large
enough trades folder tree can exhaust the kernel's inotify watch budget; if
Data Health reports the folder is not being watched and polling is not
configured, raise the limit with:

```sh
sysctl fs.inotify.max_user_watches
```

(as root, to change it: `sudo sysctl -w fs.inotify.max_user_watches=<a bigger number>`).

## 6. Clear

**"Clear trades" stops Wingspan showing the trades, and never deletes
anything.** The folder is not touched — not one file, not one byte — and it
stays one click away in the recent-folders list, so loading it again costs
nothing.

## 7. Schwab

Wingspan can price your book against live Schwab market data, but it never
creates, logs in for, or manages the Schwab token itself — that is the job
of whatever login tooling you already use (schwab-py's
`client_from_login_flow`, a cron login job, or another application on this
machine). Wingspan is only ever handed a path.

**The token file arrives via a picker, and Wingspan reads it and never
writes it.** In Settings → Market data, **Browse…** opens a native file
chooser; after you pick a file, a confirmation dialog names the full path
and states plainly what Wingspan is about to do with it ("Wingspan will
read (never write) your Schwab token from: …"). Only after you confirm is
the path string written to `wingspan.toml` — the token file itself is never
opened, copied, chmoded or created by that action. If the token file's
permissions are wider than its owner (not `0600`), Wingspan shows a warning
naming the file and says to tighten it where it is produced; it never
changes the mode itself.

If your app key and secret live in a separate `KEY=VALUE` file owned by
other tooling, point Wingspan at it the same way: Settings → Market data →
**Credentials file** → Browse… (another native picker with its own
confirmation). The two environment variable *names* Wingspan reads out of
that file (`app_key_env`, `app_secret_env`) are editable text fields — the
*values* never leave the file and never reach the settings form.

## 8. The Python bridge (yfinance)

When Schwab is not configured, expired, or degraded, Wingspan falls back to
a Yahoo-Finance-backed Python bridge for delayed quotes — the middle rung of
the fallback chain (Schwab → yfinance → cached snapshot → statement marks).
**It is entirely optional.** With no working interpreter, Wingspan says so
("Delayed quotes unavailable — you lose only the middle rung") and keeps
going on the cache and the statement marks.

To enable it, create a virtual environment once and point Wingspan at it:

```sh
python3 -m venv ~/.local/share/wingspan/venv
~/.local/share/wingspan/venv/bin/pip install yfinance==1.7.0
# then Settings → Market data → interpreter → Browse… →
# ~/.local/share/wingspan/venv/bin/python3, and press Test
```

The bridge script itself is embedded in the `wingspan` binary and
self-installs into the state directory the first time it runs — nothing to
deploy for it beyond the interpreter.

## 9. Where things live

| what | path |
|---|---|
| config | `~/.config/wingspan/wingspan.toml` |
| state | `~/.local/share/com.jbolinge.wingspan/state/` (mode `0700`): `snapshot.json`, `closes.json`, `trades_root.json`, the self-installed `yfinance_bridge.py`, `demo/` |
| logs | `~/.local/share/com.jbolinge.wingspan/logs/` — daily-rolled, 7 kept; honours `RUST_LOG` |
| window geometry | `~/.config/com.jbolinge.wingspan/` |
| the bundled demo book | `/usr/lib/wingspan/fixtures/demo` |

## 10. Diagnostics

The **Diagnostics** card, in the Data Health tab (or Help → Diagnostics),
shows the same checklist a terminal `wingspan-cli doctor` prints — config
file found, trades folder resolved, state directory writable, the yfinance
interpreter, the Schwab token file's presence/age/permissions, and each
provider's reachability. Its three buttons:

- **Run checks** — re-runs the checklist, this time reaching out to the
  configured providers (the CLI's `--probe`).
- **Reveal log folder** — opens the log directory in your file manager.
- **Open config file** — opens `wingspan.toml` (or, if none exists yet, the
  folder it would be created in).

`wingspan-cli doctor` prints the identical rows outside the app — useful
over SSH, or before the window has opened at all.

## 11. First-boot migrations

If you are upgrading from the earlier LAN-served build, two one-time
migrations run on first launch:

- **The state directory moved.** The old build kept its state under
  `~/.local/state/wingspan`; this one keeps it under
  `~/.local/share/com.jbolinge.wingspan/state/`. On first boot, Wingspan
  copies `closes.json` (and `snapshot.json`, if present) across — never
  `uploads/**`, never `trades_root.json` — and writes a `.migrated-from`
  marker. **The old directory is left exactly as it was**, untouched.
- **The old upload-only build's copy is set aside, never deleted.** That
  build kept an uploaded copy of your book in
  `<old state dir>/uploads`. It may be the only copy left of a book you
  once uploaded from a machine you no longer have, so Wingspan **renames**
  it — never deletes it — to `uploads.discarded-<YYYYMMDD>` in the same
  directory, and tells you where it went. The Diagnostics card lists any
  such directory and offers **Delete the old uploaded copy**, which asks
  for confirmation naming the exact path before permanently removing it.
  Nothing under your own trades folder is ever touched by this.

## 12. Ambient authority

Wingspan runs with your full user account's ambient authority — it can read
and write anything you can, exactly like any other desktop application you
run. There is no systemd sandbox, no `ReadOnlyPaths`/`ProtectSystem`
confinement, no HAProxy or firewall standing between it and your files, the
way the earlier LAN-served build had. The only access-control layer left is
the Tauri ACL that gates which JavaScript-invokable commands exist at all —
and that constrains the **webview**, not the operating system: it stops an
XSS in the page from reaching a command that was never declared, but it is
not a sandbox around the process itself. In practice this means: install it
the way you would any desktop application, from a source you trust (verify
`SHA256SUMS`), and do not expect the isolation the old systemd unit
described.

## 13. Building from source

Prerequisites:

```sh
pkg-config libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev \
  libgtk-3-dev libsoup-3.0-dev librsvg2-dev libxdo-dev
```

Then, from the repository root:

```sh
just dev      # the desktop app in dev mode: Vite + the Tauri window
just bundle   # the shipped .deb (a release build)
just smoke    # the WebDriver smoke suite, against the built binary
```

One setup step is easy to trip over: `bundle.externalBin` names the
`wingspan-cli` sidecar, and `tauri-build` copies it on *every* `cargo`
command that touches `wingspan-desktop` — not only when bundling — so
`just tauri-check` and `just tauri-test` fail with `resource path ...
doesn't exist` until the sidecar is in place. `just bundle`'s first two
lines are exactly what builds it:

```sh
cargo build -p wingspan-cli --release
mkdir -p web/src-tauri/binaries
cp target/release/wingspan-cli \
  "web/src-tauri/binaries/wingspan-cli-$(rustc -vV | sed -n 's/host: //p')"
```

`just smoke` drives the *shipped* release binary through `tauri-driver` and
the system `WebKitWebDriver` (`cargo install tauri-driver --locked` and the
`webkit2gtk-driver` package, once). That is the one thing that reads
`WEBKIT_INSPECTOR_SERVER`: the app clears both `WEBKIT_INSPECTOR_*`
variables at startup so no build of Wingspan ever opens a remote-inspector
socket, *except* when `TAURI_WEBVIEW_AUTOMATION=true` — the variable
`tauri-driver` sets and that Tauri itself already reads to decide whether to
allow automation at all. A user-launched app never has it, and anyone who
could set it could equally set `LD_PRELOAD`, so the exception grants no new
authority; it is what lets the smoke suite test the binary that actually
ships rather than a special build of it.

**On a box with no display** (this includes most CI runners and dev
containers), run the window under Xvfb:

```sh
xvfb-run -a env \
  WEBKIT_DISABLE_COMPOSITING_MODE=1 \
  WEBKIT_DISABLE_DMABUF_RENDERER=1 \
  DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/wingspan-no-portal \
  <command>
```

The `DBUS_SESSION_BUS_ADDRESS` line is not optional: without it the app
hangs indefinitely in an `xdg-desktop-portal` D-Bus call that has nothing to
answer it under Xvfb. The two `WEBKIT_*` variables keep WebKitGTK off
compositing/DMA-BUF paths Xvfb cannot serve.

On a real machine with a real display, if a GPU/driver combination paints a
blank or torn window, `WINGSPAN_GPU_WORKAROUND=1` is the escape hatch —
it disables the DMA-BUF renderer without needing Xvfb at all.

## 14. Uninstall

```sh
sudo apt-get remove wingspan
```

This removes the application only. It leaves behind, untouched:

- your config (`~/.config/wingspan/`)
- your state (`~/.local/share/com.jbolinge.wingspan/`)
- your logs
- **your own trades folder**, wherever it is — Wingspan never had a copy of
  it to remove

## 15. Release checklist

The build box has no display and no sudo, so the following cannot be
verified in CI or on the build machine and must be done by hand — on the
owner's own Ubuntu 26.04 machine, plus a throwaway `ubuntu:26.04` container
— before a release is called good. `release.yml`'s `install-test` job
automates the container half; everything else here is manual.

- [ ] install the `.deb` in a clean `ubuntu:26.04` container *(automated —
      `install-test` job)*
- [ ] install the `.deb` on the owner's own Ubuntu 26.04 machine *(not
      verifiable in CI)*
- [ ] launch it from the desktop icon with an empty environment *(not
      verifiable in CI)*
- [ ] confirm it finds its config, loads the remembered folder, and
      refreshes prices twice *(not verifiable in CI)*
- [ ] confirm `/usr/lib/wingspan/fixtures/demo` resolves and "Load demo
      book" works *(not verifiable in CI)*
- [ ] confirm `Ctrl+C` works in the price-range input *(not verifiable in
      CI — the predefined Edit menu items are what make this work at all)*
- [ ] walk each of the four native pickers — Choose folder…, and Settings'
      interpreter / credentials-file / token-file Browse… — through pick,
      confirm and cancel *(not verifiable in CI: WebDriver cannot drive a
      native dialog, so no automated test opens one)*
- [ ] benchmark the risk-graph drag responsiveness (R6) *(not verifiable in
      CI)*
- [ ] re-shoot the README screenshots (`just screenshots`) *(not verifiable
      in CI)*
