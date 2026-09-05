#!/usr/bin/env bash
# The shipped binaries must not link a web-server transport.
#
# `wingspan-devserver` is dev-only; `wingspan-cli` and `wingspan-desktop` are
# what a user gets. This asserts that neither of them can serve HTTP.
#
# The check is two-part rather than a bare absence grep because two of the
# crate names involved are also reachable as HTTP *client* transitives:
# `wingspan-marketdata` talks to Schwab over `reqwest`, which pulls in
# `tower-http`, and `hyper-util` pulls in `ipnet`. Absence of those two is
# therefore not something the shipped binaries can satisfy.
#
# So: `axum` and `rust-embed` - the router and the embedded SPA - must not
# appear at all, and `tower-http`/`ipnet` must be reachable by their client
# route and by nothing else. That second half is stronger than absence would
# have been: a server stack sneaking back in would add a second dependent and
# fail here.
set -euo pipefail

crate="${1:?usage: check_no_transport.sh <crate>}"

# `--color never` and `--prefix none`: CI sets CARGO_TERM_COLOR=always, and
# the tree glyphs are not something a parser should have to know about.
tree() { cargo tree --color never --prefix none -p "$crate" -e normal "$@"; }

# 1. The server halves must be absent outright.
if tree | grep -qE '^(axum|rust-embed) '; then
  echo "check_no_transport: $crate links a web-server stack:" >&2
  tree | grep -E '^(axum|rust-embed) ' >&2
  exit 1
fi

# 2. The client-only transitives must have exactly one route in, the expected one.
check_only_dependent() {
  local dep="$1" expected="$2" dependents
  # `cargo tree -i <dep> --depth 1` prints the dep itself, then one line per
  # direct dependent (`name vX.Y.Z`). Drop the first line, keep the names.
  dependents="$(tree -i "$dep" --depth 1 | tail -n +2 | awk '{print $1}' | sort -u)"
  if [ "$dependents" != "$expected" ]; then
    echo "check_no_transport: $crate reaches $dep by an unexpected route." >&2
    echo "  expected only: $expected" >&2
    echo "  got:" >&2
    printf '    %s\n' $dependents >&2
    exit 1
  fi
}

check_only_dependent tower-http reqwest
check_only_dependent ipnet hyper-util

echo "check_no_transport: $crate links no web-server stack (no axum, no rust-embed;" \
     "tower-http only via reqwest, ipnet only via hyper-util)"
