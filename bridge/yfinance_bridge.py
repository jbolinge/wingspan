#!/usr/bin/env python3
"""Wingspan's yfinance sidecar.

One JSON request on stdin, one JSON response on stdout, then exit. No
arguments, no network configuration, no state: everything the bridge needs is
in the request, which makes it trivial to fake in tests and trivial to run by
hand when something looks wrong:

    echo '{"underlying":"SPX","want_spot":true,"contracts":[]}' \
        | python3 bridge/yfinance_bridge.py

Request
-------
{
  "underlying": "SPX",              # symbol as wingspan knows it
  "yahoo_symbol": "^SPX",           # optional override
  "want_spot": true,
  "contracts": [                    # may be empty
    {"key": "SPX270219P7325", "expiry": "2027-02-19", "strike": 7325.0, "right": "P"}
  ]
}

Response (always exit code 0 when we produced JSON)
---------------------------------------------------
{"ok": true,
 "as_of": "2026-08-31T17:00:00+00:00",
 "spot": {"symbol": "SPX", "last": 7750.2, "change": -12.5, "previous_close": 7762.7},
 "quotes": [{"key": "...", "bid": .., "ask": .., "last": .., "mark": .., "iv": ..}],
 "missing": ["KEY", ...],
 "warnings": ["..."]}

{"ok": false, "error": {"kind": "...", "message": "..."}}

`kind` is one of `bad_request`, `yfinance_missing`, `yahoo_error`, `no_data`
so the Rust side can name the failure precisely instead of showing a
traceback.
"""

from __future__ import annotations

import datetime as _dt
import json
import math
import sys

EXPIRY_FMT = "%Y-%m-%d"


def _fail(kind: str, message: str) -> None:
    json.dump({"ok": False, "error": {"kind": kind, "message": str(message)[:500]}}, sys.stdout)
    sys.stdout.write("\n")
    sys.stdout.flush()
    sys.exit(0)


def _num(value):
    """Yahoo fills gaps with NaN; JSON has no such thing."""
    try:
        out = float(value)
    except (TypeError, ValueError):
        return None
    if math.isnan(out) or math.isinf(out):
        return None
    return out


def _fast(info, *names):
    """`fast_info` is neither an object nor a Mapping; try both."""
    for name in names:
        value = _num(getattr(info, name, None))
        if value is not None:
            return value
    for name in names:
        try:
            value = _num(info[name])
        except Exception:  # noqa: BLE001 - not a real Mapping
            continue
        if value is not None:
            return value
    return None


def _spot_from(ticker, symbol):
    last = previous = None
    info = getattr(ticker, "fast_info", None)
    if info is not None:
        last = _fast(info, "last_price", "lastPrice")
        previous = _fast(info, "previous_close", "previousClose")
    if last is None:
        # Off-hours, and for some indices, fast_info is empty; the daily bar's
        # close is the prior close, which is exactly what we want to report.
        close = getattr(ticker.history(period="2d"), "Close", None)
        closes = [] if close is None else [c for c in (_num(v) for v in list(close)) if c is not None]
        if closes:
            last = closes[-1]
            if previous is None and len(closes) > 1:
                previous = closes[-2]
    if last is None:
        return None
    change = None if previous is None else last - previous
    return {"symbol": symbol, "last": last, "change": change, "previous_close": previous}


def _rows(frame):
    if frame is None:
        return []
    try:
        return list(frame.itertuples())
    except AttributeError:
        return list(frame)


def _quote_from(row, key):
    bid = _num(getattr(row, "bid", None))
    ask = _num(getattr(row, "ask", None))
    last = _num(getattr(row, "lastPrice", None))
    mark = None
    if bid is not None and ask is not None and ask > 0 and bid <= ask:
        mark = (bid + ask) / 2.0
    if mark is None or mark <= 0:
        mark = last
    iv = _num(getattr(row, "impliedVolatility", None))
    if iv is not None and (iv <= 0 or iv > 5):
        iv = None
    return {"key": key, "bid": bid, "ask": ask, "last": last, "mark": mark, "iv": iv}


def main() -> None:
    raw = sys.stdin.read()
    try:
        request = json.loads(raw) if raw.strip() else {}
    except ValueError as exc:
        _fail("bad_request", f"stdin is not JSON: {exc}")
        return

    underlying = request.get("underlying") or "SPX"
    symbol = request.get("yahoo_symbol") or ("^" + underlying if underlying == "SPX" else underlying)
    contracts = request.get("contracts") or []
    want_spot = bool(request.get("want_spot", True))

    try:
        import yfinance  # noqa: PLC0415 - deliberately inside main
    except Exception as exc:  # noqa: BLE001 - any import failure is the same story
        _fail("yfinance_missing", f"import yfinance failed: {exc}")
        return

    warnings: list[str] = []
    missing: list[str] = []
    quotes: list[dict] = []
    spot = None

    try:
        ticker = yfinance.Ticker(symbol)
        if want_spot:
            spot = _spot_from(ticker, underlying)
            if spot is None:
                warnings.append(f"no spot for {symbol}")

        if contracts:
            by_expiry: dict[str, list[dict]] = {}
            for contract in contracts:
                expiry = str(contract.get("expiry"))
                try:
                    _dt.datetime.strptime(expiry, EXPIRY_FMT)
                except ValueError:
                    missing.append(contract.get("key"))
                    continue
                by_expiry.setdefault(expiry, []).append(contract)

            available = set(getattr(ticker, "options", ()) or ())
            for expiry, wanted in sorted(by_expiry.items()):
                if available and expiry not in available:
                    warnings.append(f"{symbol} has no {expiry} expiry listed")
                    missing.extend(c.get("key") for c in wanted)
                    continue
                chain = ticker.option_chain(expiry)
                frames = {
                    "C": _rows(getattr(chain, "calls", None)),
                    "P": _rows(getattr(chain, "puts", None)),
                }
                for contract in wanted:
                    right = str(contract.get("right", "")).upper()[:1]
                    strike = _num(contract.get("strike"))
                    key = contract.get("key")
                    match = None
                    if strike is not None:
                        for row in frames.get(right, []):
                            row_strike = _num(getattr(row, "strike", None))
                            if row_strike is not None and abs(row_strike - strike) < 1e-6:
                                match = row
                                break
                    if match is None:
                        missing.append(key)
                    else:
                        quotes.append(_quote_from(match, key))
    except Exception as exc:  # noqa: BLE001 - everything below yfinance is Yahoo's fault
        _fail("yahoo_error", f"{type(exc).__name__}: {exc}")
        return

    if want_spot and spot is None and not quotes:
        _fail("no_data", f"Yahoo returned nothing for {symbol}")
        return

    json.dump(
        {
            "ok": True,
            "as_of": _dt.datetime.now(_dt.timezone.utc).isoformat(),
            "spot": spot,
            "quotes": quotes,
            "missing": [m for m in missing if m],
            "warnings": warnings,
        },
        sys.stdout,
    )
    sys.stdout.write("\n")
    sys.stdout.flush()


if __name__ == "__main__":
    main()
