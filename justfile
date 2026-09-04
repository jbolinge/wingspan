# Wingspan task runner. `just ci` mirrors .github/workflows/ci.yml (the one
# deliberate difference: CI always runs a clean `npm ci`, while local recipes
# go through the idempotent `web-deps` - see its comment).

set shell := ["bash", "-uc"]

default:
    @just --list

# ---- Rust ----
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all --check

# `wingspan-desktop` is excluded: it links WebKitGTK, which a plain checkout
# need not have. `tauri-check` / `tauri-test` cover it.
clippy:
    cargo clippy --workspace --all-targets --all-features --exclude wingspan-desktop -- -D warnings

test:
    cargo test --workspace --locked --exclude wingspan-desktop

# ---- Desktop (Tauri) ----
# Both recipes need the WebKitGTK/GTK dev packages (see docs/DESKTOP.md) and
# `web/dist` must exist (`just web-build`), because `generate_context!()`
# embeds `frontendDist`.
tauri-check:
    cargo check -p wingspan-desktop

tauri-test:
    cargo test -p wingspan-desktop --locked

# The one property of the built index.html that the desktop CSP depends on:
# no inline <style>, exactly one inline <script>. Mirrors ci.yml's `desktop`
# job step. Read scripts/check_csp_invariant.sh for why.
csp-check: web-build
    scripts/check_csp_invariant.sh

deny:
    cargo deny check

# ---- Web ----
# Forced clean install: wipes web/node_modules and reinstalls from the
# lockfile, exactly like CI. Run it by hand when you want that; the recipes
# below use `web-deps` instead so the dev loop stays offline and instant.
web-install:
    cd web && npm ci

# Idempotent dependency check that every web recipe depends on, so a fresh
# clone works with no manual `npm ci` step (see docs/DEPLOY.md §2). `npm ci`
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
    cd web && npm run typecheck

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
    cargo run -p wingspan-server -- dump-contract --demo --out web/e2e/fixtures/contract

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
    cargo run -p wingspan-server -- dump-contract --demo --out "$out" >/dev/null
    diff -ru "{{justfile_directory()}}/web/e2e/fixtures/contract" "$out"

# ---- E2E ----
# Playwright drives the real binary: the recipe's web servers build `web/dist`
# (when stale) and `wingspan --features embed-ui`, then serve the test fixtures
# (:8787, :8789) and the synthetic demo book (:8791).
#
# A `wingspan` process left running on :8787/:8789/:8791 from an earlier run is
# REUSED as-is, not rebuilt (`reuseExistingServer: !process.env.CI` in
# web/playwright.config.ts) - kill it first after editing Rust or SPA code.
e2e: web-deps
    cd web && npm run e2e

# Re-take the risk-chart baseline after an intentional chart change.
e2e-update: web-deps
    cd web && npm run e2e:update

# README screenshots -> docs/screenshots/{dashboard,risk-graph}.png
screenshots: web-deps
    cd web && npm run screenshots

# ---- Everything CI runs ----
ci: redaction-guard fmt-check clippy test tauri-check web-typecheck web-lint web-test web-build csp-check contract-check e2e

# Release binary with the SPA embedded
release: web-build
    cargo build --release -p wingspan-server --features embed-ui

# The synthetic demo book (fixtures/demo), synthetic prices, pinned to 2026-08-31.
serve-demo:
    cargo run -p wingspan-server -- serve --demo

# Regenerate fixtures/demo after editing scripts/make_demo_book.py.
demo-book:
    python3 scripts/make_demo_book.py
