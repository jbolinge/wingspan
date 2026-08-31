#!/usr/bin/env bash
# Build what the e2e suite serves: the SPA bundle plus the `wingspan` binary
# with that bundle embedded (`--features embed-ui`).
#
# Both Playwright `webServer` entries call this, and Playwright starts them in
# parallel, so the whole thing runs under an flock: two concurrent `npm run
# build`s would write `web/dist` at the same time. Cargo has its own lock, but
# serialising here keeps the log readable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
mkdir -p "$ROOT/target"
exec 9>"$ROOT/target/.e2e-build.lock"
flock 9

cd "$ROOT"

# `web/dist` is stale when it is missing, or older than anything it is built
# from. In CI the web job has already built it, so this is a no-op.
web_is_stale() {
  [ -f web/dist/index.html ] || return 0
  [ -n "$(find web/src web/index.html web/package.json web/vite.config.ts \
      -newer web/dist/index.html -print -quit 2>/dev/null)" ]
}

if web_is_stale; then
  echo "e2e: building the SPA (web/dist is missing or stale)" >&2
  (cd web && npm run build)
else
  echo "e2e: reusing web/dist" >&2
fi

# Cheap when the tree has not changed; `build.rs` re-stages web/dist for
# rust-embed whenever the bundle above was rewritten.
echo "e2e: building wingspan (embed-ui)" >&2
cargo build -p wingspan-server --features embed-ui
