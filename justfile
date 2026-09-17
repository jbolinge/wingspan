# Wingspan task runner. `just ci` runs what .github/workflows/ci.yml runs,
# with these differences:
#   * CI always does a clean `npm ci`; the local recipes go through the
#     idempotent `web-deps` instead - see its comment.
#   * Four CI jobs/steps have no place in `ci` and are run separately:
#     `deny` (`just deny`), `msrv` (a 1.98 toolchain), the two `npm audit`
#     steps, and the bundle chain - `deb-gates` and `e2e-bundle`, which are
#     `just bundle` + `just deb-gates` + `just smoke` locally.

set shell := ["bash", "-uc"]

default:
    @just --list

# ---- Rust ----
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# `wingspan-desktop` is excluded: it links WebKitGTK, which a plain checkout
# need not have. `tauri-clippy` is its clippy pass.
clippy:
    cargo clippy --workspace --all-targets --all-features --exclude wingspan-desktop -- -D warnings

test:
    cargo test --workspace --locked --exclude wingspan-desktop

# ---- Desktop (Tauri) ----
# `bundle.externalBin` names binaries/wingspan-cli-<target triple>, and
# tauri-build's build.rs copies external binaries *unconditionally* - a plain
# `cargo check -p wingspan-desktop` panics with "resource path ... doesn't
# exist" without it, not only `tauri build`. The directory is gitignored, so
# on a fresh clone nothing exists until this has run: every recipe that
# touches the desktop crate depends on it. Idempotent - cargo does nothing
# when the binary is already fresh, and the copy only happens on a change, so
# wingspan-desktop is not rebuilt for nothing.
cli-sidecar:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build -p wingspan-cli --release
    dest="web/src-tauri/binaries/wingspan-cli-$(rustc -vV | sed -n 's/host: //p')"
    mkdir -p "$(dirname "$dest")"
    cmp -s target/release/wingspan-cli "$dest" || cp target/release/wingspan-cli "$dest"

# All three desktop recipes need the WebKitGTK/GTK dev packages (see
# docs/DESKTOP.md), and both dependencies for a reason: `generate_context!()`
# embeds `frontendDist`, so `web/dist` must be built, and tauri-build copies
# `bundle.externalBin` unconditionally, so the sidecar must be in place.
tauri-check: web-build cli-sidecar
    cargo check -p wingspan-desktop --features custom-protocol

# `custom-protocol` is what makes the test binary load the bundled assets
# rather than http://localhost:5173, exactly as `tauri build` does.
tauri-test: web-build cli-sidecar
    cargo test -p wingspan-desktop --locked --features custom-protocol

tauri-clippy: web-build cli-sidecar
    cargo clippy -p wingspan-desktop --all-targets --features custom-protocol -- -D warnings

# The properties of the built index.html that the desktop CSP depends on: no
# inline <style>, exactly one inline <script>. CI's `desktop` job runs the
# same script. Read scripts/check_csp_invariant.sh for why.
csp-check: web-build
    scripts/check_csp_invariant.sh

deny:
    cargo deny check

# The shipped binaries must not link a web-server transport. CI runs the same
# script in two jobs; see the script for why the check is two-part rather than
# a bare absence grep.
transport-check:
    scripts/check_no_transport.sh wingspan-cli
    scripts/check_no_transport.sh wingspan-desktop

# ---- Web ----
# Forced clean install: wipes web/node_modules and reinstalls from the
# lockfile, exactly like CI. Run it by hand when you want that; the recipes
# below use `web-deps` instead so the dev loop stays offline and instant.
web-install:
    cd web && npm ci

# Idempotent dependency check that every web recipe depends on, so a fresh
# clone works with no manual `npm ci` step (see docs/DESKTOP.md). `npm ci`
# is only invoked when node_modules is missing or older than the lockfile;
# on a warm tree this is a no-op that needs no network, which keeps
# `just web-test` / `just e2e` usable as fast dev-loop commands.
web-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}/web"
    if [ ! -d node_modules ] || [ package-lock.json -nt node_modules/.package-lock.json ]; then
        npm ci
    fi

web-typecheck: web-deps
    cd web && npm run typecheck && npm run typecheck:e2e

web-lint: web-deps
    cd web && npm run lint

web-test: web-deps
    cd web && npm run test -- --run

web-build: web-deps
    cd web && npm run build

# ---- Redaction guard ----
# Broader companion to crates/wingspan-core/tests/redaction.rs (part of
# `test`): not anchored to the literal "Account Statement for " prefix.
redaction-guard:
    python3 -m unittest scripts.test_anonymize_statements scripts.test_check_redaction scripts.test_make_demo_book
    python3 scripts/check_redaction.py

# ---- API contract ----
# Re-dump every service-layer response for the demo book into
# web/e2e/fixtures/contract, overwriting what is there. Run this after a
# *deliberate* contract change and commit the result - `contract-check`, not
# this recipe, is what `ci` runs.
contract:
    cargo run -p wingspan-cli -- dump-contract --demo --out web/e2e/fixtures/contract

# The gate: dump into a scratch directory and diff it against the checked-in
# copy, so an unintended change to a DTO, a number or an error sentence fails
# here rather than being silently rewritten under the developer's feet. `diff
# -r` also catches a response file that was added or removed, which a
# `git diff` over tracked paths would miss. Mirrors ci.yml's `contract` job.
contract-check:
    #!/usr/bin/env bash
    set -euo pipefail
    out="$(mktemp -d)"
    trap 'rm -rf "$out"' EXIT
    cargo run -p wingspan-cli -- dump-contract --demo --out "$out" >/dev/null
    diff -ru "{{justfile_directory()}}/web/e2e/fixtures/contract" "$out"

