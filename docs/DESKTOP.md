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
- `xdg-desktop-portal` — GTK and WebKitGTK call the desktop portal while the
  window comes up; with no portal to answer, those calls hang (§13's
  `DBUS_SESSION_BUS_ADDRESS` line is the same story under Xvfb). The file and
  folder choosers are *not* portal dialogs: `rfd` and `tauri-plugin-dialog`
  are both pinned to their `gtk3` backend, so a chooser is an in-process
  GTK 3 window

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
terminal). The window opens dark, 1600×1000, centered. Launching it again
brings the existing window forward instead of starting a second copy, and
bringing the window back to the front asks the market loop for a fresh
snapshot (at most once every 5 s, for the Schwab budget's sake); see
`[market] pause_when_hidden` in `docs/CONFIG.md` for the hidden-window half.

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

## 7. The menu, and where the screens are

The native menu bar has four submenus. Every item does exactly what the same
control inside the window does — there is no menu-only behaviour.

| menu | items |
|---|---|
| **Trades** | Choose folder… (`Ctrl+O`), Reload (`Ctrl+Shift+R`), Reveal folder, Clear trades — then Load demo book, Leave demo |
| **Edit** | Cut, Copy, Paste, Select All (the platform's own items, which is where `Ctrl+C` in the price-range and what-if inputs comes from) |
| **View** | Dashboard, Risk graph, Closed, Data health — then Toggle theme |
| **Help** | Diagnostics, Open log folder, Check for updates |

**Settings and Diagnostics are cards in the Data health tab**, not separate
windows: View → Data health and Help → Diagnostics both open that tab.
View → Risk graph is the graph's own **maximize** button: the positions tab
with only the graph on it (`Esc`, **restore** or View → Dashboard bring the
cards and the table back). On the dashboard the **Open positions** card folds
down to its header from the chevron before its title, and the bar under it
drags to set how much of the tab the table takes (double-click resets); the
chart takes whatever is left. Both preferences are remembered.

Under the chart — pinned below the scrolling area, so it is never off the
fold — a **zoom** slider (log2-scaled, so 1×→2×→4×→8×→16× sit at even steps)
and a **reset view** button narrow the price window without touching the
server: dragging the chart pans it, the mouse wheel zooms about the price
under the cursor, and double-clicking the chart is the same as reset view. At
1× a wheel *down* has nothing to zoom out to, so it scrolls the panel the way
it always did; everything else over the chart is a zoom. Zooming in re-scales
the y axis to whatever is inside the visible window — a flat T+0 region opens
up vertically instead of sitting as a thin band — unless **lock y-axis** is
on, which keeps the frozen extent exactly as it was, panned or not. Locking
freezes what is on screen: pressed while zoomed it holds the visible slice's
extent, not the whole range's. Max profit / max loss / PoP / BE in the header stay
the server's numbers over the whole computed range regardless of zoom, which
is why their tooltips say "computed", not "plotted". The view (zoom level and
pan centre) is remembered like the rest of the graph settings — except that
the centre is dropped (the zoom kept) when the price grid itself changes:
another selection, range mode or point count, where a centre from the old grid
would land you on an edge sliver of the new one — and **restore graph
defaults** puts it back to the full range along with everything else.

The **Warnings** card on Data health folds the same way, and **clear** hides
the warnings currently listed (the severity filter scopes it, so `info` then
`clear` silences the skipped-section notes and nothing else). Clearing
touches no file: the warning is remembered by its content and stays hidden
through reloads and relaunches until **restore** brings every cleared one
back; a warning whose text changes surfaces again as new. The tab badge
counts only what is still listed.

## 8. Schwab and tastytrade

Wingspan can price your book against live Schwab market data, but it never
creates, logs in for, or manages the Schwab token itself — that is the job
of whatever login tooling you already use (schwab-py's
`client_from_login_flow`, a cron login job, or another application on this
machine). Wingspan is only ever handed a path.

**The token file arrives via a picker, and Wingspan reads it and never
writes it.** In Data health → Settings → Market data, **Browse…** opens a
native file chooser; after you pick a file, a confirmation dialog names the
full path and states plainly what Wingspan is about to do with it ("Wingspan
will read (never write) your Schwab token from: …"). Only after you confirm
is the path string written to `wingspan.toml` — the token file itself is
never opened, copied, chmoded or created by that action. If the token file's
permissions are wider than its owner (not `0600`), Wingspan shows a warning
naming the file and says to tighten it where it is produced; it never
changes the mode itself.

If your app key and secret live in a separate `KEY=VALUE` file owned by
other tooling, point Wingspan at it the same way: Data health → Settings →
Market data → **Credentials file** → Browse… (another native picker with its
own confirmation). The two environment variable *names* Wingspan reads out of
that file (`app_key_env`, `app_secret_env`) are editable text fields — the
*values* never leave the file and never reach the settings form.

### tastytrade

**tastytrade is a second live source, and it has no token file at all.** Its
credentials are an OAuth2 personal grant you mint once at `my.tastytrade.com`
(Manage → My Profile → API → OAuth Applications → new client with the **`read`
scope only**, then **Create Grant** on that client). The refresh token that
grant mints does not rotate and does not expire, so it is a static string in an
environment variable: there is nothing on disk for Wingspan to read, nothing
for it to write, and the 15-minute access token lives only in memory.
`docs/TASTYTRADE.md` walks the whole thing through.

Point Wingspan at the `KEY=VALUE` file holding the client secret and refresh
token the same way as Schwab's: Data health → Settings → Market data →
**tastytrade** → Credentials file → **Browse…**, a native chooser, and then its
own confirmation dialog:

> Wingspan will read your tastytrade client secret and refresh token from:
> `/home/trader/.config/tastytrade/secrets.env`. Continue?

Only after you confirm is the path *string* written to `wingspan.toml`; the
file itself is never opened, copied, chmoded or created by that action. It may
be the **same file** the Schwab credentials live in — the two are merged by
variable name, first writer winning, and anything already set in your real
environment beats both. As with Schwab, only the variable *names*
(`client_secret_env`, `refresh_token_env`, `client_id_env`) are editable text
on the form; the values never reach it.

Two more ticks and it is live: **tastytrade enabled** on the same card, and
`tastytrade` named in `[market] spot_sources` / `option_sources` — putting it
first is what makes it lead the chain. Off by default and absent from the
default ladders, so an upgrade changes nothing until you do both.

Two switches on that card are yours to set because the app cannot work them
out: **Use the tastytrade sandbox** points at `api.cert.tastyworks.com`, where
auth works but every market-data call answers 502, and **This tastytrade
account gets delayed quotes** is your own statement that the account is not
entitled to real-time data — nothing on the wire says so, and off-hours
staleness looks exactly the same.

## 9. The Python bridge (yfinance)

When Schwab is not configured, expired, or degraded, Wingspan falls back to
a Yahoo-Finance-backed Python bridge for delayed quotes — the middle rung of
the fallback chain (Schwab — or tastytrade, if you put it first — → yfinance →
cached snapshot → statement marks).
**It is entirely optional.** With no working interpreter, Wingspan says so
("Delayed quotes unavailable — you lose only the middle rung") and keeps
going on the cache and the statement marks.

To enable it, create a virtual environment once and point Wingspan at it:

```sh
python3 -m venv ~/.local/share/wingspan/venv
~/.local/share/wingspan/venv/bin/pip install yfinance==1.7.0
# then Data health → Settings → Market data → interpreter → Browse… →
# ~/.local/share/wingspan/venv/bin/python3, and press Test
```

The bridge script itself is embedded in the `wingspan` binary and
self-installs into the state directory the first time it runs — nothing to
deploy for it beyond the interpreter.

## 10. Where things live

| what | path |
|---|---|
| config | `~/.config/wingspan/wingspan.toml` |
| state | `~/.local/share/com.jbolinge.wingspan/state/` (mode `0700`): `snapshot.json`, `closes.json`, `trades_root.json`, the self-installed `yfinance_bridge.py`, `demo/` |
| logs | `~/.local/share/com.jbolinge.wingspan/logs/` — daily-rolled, 7 kept; honours `RUST_LOG` |
| window geometry | `~/.config/com.jbolinge.wingspan/` |

The state directory is created at mode `0700` by whichever binary gets there
first — the app, `wingspan-cli doctor`, or a `wingspan-cli risk` run that
writes the snapshot cache — and an existing directory's mode is left alone, so
a mode you loosen on purpose stays loosened.

The `.deb` installs:

| what | path |
|---|---|
| the application | `/usr/bin/wingspan` |
| the command-line tool | `/usr/bin/wingspan-cli` |
| the bundled demo book | `/usr/lib/wingspan/fixtures/demo` |
| the desktop entry | `/usr/share/applications/wingspan.desktop` |
| the icons | `/usr/share/icons/hicolor/*/apps/wingspan.png` |

## 11. Diagnostics

The **Diagnostics** card, in the Data Health tab (or Help → Diagnostics),
shows the same checklist a terminal `wingspan-cli doctor` prints — config
file found, trades folder resolved, state directory writable, the yfinance
interpreter, the Schwab token file's presence/age/permissions, the tastytrade
grant's configuration and health, and each provider's reachability. **The
Tastytrade rows never run a token exchange** — they report what the running
client already knows, because repeated failed logins earn an hours-long IP
block and a checklist you can press twice must not be able to cause one. The
Live probe behind **Run checks** is where the grant is exercised for real. The rows that cost a process spawn or a network
round trip (the interpreter, the providers) wait for **Run checks**. Its three buttons:

- **Run checks** — re-runs the checklist, this time running the configured
  Python interpreter and reaching out to the providers (the CLI's `--probe`).
  Until you press it the yfinance row reads *"not checked; press Run
  checks"*: the interpreter is a program **you** chose, and opening a tab is
  not a reason to run it. It is also given the same 20 seconds the bridge
  gets, and killed at the deadline — an interpreter on a stalled network
  mount is a row that says *"did not answer within 20s"*, not a button that
  spins forever.
- **Reveal log folder** — opens the log directory in your file manager.
- **Open config file** — opens `wingspan.toml` (or, if none exists yet, the
  folder it would be created in).

`wingspan-cli doctor` prints the identical rows outside the app — useful over
SSH, or before the window has opened at all. Installing the `.deb` puts
`wingspan-cli` on your `PATH` alongside the application itself, so it is there
without a separate download.

## 12. Ambient authority

Wingspan runs with your full user account's ambient authority — it can read
and write anything you can, exactly like any other desktop application you
run. There is no sandbox around the process: no `ReadOnlyPaths`/
`ProtectSystem` confinement, no firewall or proxy standing between it and
your files. The one access-control layer is the Tauri ACL that decides which
JavaScript-invokable commands exist at all — and that constrains the
**webview**, not the operating system: it stops an XSS in the page from
reaching a command that was never declared, but it is not a sandbox around
the process itself. In practice this means: install it the way you would any
desktop application, from a source you trust (verify `SHA256SUMS`).

## 13. Building from source

Prerequisites (the same list CI installs):

```sh
sudo apt-get update
sudo apt-get install -y pkg-config libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev \
  libgtk-3-dev libsoup-3.0-dev librsvg2-dev libxdo-dev
```

Then, from the repository root:

```sh
just dev      # the desktop app in dev mode: Vite + the Tauri window
just bundle   # the shipped .deb (a release build)
just smoke    # the WebDriver smoke suite, against the built binary
```

One quirk is worth knowing about, because the error message is puzzling:
`bundle.externalBin` names the `wingspan-cli` sidecar, and `tauri-build`
copies it on *every* `cargo` command that touches `wingspan-desktop` — not
only when bundling — so a bare `cargo check -p wingspan-desktop` on a fresh
clone stops at `resource path binaries/wingspan-cli-… doesn't exist`. The
directory is gitignored, so it is built rather than checked in. `just
cli-sidecar` does that, and `tauri-check`, `tauri-test` and `bundle` all
depend on it, so `just ci` works on a clean checkout without any setup:

```sh
cargo build -p wingspan-cli --release
mkdir -p web/src-tauri/binaries
cp target/release/wingspan-cli \
  "web/src-tauri/binaries/wingspan-cli-$(rustc -vV | sed -n 's/host: //p')"
```

Reach for `just tauri-check` rather than a bare `cargo check -p
wingspan-desktop` and the question never comes up.

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
- [ ] walk each of the five native pickers — Choose folder…, and Settings'
      interpreter / Schwab credentials-file / Schwab token-file / tastytrade
      credentials-file Browse… — through pick, confirm and cancel *(not
      verifiable in CI: WebDriver cannot drive a native dialog, so no
      automated test opens one — `tests/commands.rs` pins only that each
      picker is dispatched off the IPC thread, which is what the 0.2.0 hang
      cost)*
- [ ] benchmark the risk-graph drag responsiveness (R6) *(not verifiable in
      CI)*
- [ ] re-shoot the README screenshots (`just screenshots`) *(not verifiable
      in CI)*

### Cutting a release

`release.yml` runs only on a pushed `v*` tag, and GitHub runs the copy of
the workflow that is *in the tagged commit* — a tag on a commit that
predates `release.yml` (as `v0.1.0` does) triggers nothing at all, and no
release, not even a draft, appears. So:

1. Bump `[workspace.package].version` in the root `Cargo.toml` (the .deb's
   version comes from there via `web/src-tauri`), run
   `cargo update --workspace --offline` so `Cargo.lock` follows, and merge
   that to `main`.
2. Tag the `main` commit — `git tag -a vX.Y.Z -m '...' && git push origin
   vX.Y.Z` — with the same version the tree now declares; tauri-action
   names the release after the tag, the .deb after the manifest.
3. Watch the `Release` run: the `deb` job builds and attaches
   `wingspan_X.Y.Z_amd64.deb` and `SHA256SUMS` to a **draft** release,
   then `install-test` proves the .deb installs in a clean `ubuntu:26.04`
   container.
4. Work the manual half of the checklist above against that draft's .deb,
   then publish the draft. A draft is invisible on the public Releases
   page, so nothing is downloadable until this step.
