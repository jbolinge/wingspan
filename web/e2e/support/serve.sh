#!/usr/bin/env bash
# The main e2e server: `wingspan serve --demo` on 127.0.0.1:8787, SPA embedded.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
"$ROOT/web/e2e/support/build.sh"

cd "$ROOT"
export WINGSPAN_SERVER__BIND="${WINGSPAN_E2E_BIND:-127.0.0.1:8787}"
# Keep the snapshot cache out of the developer's real state dir.
export WINGSPAN_MARKET__STATE_DIR="$ROOT/target/e2e-state/main"
exec ./target/debug/wingspan serve --demo
