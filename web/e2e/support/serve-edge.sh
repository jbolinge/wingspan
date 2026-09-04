#!/usr/bin/env bash
# The "edge case" e2e server on 127.0.0.1:8789: a development-only
# `wingspan-devserver` instance, not the shipped product.
#
# The test fixtures are deliberately clean: every order matches an execution,
# so the SPA's null/degraded paths (a leg with no average cost, an unmatched
# order, a reconciliation row that does not balance) never render against them.
# This server serves a copy of the fixtures with one extra open trade whose
# order appears in no statement, which exercises exactly those paths against
# the real API instead of a mock.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

STAGE="$ROOT/target/e2e-edge"
rm -rf "$STAGE"
mkdir -p "$STAGE/fixtures"
cp -r "$ROOT/fixtures/trades" "$STAGE/fixtures/trades"

# No statement contains this vertical, so both legs land unpriced:
# `avg_cost: null`, an `unmatched_order` warning and two `missing`
# reconciliation rows.
printf '%s\n' \
  'BUY +1 VERTICAL SPX 100 19 FEB 27 [AM] 7000/6900 PUT @21.00 LMT FIXED' \
  > "$STAGE/fixtures/trades/Open Trades/Zeta Aug 20 2026"

cd "$STAGE"
PORT="${WINGSPAN_E2E_EDGE_PORT:-8789}"
export WINGSPAN_MARKET__STATE_DIR="$STAGE/state"
exec "$ROOT/target/debug/wingspan-devserver" serve --demo --root "$STAGE/fixtures/trades" --port "$PORT"
