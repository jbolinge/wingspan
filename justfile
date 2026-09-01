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

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --locked

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
    python3 -m unittest scripts.test_anonymize_statements scripts.test_check_redaction
    python3 scripts/check_redaction.py

# ---- E2E ----
# Playwright drives the real binary: the recipe's web server builds `web/dist`
# (when stale) and `wingspan --features embed-ui`, then serves `--demo`.
#
# A `wingspan` process left running on :8787/:8789 from an earlier run is
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
ci: redaction-guard fmt-check clippy test web-typecheck web-lint web-test web-build e2e

# Release binary with the SPA embedded
release: web-build
    cargo build --release --features embed-ui

serve-demo:
    cargo run -p wingspan-server -- serve --demo
