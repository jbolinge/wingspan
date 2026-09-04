#!/usr/bin/env bash
# The demo-book e2e server on 127.0.0.1:8791: plain `wingspan serve --demo`,
# exactly what the README's Quick Start runs and what `just screenshots`
# captures. It serves the wholly synthetic book in `fixtures/demo` (see
# scripts/make_demo_book.py) — no account data, anonymised or otherwise.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

cd "$ROOT"
export WINGSPAN_SERVER__BIND="${WINGSPAN_E2E_DEMO_BIND:-127.0.0.1:8791}"
export WINGSPAN_MARKET__STATE_DIR="$ROOT/target/e2e-state/demo"
# See serve.sh: a root override left by an interrupted run must not steer
# this one.
rm -f "$WINGSPAN_MARKET__STATE_DIR/trades_root.json"
exec ./target/debug/wingspan serve --demo
