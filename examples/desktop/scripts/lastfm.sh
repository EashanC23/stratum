#!/bin/sh
# Last.fm — what you are playing now, or what you played last.
#
#   method=user.getRecentTracks, limit=1
#
# Auth: $LASTFM_API_KEY + $LASTFM_USER, else
#   ~/.config/stratum/secrets/lastfm.key   (the API key, chmod 600)
#   ~/.config/stratum/secrets/lastfm.user  (the username)
#
# Get a key at https://www.last.fm/api/account/create
#
# STRATUM_LASTFM_API_URL overrides the endpoint (used by the tests with a local
# stub, so the script can be exercised without an account).

set -u
API="${STRATUM_LASTFM_API_URL:-https://ws.audioscrobbler.com/2.0/}"
SECRETS="${XDG_CONFIG_HOME:-$HOME/.config}/stratum/secrets"

KEY="${LASTFM_API_KEY:-}"
USER="${LASTFM_USER:-}"
[ -z "$KEY" ] && [ -r "$SECRETS/lastfm.key" ] && KEY=$(tr -d '\r\n' <"$SECRETS/lastfm.key")
[ -z "$USER" ] && [ -r "$SECRETS/lastfm.user" ] && USER=$(tr -d '\r\n' <"$SECRETS/lastfm.user")

if [ -z "$KEY" ] || [ -z "$USER" ]; then
    echo "state=error"
    echo "label=lastfm: no key/user"
    echo "hint=put the key in ~/.config/stratum/secrets/lastfm.key"
    exit 0
fi

json=$(
    curl -fsS --max-time 8 --get "$API" \
        --data-urlencode "method=user.getRecentTracks" \
        --data-urlencode "user=$USER" \
        --data-urlencode "api_key=$KEY" \
        --data-urlencode "format=json" \
        --data-urlencode "limit=1" 2>/dev/null
) || {
    echo "state=error"
    echo "label=lastfm unreachable"
    exit 0
}

printf '%s' "$json" | python3 -c '
import json, sys, time

try:
    tracks = json.load(sys.stdin)["recenttracks"]["track"]
except Exception:
    print("state=error")
    print("label=lastfm: bad response")
    raise SystemExit(0)

# The API returns a list, but a single result is sometimes an object.
if isinstance(tracks, dict):
    tracks = [tracks]
if not tracks:
    print("state=empty")
    print("label=lastfm: no scrobbles")
    raise SystemExit(0)

t = tracks[0]
now = t.get("@attr", {}).get("nowplaying") == "true"
title = t.get("name", "")
artist = t.get("artist", {}).get("#text", "")
album = t.get("album", {}).get("#text", "")

when = ""
if not now:
    uts = t.get("date", {}).get("uts")
    if uts:
        ago = max(0, int(time.time()) - int(uts))
        if ago < 90:
            when = f"{ago}s ago"
        elif ago < 5400:
            when = f"{ago // 60}m ago"
        elif ago < 172800:
            when = f"{ago // 3600}h ago"
        else:
            when = f"{ago // 86400}d ago"

mark = ">" if now else "(last)"
label = f"{artist} - {title}" if artist else title
state = "playing" if now else "last"
print(f"state={state}")
print(f"title={title}")
print(f"artist={artist}")
print(f"album={album}")
print(f"when={when}")
print(f"mark={mark}")
print(f"label={label}")
'
