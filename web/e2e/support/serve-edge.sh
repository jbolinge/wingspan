#!/usr/bin/env bash
# The "edge case" e2e server on 127.0.0.1:8789: a development-only
# `wingspan-devserver` instance, not the shipped product.
#
# The main fixtures (`fixtures/trades/legs.csv`) are deliberately clean: every
# leg is priced and every OCC symbol is a valid SPX option, so the SPA's
# null/degraded paths (a leg with no average cost, a row the parser could not
# use) never render against them. This server serves a copy of that CSV with
# four extra rows appended, covering three cases — a blank-price vertical
# (two legs), a non-SPX futures symbol and an unparseable OCC code — which
# exercises exactly those paths against the real API instead of a mock.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

STAGE="$ROOT/target/e2e-edge"
rm -rf "$STAGE"
mkdir -p "$STAGE/fixtures"
cp "$ROOT/fixtures/trades/legs.csv" "$STAGE/fixtures/legs.csv"

# Four rows (three cases) appended to the clean fixture, same 16 columns in
# the same order as the committed file's header:
#   #,Trade,Leg,Side,Qty,Opened,Open Price,Closed,Close Price,Mark,Status,
#   Fees,Expiry,OCC Symbol,Structure,Key
#
# 1-2. A blank-price vertical ("Zeta"): both legs have an empty Open Price,
#      so they load unpriced -> `sheet_leg_unpriced` warnings and
#      `avg_cost: null`.
# 3.   An MES row: `OCC Symbol` is a non-SPX futures contract -> skipped with
#      a `sheet_row_skipped` warning.
# 4.   A bad-OCC row: `OCC Symbol` does not parse as any option code ->
#      skipped with a `sheet_row_skipped` warning.
printf '%s\n' \
  '9,Zeta,,BUY,1,2026-08-20,,,,,Open,0.00,2027-02-19,SPX270219P7000,,Zeta 2026-08-20||2026-08-20 10:00:00' \
  '9,Zeta,,SELL,1,2026-08-20,,,,,Open,0.00,2027-02-19,SPX270219P6900,,Zeta 2026-08-20||2026-08-20 10:00:00' \
  '10,Mesa,,BUY,1,2026-08-20,10.00,,,,Open,0.00,2026-09-19,MESU26,,Mesa 2026-08-20||2026-08-20 11:00:00' \
  '11,Bad Occ,,BUY,1,2026-08-20,10.00,,,,Open,0.00,2026-09-19,NOTACONTRACT,,Bad Occ 2026-08-20||2026-08-20 12:00:00' \
  >> "$STAGE/fixtures/legs.csv"

cd "$STAGE"
PORT="${WINGSPAN_E2E_EDGE_PORT:-8789}"
export WINGSPAN_MARKET__STATE_DIR="$STAGE/state"
# See serve.sh: a source override left by an interrupted run must not steer
# this one. `$STAGE` is wiped above on every run, but the state dir is a
# fixed path outside it, so this is the belt-and-suspenders for a reused
# still-running server picking up a stray override.
rm -f "$WINGSPAN_MARKET__STATE_DIR/trade_source.json" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.csv" \
      "$WINGSPAN_MARKET__STATE_DIR/trade_source_cache.json"
exec "$ROOT/target/debug/wingspan-devserver" serve --demo --legs "$STAGE/fixtures/legs.csv" --port "$PORT"
