#!/usr/bin/env bash
# The demo-book e2e server on 127.0.0.1:8791: a development-only
# `wingspan-devserver` instance running plain `serve --demo`, used by the
# Playwright suite and by `just screenshots`. It serves the wholly synthetic
# book in `fixtures/demo/legs.csv` (see scripts/make_demo_book.py) — no
# account data, anonymised or otherwise. Plain `--demo` resolves that CSV on
# its own; no `--legs` flag is needed here.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

cd "$ROOT"
PORT="${WINGSPAN_E2E_DEMO_PORT:-8791}"
export WINGSPAN_MARKET__STATE_DIR="$ROOT/target/e2e-state/demo"
# See serve.sh: a root/source override left by an interrupted run must not
# steer this one.
rm -f "$WINGSPAN_MARKET__STATE_DIR/trades_root.json" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source.json" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.csv" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.json"
exec "$ROOT/target/debug/wingspan-devserver" serve --demo --port "$PORT"
