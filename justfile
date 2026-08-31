# Wingspan task runner. `just ci` mirrors .github/workflows/ci.yml.

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
web-install:
    cd web && npm ci

web-typecheck:
    cd web && npm run typecheck

web-lint:
    cd web && npm run lint

web-test:
    cd web && npm run test -- --run

web-build:
    cd web && npm run build

# ---- E2E ----
e2e:
    cd web && npm run e2e

# ---- Everything CI runs ----
ci: fmt-check clippy test web-typecheck web-lint web-test web-build

# Release binary with the SPA embedded
release: web-build
    cargo build --release --features embed-ui

serve-demo:
    cargo run -p wingspan-server -- serve --demo
