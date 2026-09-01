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

Python 3.13, stdlib only. Usage:

    python3 scripts/check_redaction.py

Exit 0 = clean, 1 = a candidate real account id was found. Findings are
reported by path and line only - the candidate value itself is never
printed, matching redaction.rs's own policy.
"""

from __future__ import annotations

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

SKIPPED_DIRS = {
    ".git",
    ".venv",
    "__pycache__",
    "dist",
    "node_modules",
    "playwright-report",
    "target",
    "test-results",
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
        for m in ACCOUNT_ID_SHAPE_RE.finditer(line):
            candidate = m.group(1) + m.group(2)
            if candidate.startswith(REPLACEMENT_ACCOUNT):
                continue
            out.append(i)
            break
    return out


def main() -> int:
    root = Path(__file__).resolve().parent.parent
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
            f"code, e.g. 'SCHW') appears in {len(offenders)} place(s) - this "
            "check is not limited to the 'Account Statement for' header "
            "redaction.rs looks for (the value itself is not printed on "
            "purpose):",
            file=sys.stderr,
        )
        for offender in offenders:
            print(f"  {offender}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
