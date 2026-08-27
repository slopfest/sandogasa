#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Fetch a published koji-lag store, verify it, and decompress it.
#
# The other half of publish-store.sh, and the reason the notebook in
# tools/koji-lag/notebooks is usable by somebody who has not spent a day
# collecting: sweeping fourteen months out of Koji costs hours of hub time,
# while the same data as a file is a few minutes of download.
#
# The checksum is fetched and checked rather than reported. An analysis is
# only worth as much as the data under it, and a truncated download is
# indistinguishable from a small dataset once the file is open — SQLite will
# happily query a store missing its last pages.
#
# Usage: scripts/fetch-store.sh URL [DEST]
#   e.g. scripts/fetch-store.sh https://example.org/lag-2026-08-20.sqlite.zst
set -euo pipefail

usage() {
    echo "usage: $(basename "$0") URL [DEST]" >&2
    echo "  Fetches a published store, verifies its checksum, decompresses it." >&2
    exit 2
}

[[ $# -ge 1 && $# -le 2 ]] || usage
URL=$1
NAME=$(basename "$URL")
[[ $NAME == *.zst ]] || { echo "error: expected a .zst URL" >&2; exit 1; }
DEST=${2:-${NAME%.zst}}
command -v zstd >/dev/null || { echo "error: zstd is not installed" >&2; exit 1; }
command -v curl >/dev/null || { echo "error: curl is not installed" >&2; exit 1; }

[[ -e $DEST ]] && { echo "error: $DEST exists; move it or name another dest" >&2; exit 1; }

WORK=$(mktemp -d "${TMPDIR:-/tmp}/lag-fetch.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

echo "fetching $URL"
# --fail so an HTML error page is not mistaken for a store, -C - so an
# interrupted download resumes rather than starting over.
curl --fail --location --progress-bar -C - -o "$WORK/$NAME" "$URL"

echo -n "checksum: "
if curl --fail --silent --location -o "$WORK/$NAME.sha256" "$URL.sha256"; then
    if (cd "$WORK" && sha256sum --check --status "$NAME.sha256"); then
        echo "ok"
    else
        echo "MISMATCH"
        echo "error: $NAME does not match its published checksum; the download \
is corrupt or the file has changed" >&2
        exit 1
    fi
else
    # Refuse rather than shrug: silently accepting unverified data is how a
    # truncated download becomes a wrong conclusion.
    echo "NOT PUBLISHED"
    echo "error: no $NAME.sha256 beside the store; refusing to use data that \
cannot be verified" >&2
    exit 1
fi

echo "decompressing to $DEST"
zstd -q -d -o "$DEST" "$WORK/$NAME"

if command -v sqlite3 >/dev/null; then
    echo -n "store: "
    # `to_ts` is an exclusive midnight bound, so the last day held is the
    # one before it -- reporting the bound claims a day the store lacks.
    sqlite3 "file:$DEST?mode=ro" "SELECT 'covers ' || date(min(from_ts),'unixepoch')
        || ' to ' || date(max(to_ts),'unixepoch','-1 day') FROM listed;"
fi
# Name the notebook where the caller actually has it: a package installs
# it under /usr/share, a checkout keeps it in the tree. Printing the
# repo path to someone who installed the RPM sends them to a file they
# do not have.
NOTEBOOK=/usr/share/koji-lag/notebooks/arch-lag.ipynb
[[ -f $NOTEBOOK ]] || NOTEBOOK=tools/koji-lag/notebooks/arch-lag.ipynb
printf '\nready:\n  KOJI_LAG_STORE=%s jupyter lab %s\n' "$DEST" "$NOTEBOOK"
