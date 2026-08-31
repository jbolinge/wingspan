#!/usr/bin/env bash
# The main e2e server: `wingspan serve --demo` on 127.0.0.1:8787, SPA embedded.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

cd "$ROOT"
export WINGSPAN_SERVER__BIND="${WINGSPAN_E2E_BIND:-127.0.0.1:8787}"
# Keep the snapshot cache out of the developer's real state dir.
export WINGSPAN_MARKET__STATE_DIR="$ROOT/target/e2e-state/main"
# A trades-root override saved by a crashed/interrupted e2e run (or a stray
# manual PUT) must not silently steer the *next* run's fixture book — every
# test that sets one resets it, but this is the belt-and-suspenders for a
# fresh process start. Only matters when this script actually runs; a reused
# still-running server (local, non-CI) keeps whatever is already in memory,
# which is why the suite itself also clears the override in an afterEach.
rm -f "$WINGSPAN_MARKET__STATE_DIR/trades_root.json"
exec ./target/debug/wingspan serve --demo
