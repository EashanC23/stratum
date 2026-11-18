#!/bin/sh
# Stock ticker, for the `stock` widget.
#
# Data: Yahoo Finance's chart endpoint — no API key, no account, no dependency
# beyond curl and python3. Change TICKER (or export STRATUM_TICKER) per widget;
# one script per symbol is the intended pattern.
#
#   STRATUM_STOCK_RANGE     1d (default), 5d, 1mo, 3mo, 1y
#   STRATUM_STOCK_INTERVAL  5m (default for 1d), 15m, 1h, 1d
#   STRATUM_STOCK_API_URL   override the endpoint (tests use a local stub)
#
# Prints, per the stratum contract:
#   series   comma-separated closes  -> the line graph's data_key
#   label    "AAPL 353.81  +0.54%"   -> one complete sentence, success or failure
#   symbol, price, change, range     -> the same numbers separately
#
# The label is never half-filled: no points, a bad response and an unreachable
# host each print a readable line, which is the pattern every widget script here
# follows.

set -u
TICKER="${STRATUM_TICKER:-AAPL}"
RANGE="${STRATUM_STOCK_RANGE:-1d}"
INTERVAL="${STRATUM_STOCK_INTERVAL:-5m}"
URL="${STRATUM_STOCK_API_URL:-https://query1.finance.yahoo.com/v8/finance/chart/${TICKER}?range=${RANGE}&interval=${INTERVAL}}"

fail() {
    printf 'label=stock: %s\n' "$1"
    printf 'symbol=%s\nseries=\nrange=%s\n' "$TICKER" "$RANGE"
    exit 0
}

json=$(curl -fsS --max-time 8 \
    -A 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' "$URL" 2>/dev/null) ||
    fail "$TICKER unreachable"

printf '%s' "$json" | TICKER="$TICKER" RANGE="$RANGE" python3 -c '
import json, os, sys

ticker = os.environ.get("TICKER", "?")
span = os.environ.get("RANGE", "?")

try:
    result = json.load(sys.stdin)["chart"]["result"][0]
    meta = result.get("meta", {})
    closes = [c for c in result["indicators"]["quote"][0]["close"] if c is not None]
except Exception:
    print(f"label=stock: {ticker} bad response")
    print(f"symbol={ticker}")
    print("series=")
    raise SystemExit(0)

if not closes:
    print(f"label=stock: {ticker} no data")
    print(f"symbol={ticker}")
    print("series=")
    raise SystemExit(0)

price = meta.get("regularMarketPrice") or closes[-1]
prev = meta.get("chartPreviousClose") or meta.get("previousClose") or closes[0]
change = price - prev
pct = (change / prev * 100.0) if prev else 0.0

# Downsample: the graph is a couple of hundred points wide at most, and a
# 400-point series would be resampled by the rasteriser anyway.
step = max(1, len(closes) // 60)
series = closes[::step]
if series[-1] != closes[-1]:
    series.append(closes[-1])

print("series=" + ",".join(f"{v:.2f}" for v in series))
print(f"symbol={ticker}")
print(f"price={price:.2f}")
print(f"change={change:+.2f}")
print(f"pct={pct:+.2f}")
print(f"range={span}")
print(f"label={ticker} {price:.2f}  {pct:+.2f}%")
'
