#!/usr/bin/env bash
# The desktop build's CSP rests on one property of the *built* index.html.
#
# `web/src-tauri/tauri.conf.json` ships `style-src 'self' 'unsafe-inline'`,
# which is what lets ECharts and React set inline styles at runtime. Tauri
# appends a nonce to `style-src` for every inline `<style>` it finds in the
# frontend's index.html, and per CSP3 a nonce makes browsers *ignore*
# `'unsafe-inline'` — so one inline `<style>` would silently block every
# runtime-injected style element and every `setAttribute('style')` call: the
# whole chart surface, with no error anywhere. (Phase 0 spike, a4 correction 2.)
#
# Vite emits external CSS, so the invariant holds by construction — until
# somebody adds a `<style>` to web/index.html or a plugin starts inlining the
# CSS. This is the assertion that says so out loud, next to the one inline
# `<script>` that is allowed: the pre-paint theme boot.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
built="$root/web/dist/index.html"

if [ ! -f "$built" ]; then
    echo "check_csp_invariant: $built is missing - run 'npm run build' in web/ first" >&2
    exit 1
fi

status=0

styles="$(grep -c -i '<style' "$built" || true)"
if [ "$styles" -ne 0 ]; then
    echo "check_csp_invariant: $styles inline <style> in web/dist/index.html" >&2
    echo "  Tauri would add a style-src nonce, which disables 'unsafe-inline'" >&2
    echo "  and breaks every runtime inline style (ECharts, React). Keep the" >&2
    echo "  CSS external." >&2
    status=1
fi

# An inline script is a <script> tag with no src= attribute.
inline="$(grep -o -i '<script[^>]*>' "$built" | grep -c -v 'src=' || true)"
if [ "$inline" -ne 1 ]; then
    echo "check_csp_invariant: $inline inline <script> in web/dist/index.html, expected 1" >&2
    echo "  Only the pre-paint theme boot may be inline; script-src is 'self'" >&2
    echo "  in the shipped CSP, and a second one would change how Tauri emits" >&2
    echo "  script-src." >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "check_csp_invariant: web/dist/index.html has no inline <style> and one inline <script>"
fi
exit "$status"
