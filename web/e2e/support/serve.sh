#!/usr/bin/env bash
# The main e2e server on 127.0.0.1:8787, SPA embedded: a development-only
# `wingspan-devserver` instance with the demo wiring (synthetic prices,
# pinned clock, `demo` root source) over the anonymised test fixtures in
# `fixtures/trades`, whose golden numbers the specs assert. `--root` is what
# keeps it off the made-up book plain `serve --demo` shows; that one has its
# own server (serve-demo.sh, :8791).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

cd "$ROOT"
PORT="${WINGSPAN_E2E_PORT:-8787}"
# Keep the snapshot cache out of the developer's real state dir.
export WINGSPAN_MARKET__STATE_DIR="$ROOT/target/e2e-state/main"
# A trades-root/trade-source override saved by a crashed/interrupted e2e run
# (or a stray manual PUT) must not silently steer the *next* run's fixture
# book — every test that sets one resets it, but this is the
# belt-and-suspenders for a fresh process start. Only matters when this
# script actually runs; a reused still-running server (local, non-CI) keeps
# whatever is already in memory, which is why the suite itself also clears
# the override in an afterEach.
rm -f "$WINGSPAN_MARKET__STATE_DIR/trades_root.json" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source.json" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.csv" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.json"
exec "$ROOT/target/debug/wingspan-devserver" serve --demo --root "$ROOT/fixtures/trades" --port "$PORT"
