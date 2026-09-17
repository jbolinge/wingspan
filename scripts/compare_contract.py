#!/usr/bin/env python3
"""Compare a freshly dumped API contract against the checked-in one.

`wingspan-cli dump-contract` rounds every float to six decimals, which is
coarse enough for values the pricing engine produces directly. The T+0 greek
strip is different: its gamma is a *second* central difference over a 0.01-pt
spot bump, so a last-bit disagreement between two libm builds (exp, ln, erf)
is divided by 1e-4 and lands in the sixth decimal — the same four samples of
`t0_greeks.gamma` have differed between this box and the CI runner on every
run since the strip landed. A byte diff cannot tell that from a contract
change, so this compares the JSON trees instead: keys, lengths, strings,
booleans, nulls and integers exactly; floats to within `--tolerance`
(default 1e-5, a decade above the noise and a decade below anything the
dump keeps). Files present on one side only are reported too, exactly as
`diff -r` would.

Exit status 0 when nothing differs, 1 otherwise; one line per difference.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

DEFAULT_TOLERANCE = 1e-5


def compare_values(expected, actual, path: str, tolerance: float, out: list[str]) -> None:
    """Append one line to `out` for every difference under `path`."""
    if isinstance(expected, bool) or isinstance(actual, bool):
        if expected is not actual:
            out.append(f"{path}: expected {expected!r}, got {actual!r}")
        return
    if isinstance(expected, (int, float)) and isinstance(actual, (int, float)):
        if isinstance(expected, float) or isinstance(actual, float):
            if abs(float(expected) - float(actual)) > tolerance:
                out.append(f"{path}: expected {expected!r}, got {actual!r} (tolerance {tolerance:g})")
        elif expected != actual:
            out.append(f"{path}: expected {expected!r}, got {actual!r}")
        return
    if isinstance(expected, dict) and isinstance(actual, dict):
        for key in sorted(set(expected) | set(actual)):
            child = f"{path}.{key}" if path else key
            if key not in actual:
                out.append(f"{child}: missing from the dump")
            elif key not in expected:
                out.append(f"{child}: not in the checked-in contract")
            else:
                compare_values(expected[key], actual[key], child, tolerance, out)
        return
    if isinstance(expected, list) and isinstance(actual, list):
        if len(expected) != len(actual):
            out.append(f"{path}: expected {len(expected)} items, got {len(actual)}")
            return
        for index, (left, right) in enumerate(zip(expected, actual)):
            compare_values(left, right, f"{path}[{index}]", tolerance, out)
        return
    if type(expected) is not type(actual) or expected != actual:
        out.append(f"{path}: expected {expected!r}, got {actual!r}")


def compare_files(expected: Path, actual: Path, tolerance: float) -> list[str]:
    """Differences between two files of the same name; non-JSON is byte-compared."""
    if expected.suffix != ".json":
        return [] if expected.read_bytes() == actual.read_bytes() else [f"{expected.name}: contents differ"]
    try:
        left = json.loads(expected.read_text(encoding="utf-8"))
        right = json.loads(actual.read_text(encoding="utf-8"))
    except json.JSONDecodeError as err:
        return [f"{expected.name}: not valid JSON on one side ({err})"]
    out: list[str] = []
    compare_values(left, right, "", tolerance, out)
    return [f"{expected.name}: {line}" for line in out]


def compare_dirs(expected_dir: Path, actual_dir: Path, tolerance: float) -> list[str]:
    """Every difference between the two directories, file additions and removals included."""
    expected_names = {p.name for p in expected_dir.iterdir() if p.is_file()}
    actual_names = {p.name for p in actual_dir.iterdir() if p.is_file()}
    out: list[str] = []
    for name in sorted(expected_names - actual_names):
        out.append(f"{name}: only in the checked-in contract")
    for name in sorted(actual_names - expected_names):
        out.append(f"{name}: only in the dump")
    for name in sorted(expected_names & actual_names):
        out.extend(compare_files(expected_dir / name, actual_dir / name, tolerance))
    return out


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("expected", type=Path, help="the checked-in contract directory")
    parser.add_argument("actual", type=Path, help="the freshly dumped directory")
    parser.add_argument("--tolerance", type=float, default=DEFAULT_TOLERANCE)
    args = parser.parse_args(argv)
    differences = compare_dirs(args.expected, args.actual, args.tolerance)
    for line in differences:
        print(line)
    if differences:
        print(f"{len(differences)} difference(s) between {args.expected} and {args.actual}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
