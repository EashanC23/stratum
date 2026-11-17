#!/bin/sh
# OpenRouter credit usage, for the `credits` widget.
#
# The stratum contract: print `key=value` lines, one per line.
#
#   parts      "<used>,<remaining>"  -> the donut (two slices)
#   label      the whole line of text: "25.6% · $25.75 of $100.50",
#              or "no API key" / "credits unreachable" on failure.
#              The label is complete so the widget never shows half a sentence
#              when a value is missing.
#   pct, used, total, remaining      -> the same numbers, for templates that
#              want them separately
#
# Auth: $OPENROUTER_API_KEY, else ~/.config/stratum/secrets/openrouter.key
# (chmod 600). The key is handed to curl through stdin as a --config document,
# so it never appears in the process table the way `-H "Authorization: ..."`
# would.
#
# STRATUM_OPENROUTER_API_URL is an override for testing against a local stub.

set -u
URL="${STRATUM_OPENROUTER_API_URL:-https://openrouter.ai/api/v1/credits}"
KEY="${OPENROUTER_API_KEY:-}"
KEY_FILE="${OPENROUTER_KEY_FILE:-$HOME/.config/stratum/secrets/openrouter.key}"

if [ -z "$KEY" ] && [ -r "$KEY_FILE" ]; then
    KEY=$(tr -d '\r\n' < "$KEY_FILE")
fi

if [ -z "$KEY" ]; then
    echo "label=no API key"
    echo "parts=0,1"
    exit 0
fi

json=$(
    printf 'header = "Authorization: Bearer %s"\nsilent\nshow-error\nfail\nmax-time = 8\n' "$KEY" \
        | curl --config - "$URL" 2>/dev/null
) || {
    echo "label=credits unreachable"
    echo "parts=0,1"
    exit 0
}

# Response: {"data": {"total_credits": 100.5, "total_usage": 25.75}}
printf '%s' "$json" | python3 -c '
import json, sys

try:
    data = json.load(sys.stdin)["data"]
    total = float(data["total_credits"])
    used = float(data["total_usage"])
except Exception:
    print("label=bad response")
    print("parts=0,1")
    raise SystemExit(0)

remaining = max(total - used, 0.0)
pct = (used / total * 100.0) if total > 0 else 0.0

print(f"label={pct:.1f}% · ${used:,.2f} of ${total:,.2f}")
print(f"parts={used:.2f},{remaining:.2f}")
print(f"pct={pct:.1f}")
print(f"used={used:.2f}")
print(f"total={total:.2f}")
print(f"remaining={remaining:.2f}")
'
