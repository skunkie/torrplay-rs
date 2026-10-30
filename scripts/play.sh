#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

# Plays a torrent through a running TorrPlay: adds it from a magnet URI or a
# torrent file, waits for its metadata, picks a file, and opens the file's
# content URL in a media player.
#
#   scripts/play.sh [options] <magnet URI | torrent file>
#
# Options:
#   --api <URL>        TorrPlay's address (default http://127.0.0.1:8642)
#   --file <TEXT>      play the first file whose path contains TEXT; by
#                      default, the largest video file, or the largest file
#   --player <COMMAND> the player to open the URL with (default mpv, else
#                      VLC, as a new instance on macOS); "none" only prints
#                      the URL
#   --list             lists the files and exits
#   --timeout <SECONDS> how long to wait for a magnet's metadata (default
#                      180)
#
# A torrent TorrPlay already has is reused. Needs curl and jq.

set -euo pipefail

api=http://127.0.0.1:8642
file_filter=
player=
list=0
# How long metadata retrieval may take, in seconds.
metadata_timeout=180
# How often to report that metadata is still being retrieved, in seconds.
readonly PROGRESS_INTERVAL_SECONDS=5

usage() {
    sed -n '/^#   scripts/,/^# A torrent/p' "$0" | sed 's/^# \{0,1\}//' >&2
    exit 2
}

while (($#)); do
    case $1 in
        --api) api=${2:?--api needs a URL}; shift 2 ;;
        --file) file_filter=${2:?--file needs text}; shift 2 ;;
        --player) player=${2:?--player needs a command}; shift 2 ;;
        --list) list=1; shift ;;
        --timeout) metadata_timeout=${2:?--timeout needs seconds}; shift 2 ;;
        -h | --help) usage ;;
        -*) echo "unknown option $1" >&2; usage ;;
        *) break ;;
    esac
done
[[ $# == 1 ]] || usage
source=$1
api=${api%/}

for tool in curl jq; do
    command -v "$tool" >/dev/null || { echo "$tool is required" >&2; exit 1; }
done

# Sends a request; prints the body, and the HTTP status on stdout's last line.
request() {
    curl -sS --max-time 60 -w '\n%{http_code}' "$@" ||
        { echo "cannot reach TorrPlay at $api; is it running?" >&2; exit 1; }
}

# Fails with a response's problem detail, or its status.
fail_with() {
    local body=$1 status=$2 what=$3
    local detail
    detail=$(jq -r '.detail // .title // empty' <<<"$body" 2>/dev/null || true)
    echo "$what failed with HTTP $status${detail:+: $detail}" >&2
    exit 1
}

if [[ $source == magnet:* ]]; then
    # Without trackers, peers come only from DHT, which can take minutes.
    if [[ $source != *'&tr='* && $source != *'?tr='* ]]; then
        echo "the magnet URI lists no trackers; finding peers through DHT alone can take minutes" >&2
    fi
    response=$(request -X POST "$api/api/v1/torrents" \
        -H 'Content-Type: application/json' \
        --data "$(jq -n --arg uri "$source" '{magnet_uri: $uri}')")
elif [[ -f $source ]]; then
    response=$(request -X POST "$api/api/v1/torrents" \
        -F "metainfo=@$source;type=application/x-bittorrent")
else
    echo "$source is neither a magnet URI nor a file" >&2
    exit 1
fi
status=${response##*$'\n'}
body=${response%$'\n'*}
case $status in
    201) torrent=$(jq -r '.links.self' <<<"$body") ;;
    409) torrent=$(jq -r '.torrent' <<<"$body"); echo "reusing the torrent TorrPlay already has" >&2 ;;
    *) fail_with "$body" "$status" "adding the torrent" ;;
esac

started=$SECONDS
deadline=$((started + metadata_timeout))
reported=$started
while :; do
    response=$(request "$api$torrent")
    status=${response##*$'\n'}
    body=${response%$'\n'*}
    [[ $status == 200 ]] || fail_with "$body" "$status" "reading the torrent"
    state=$(jq -r '.observed_state' <<<"$body")
    case $state in
        retrieving_metadata) ;;
        failed)
            echo "the torrent failed: $(jq -r '.failure.detail // .failure' <<<"$body")" >&2
            exit 1
            ;;
        *) break ;;
    esac
    if ((SECONDS >= deadline)); then
        echo "no metadata within ${metadata_timeout}s: no peer has answered yet." >&2
        echo "TorrPlay keeps trying; run this again to keep waiting, or use --timeout." >&2
        exit 1
    fi
    if ((SECONDS - reported >= PROGRESS_INTERVAL_SECONDS)); then
        echo "retrieving metadata from peers... $((SECONDS - started))s" >&2
        reported=$SECONDS
    fi
    sleep 1
done
echo "$(jq -r '.name' <<<"$body") ($state)" >&2

# Every file, one JSON object per line, following the pages.
files=
page="$(jq -r '.links.files' <<<"$body")?page_size=200"
while [[ -n $page && $page != null ]]; do
    response=$(request "$api$page")
    status=${response##*$'\n'}
    body=${response%$'\n'*}
    [[ $status == 200 ]] || fail_with "$body" "$status" "listing files"
    files+=$(jq -c '.items[]' <<<"$body")$'\n'
    page=$(jq -r '.links.next // empty' <<<"$body")
done

if ((list)); then
    jq -r '"\(.size_bytes)\t\(.media_type // "-")\t\(.path | join("/"))"' <<<"$files"
    exit 0
fi

if [[ -n $file_filter ]]; then
    chosen=$(jq -sc --arg text "$file_filter" \
        'map(select(.path | join("/") | contains($text))) | first // empty' <<<"$files")
    [[ -n $chosen ]] || { echo "no file's path contains $file_filter" >&2; exit 1; }
else
    chosen=$(jq -sc '
        (map(select((.media_type // "") | startswith("video/"))) | max_by(.size_bytes))
        // max_by(.size_bytes)' <<<"$files")
fi
url=$api$(jq -r '.links.content' <<<"$chosen")
echo "playing $(jq -r '.path | join("/")' <<<"$chosen")" >&2
echo "$url"

if [[ -z $player ]]; then
    if command -v mpv >/dev/null; then
        player=mpv
    elif [[ $(uname -s) == Darwin ]] && open -Ra VLC >/dev/null 2>&1; then
        # VLC on macOS runs one instance, which would take this URL over
        # from a stream already playing; -n opens another.
        player="open -na VLC --args"
    elif command -v vlc >/dev/null; then
        player=vlc
    fi
fi
if [[ -z $player || $player == none ]]; then
    echo "no player found; open the URL above in one" >&2
    exit 0
fi
# Word splitting lets --player carry the player's own options.
# shellcheck disable=SC2086
exec $player "$url"