# ---- E2E ----
# Playwright drives a development-only `wingspan-devserver` (never the shipped
# Tauri app): the recipe's web servers build `web/dist` (when stale) and
# `wingspan-devserver --features embed-ui`, then serve the test fixtures
# (:8787, :8789) and the synthetic demo book (:8791).
#
# A `wingspan-devserver` process left running on :8787/:8789/:8791 from an
# earlier run is REUSED as-is, not rebuilt (`reuseExistingServer:
# !process.env.CI` in web/playwright.config.ts) - kill it first after editing
# Rust or SPA code.
e2e: web-deps
    cd web && npm run e2e

# Re-take the risk-chart baseline after an intentional chart change.
e2e-update: web-deps
    cd web && npm run e2e:update

# README screenshots -> docs/screenshots/{dashboard,risk-graph}.png
screenshots: web-deps
    cd web && npm run screenshots

# Regenerate fixtures/demo after editing scripts/make_demo_book.py.
demo-book:
    python3 scripts/make_demo_book.py

# ---- Desktop dev loop ----
# The desktop app in dev mode (Vite on :5173 + the Tauri window). This box has
# no display: prefix with `xvfb-run -a` and set
# DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/wingspan-no-portal (as
# `smoke` does - without it the app hangs in an xdg-desktop-portal call),
# WEBKIT_DISABLE_COMPOSITING_MODE=1 and WEBKIT_DISABLE_DMABUF_RENDERER=1. On a
# machine with a display, drop all three.
dev: web-deps
    cd web && npm run tauri dev

# The SPA alone against the typed mock layer - no Rust at all.
dev-ui: web-deps
    cd web && VITE_MOCK=1 npx vite

# The development-only HTTP API the browser dev loop proxies to.
dev-api:
    cargo run -p wingspan-devserver -- serve --demo --dev

# The pair: `just dev-api` in one shell, this in another.
dev-ui-live: web-deps
    cd web && npm run dev

# The shipped .deb. Needs a release build; only run it when you mean it, and
# check `df -h /` first (the release build is large). The CLI sidecar comes
# from `cli-sidecar`, which is the same two lines CI runs.
bundle: web-build cli-sidecar
    cd web && npm run tauri build -- --bundles deb

# Drives the BUILT binary (target/release/wingspan, so run `just bundle`
# first) through tauri-driver + the system WebKitWebDriver - the external
# provider, nothing linked into the app. Needs `cargo install tauri-driver
# --locked` and the webkit2gtk-driver package once.
#
# The three env vars are all headless-box workarounds, not app configuration:
# DBUS_SESSION_BUS_ADDRESS must point at a socket that does NOT exist, or the
# app hangs in an xdg-desktop-portal call with no portal running; the two
# WEBKIT_ ones keep WebKitGTK off compositing/DMA-BUF paths Xvfb cannot serve.
# Everything the app itself reads (XDG_CONFIG_HOME, XDG_DATA_HOME,
# WINGSPAN_AS_OF) is set per launch by web/e2e-wdio/helpers.ts.
smoke: web-deps
    cd web && DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent/wingspan-no-portal \
        WEBKIT_DISABLE_COMPOSITING_MODE=1 WEBKIT_DISABLE_DMABUF_RENDERER=1 \
        xvfb-run -a npm run e2e:wdio

# The unpack gates CI runs after bundling, so the local check and CI cannot
# drift: no fixtures/trades path, no source maps, no leaked account ids, the
# demo book and both binaries are present, and the declared Depends are.
deb-gates:
    #!/usr/bin/env bash
    set -euo pipefail
    deb=$(ls target/release/bundle/deb/wingspan_*.deb)
    rm -rf unpacked && dpkg-deb -x "$deb" unpacked
    # R11, byte for byte as ci.yml runs it. It stays a content grep - the
    # anonymised book must not reach the installer in any form, not merely
    # under a path that still says so. Passing it is why tauri.conf.json
    # enumerates the demo book's three directories plus `legs.csv` instead of
    # naming the folder: fixtures/demo/README.md names `fixtures/trades/` in
    # prose, and a generated developer note has no business in /usr/lib
    # anyway.
    ! grep -rq 'fixtures/trades' unpacked
    ! find unpacked -name '*.map' | grep -q .
    python3 scripts/check_redaction.py --root unpacked
    test -d unpacked/usr/lib/wingspan/fixtures/demo
    test -x unpacked/usr/bin/wingspan
    test -x unpacked/usr/bin/wingspan-cli
    depends=$(dpkg-deb -f "$deb" Depends); echo "Depends: $depends" >&2
    grep 'libwebkit2gtk-4.1-0' <<<"$depends" >/dev/null
    grep 'libgtk-3-0' <<<"$depends" >/dev/null
    grep 'xdg-desktop-portal' <<<"$depends" >/dev/null
    dpkg-deb -c "$deb" | grep 'usr/share/applications/wingspan.desktop' >/dev/null
    # The cost of enumerating those four entries is that a fourth directory,
    # or a second loose file, would ship as nothing at all, silently. Every
    # file the demo book has (bar the README) must be in the bundle, and
    # nothing else.
    diff <(cd fixtures/demo && find . -type f ! -name README.md | sort) \
         <(cd unpacked/usr/lib/wingspan/fixtures/demo && find . -type f | sort)

# ---- Everything CI runs ----
ci: redaction-guard fmt-check clippy test tauri-check tauri-clippy tauri-test transport-check web-typecheck web-lint web-test web-build csp-check contract-check e2e
