#!/usr/bin/env python3
"""Fail the build if a real Schwab account id has leaked into the repo.

Complements `crates/wingspan-core/tests/redaction.rs`, which only flags an
id immediately after the literal `Account Statement for ` header text. This
check is not tied to that one phrase: it looks for the account id's actual
*shape* - an 8-digit run immediately followed by the broker/account-type
code that always trails it in a real export (`SCHW`, then `(Individual)` /
`(Joint)` / ... - see `ACCOUNT_HEADER_RE` in `anonymize_statements.py`) -
wherever that shape appears in any file, not only right after that one
header phrase. A header written under a different label, moved to a
different line, or repeated in a footer is still caught.

It also looks for the shape of a real broker execution reference number -
13 digits starting `100` - which `anonymize_statements.py` renumbers into a
`9000000000001`-based sequence, so none should survive anywhere in the
repo (fixtures, snapshots, docs, tests, or the SPA's mock data).

And it looks for the shape of a Google spreadsheet id - the `/d/<id>` (or
published `/d/e/<id>`) segment of a `docs.google.com/spreadsheets/...` URL.
Wingspan's Legs sheet is shared as anyone-with-the-link, which makes that id
*bearer-equivalent*: whoever holds it can read the user's trades, exactly
like the account id and execution ref above are bearer-equivalent with a
broker session. Every doc, fixture, test and snapshot that needs to show
one uses the fixed placeholder `1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd`
instead - anything else 20 characters or longer in that position is a real
id that leaked in.

Python 3.13, stdlib only. Usage:

    python3 scripts/check_redaction.py
    python3 scripts/check_redaction.py --root path/to/other/tree

With no `--root`, the repository root (computed from this file's location)
is scanned. `--root` scans the given tree instead - e.g. an unpacked .deb in
CI - in which case `EXCLUDED_FILES` (paths relative to the repo root) simply
will not match anything under it, which is correct: those exclusions exist
for two files' own source text, not for build output.

Exit 0 = clean, 1 = a candidate real account id or execution ref was
found. Findings are reported by path and line only - the candidate value
itself is never printed, matching redaction.rs's own policy.
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# Same shape as REPLACEMENT_ACCOUNT in anonymize_statements.py: an 8-digit
# run directly followed by 2+ uppercase letters (the broker code, e.g.
# "SCHW", or the placeholder's own "TEST"). Matched anywhere in a line -
# unlike redaction.rs, this does not require the literal
# "Account Statement for " prefix to precede it.
ACCOUNT_ID_SHAPE_RE = re.compile(r"\b(\d{8})([A-Z]{2,})")

# Must match REPLACEMENT_ACCOUNT in scripts/anonymize_statements.py.
REPLACEMENT_ACCOUNT = "00000000TEST"

# Every real execution ref in the broker's exports is 13 digits beginning
# with "100"; the synthetic ones anonymize_statements.py substitutes begin
# with "9" (SYNTHETIC_REF_BASE there). Guarded against digits and decimal
# points on either side so a longer number or a decimal fraction that
# happens to contain the shape is not flagged.
EXEC_REF_SHAPE_RE = re.compile(r"(?<![\d.])100\d{10}(?![\d.])")

# A spreadsheet id is bearer-equivalent: anyone holding it can read a sheet
# shared as anyone-with-the-link. None may be committed - docs, fixtures,
# tests and snapshots all use the placeholder below instead.
SHEET_ID_SHAPE_RE = re.compile(r"spreadsheets/d/(?:e/)?([A-Za-z0-9_-]{20,})")
PLACEHOLDER_SHEET_ID = "1AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcd"

SKIPPED_DIRS = {
    ".git",
    ".venv",
    "__pycache__",
    "binaries",
    "dist",
    "gen",
    "icons",
    "node_modules",
    "permissions",
    "playwright-report",
    "target",
    "test-results",
    "unpacked",
}

# `crates/wingspan-core/tests/redaction.rs` deliberately constructs a
# fake-but-realistic id (8 digits immediately followed by "SCHW") at runtime
# via `format!` to prove *that* guard's own narrow, prefix-anchored detector
# works - the digits and letters therefore appear adjacent in its source
# text even though no real account id is involved (and even though this
# script's own docstring/comments must avoid writing that same literal
# adjacency, or it would flag itself the same way). `test_check_redaction.py`
# needs the same kind of fake-but-realistic id, as literal text, to prove
# *this* detector catches it. This check's broader, prefix-independent
# pattern would otherwise flag both known-safe test fixtures; they are
# excluded by path, not silenced, so a real leak added anywhere else in
# either file is still caught.
EXCLUDED_FILES = {
    "crates/wingspan-core/tests/redaction.rs",
    "scripts/test_check_redaction.py",
}


def text_files(root: Path) -> list[Path]:
    out: list[Path] = []
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        if any(part in SKIPPED_DIRS for part in path.relative_to(root).parts):
            continue
        out.append(path)
    return out


def offending_lines(text: str) -> list[int]:
    out = []
    for i, line in enumerate(text.splitlines(), start=1):
        if EXEC_REF_SHAPE_RE.search(line):
            out.append(i)
            continue
        found = False
        for m in ACCOUNT_ID_SHAPE_RE.finditer(line):
            candidate = m.group(1) + m.group(2)
            if candidate.startswith(REPLACEMENT_ACCOUNT):
                continue
            out.append(i)
            found = True
            break
        if found:
            continue
        for m in SHEET_ID_SHAPE_RE.finditer(line):
            if m.group(1) == PLACEHOLDER_SHEET_ID:
                continue
            out.append(i)
            break
    return out


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parent.parent,
        help="tree to scan (default: the repository root)",
    )
    args = parser.parse_args()
    root = args.root.resolve()
    offenders: list[str] = []
    for path in text_files(root):
        relative = path.relative_to(root)
        if str(relative) in EXCLUDED_FILES:
            continue
        try:
            text = path.read_bytes().decode("utf-8")
        except UnicodeDecodeError:
            continue
        for line in offending_lines(text):
            offenders.append(f"{relative}:{line}")

    if offenders:
        print(
            f"error: a candidate real account id (8 digits + a broker/account "
            f"code, e.g. 'SCHW'), real execution ref (13 digits starting "
            f"'100'), or real Google spreadsheet id (the /d/ or /d/e/ segment "
            f"of a docs.google.com/spreadsheets URL, not the "
            f"{PLACEHOLDER_SHEET_ID} placeholder) appears in {len(offenders)} "
            "place(s) - this check is not limited to the 'Account Statement "
            "for' header redaction.rs looks for (the value itself is not "
            "printed on purpose):",
            file=sys.stderr,
        )
        for offender in offenders:
            print(f"  {offender}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
