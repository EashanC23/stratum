#!/bin/sh
# Now playing — with a fallback chain, because macOS 26 will not tell a plain CLI
# what is playing (`stratum nowplaying --why` explains why in one line).
#
#   1. media        `stratum nowplaying` — MediaRemote, the source Control Centre
#                   uses. First choice whenever the OS is willing to answer.
#   2. music/spotify the app's own AppleScript, when it is running. Still inbuilt:
#                   no third-party binary, and it works even behind the gate.
#   3. lastfm       the Last.fm "now playing" flag, which sees anything a
#                   scrobbler can see — browsers included.
#   4. cache        the last track seen, marked state=last.
#
# Every path prints key=value plus `source=`, so the widget never claims to know
# more than it does.
#
# STRATUM_BIN overrides the binary; STRATUM_NOWPLAYING_JSON short-circuits with a
# fixture (handy for screenshots and tests).

set -u
BIN="${STRATUM_BIN:-$(command -v stratum 2>/dev/null || echo "$HOME/.local/bin/stratum")}"
HERE=$(cd "$(dirname "$0")" && pwd)
CACHE="${XDG_CACHE_HOME:-$HOME/.cache}/stratum/nowplaying.last"
# Test fixtures (STRATUM_NOWPLAYING_JSON) must never poison the real cache — the
# widget would then show a made-up track as "last played".
CACHE_OK=1
[ -n "${STRATUM_NOWPLAYING_JSON:-}" ] && CACHE_OK=0
# A stand-in osascript on PATH is also a fixture: never cache its output.
[ -n "${STRATUM_NOWPLAYING_NO_CACHE:-}" ] && CACHE_OK=0

emit() {
    # $1 source, $2 state, $3 title, $4 artist, $5 album, $6 elapsed, $7 duration
    printf 'source=%s\nstate=%s\ntitle=%s\nartist=%s\nalbum=%s\n' "$1" "$2" "$3" "$4" "$5"
    if [ -n "${6:-}" ] && [ -n "${7:-}" ]; then
        printf 'elapsed=%s\nduration=%s\n' "$6" "$7"
        # $6/$7, not 6/7: bare numbers are constants, not the parameters.
        remaining=$(( $7 > $6 ? $7 - $6 : 0 ))
        progress=$(( $7 > 0 ? $6 * 100 / $7 : 0 ))
        printf 'remaining=%s\nprogress=%s\nparts=%s,%s\n' "$remaining" "$progress" "$6" "$remaining"
    else
        printf 'elapsed=\nduration=\n'
    fi
    if [ -n "$4" ]; then
        printf 'label=%s — %s\n' "$4" "$3"
    else
        printf 'label=%s\n' "$3"
    fi
}

finish() {
    # $@ = emit's arguments; also records the track as "last"
    if [ "$CACHE_OK" = 1 ]; then
        printf '%s\n' "$1" >"$(dirname "$CACHE")/nowplaying.source" 2>/dev/null
        mkdir -p "$(dirname "$CACHE")" 2>/dev/null
        emit "$@" | tee "$CACHE"
    else
        emit "$@"
    fi
}

pick() {
    # pull one key out of a key=value blob held in $lf
    printf '%s\n' "$lf" | sed -n "s/^$1=//p" | head -1
}

# 1) MediaRemote, via the stratum binary -------------------------------------
if [ -x "$BIN" ]; then
    raw=$("$BIN" nowplaying 2>/dev/null)
    case "$raw" in
        state=playing* | state=paused*)
            if [ "$CACHE_OK" = 1 ]; then
                mkdir -p "$(dirname "$CACHE")" 2>/dev/null
                printf '%s\n' "$raw" >"$CACHE" 2>/dev/null
                printf 'media\n' >"$(dirname "$CACHE")/nowplaying.source" 2>/dev/null
            fi
            printf '%s\n' "$raw"
            elapsed=$(printf '%s\n' "$raw" | sed -n 's/^elapsed=//p')
            remaining=$(printf '%s\n' "$raw" | sed -n 's/^remaining=//p')
            if [ -n "${elapsed:-}" ] && [ -n "${remaining:-}" ]; then
                printf 'parts=%s,%s\n' "$elapsed" "$remaining"
            fi
            exit 0
            ;;
    esac
fi

# 2) The playing app itself, by AppleScript ----------------------------------
# Two hazards, both learned the hard way:
#   * fields must be split on a character that cannot occur in metadata — a
#     title with spaces shifts every field if you word-split, so tabs are not
#     enough and `set --` is wrong; ASCII 31 (unit separator) is used instead.
#   * Music can hang on AppleEvents (error -1712, two minutes by default), so
#     every call is bounded by `with timeout`. A stuck player must cost 3s, not
#     stall the widget and the hop after it.
read_track() {
    # $1 process name, $2 AppleScript returning unit-separator-joined fields
    command -v pgrep >/dev/null 2>&1 || return 1
    pgrep -x "$1" >/dev/null 2>&1 || return 1
    out=$(osascript -e "$2" 2>/dev/null) || return 1
    [ -n "$out" ] || return 1
    printf '%s' "$out"
}

field() {
    # $1 index, $2 blob joined by unit separator
    printf '%s' "$2" | awk -F"$(printf '\037')" -v i="$1" '{ print $i }'
}

SEP_APPLESCRIPT='(ASCII character 31)'

fields=$(read_track Music "with timeout of 3 seconds
tell application \"Music\" to if player state is playing then return ((name of current track) & $SEP_APPLESCRIPT & (artist of current track) & $SEP_APPLESCRIPT & (album of current track) & $SEP_APPLESCRIPT & ((player position) as integer) & $SEP_APPLESCRIPT & ((duration of current track) as integer))
end timeout")
if [ -n "${fields:-}" ]; then
    finish music playing "$(field 1 "$fields")" "$(field 2 "$fields")" "$(field 3 "$fields")" \
        "$(field 4 "$fields")" "$(field 5 "$fields")"
    exit 0
fi

fields=$(read_track Spotify "with timeout of 3 seconds
tell application \"Spotify\" to if player state is playing then return ((name of current track) & $SEP_APPLESCRIPT & (artist of current track) & $SEP_APPLESCRIPT & (album of current track) & $SEP_APPLESCRIPT & ((player position) as integer) & $SEP_APPLESCRIPT & (((duration of current track) / 1000) as integer))
end timeout")
if [ -n "${fields:-}" ]; then
    finish spotify playing "$(field 1 "$fields")" "$(field 2 "$fields")" "$(field 3 "$fields")" \
        "$(field 4 "$fields")" "$(field 5 "$fields")"
    exit 0
fi

# 3) Last.fm, when it is configured ------------------------------------------
if [ -x "$HERE/lastfm.sh" ]; then
    lf=$("$HERE/lastfm.sh" 2>/dev/null)
    case "$lf" in
        *"state=playing"*)
            finish lastfm playing "$(pick title)" "$(pick artist)" "$(pick album)" "" ""
            exit 0
            ;;
    esac
fi

# 4) Whatever we saw last ----------------------------------------------------
if [ -s "$CACHE" ]; then
    sed -e 's/^state=.*/state=last/' -e 's/^mark=.*/mark=(last)/' "$CACHE"
    grep -q '^source=' "$CACHE" || echo "source=cache"
else
    printf 'source=none\nstate=idle\nlabel=nothing playing yet\n'
fi
