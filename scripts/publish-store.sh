#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Package a koji-lag store for publication: a consistent snapshot, zstd
# compressed, with a checksum beside it.
#
# The store is the thing worth publishing. Reports answer the questions we
# thought to ask; a store answers the ones somebody else thinks of, which is
# the whole point of the notebook in tools/koji-lag/notebooks. And it is
# small enough to hand over: zstd at its default setting takes it to about a
# third — 2,402MB to 763MB, measured 2026-08-20 — so this is a download
# rather than a data release.
#
# `VACUUM INTO` rather than `cp`, for the reason backup-store.sh explains at
# length: a WAL-mode store keeps committed data across three files, and
# copying them individually can capture a database that opens cleanly with
# rows quietly missing. Compressing that snapshot rather than the live file
# means the artefact is consistent even if a sync is running.
#
# Usage: scripts/publish-store.sh STORE [OUTDIR]
#   e.g. scripts/publish-store.sh scratch/lag.sqlite dist/
set -euo pipefail

usage() {
    echo "usage: $(basename "$0") STORE [OUTDIR]" >&2
    echo "  Compresses a consistent snapshot of a store, with a checksum." >&2
    exit 2
}

[[ $# -ge 1 && $# -le 2 ]] || usage
STORE=$1
OUTDIR=${2:-dist}
[[ -f $STORE ]] || { echo "error: no such store: $STORE" >&2; exit 1; }
command -v zstd >/dev/null || { echo "error: zstd is not installed" >&2; exit 1; }
command -v sqlite3 >/dev/null || { echo "error: sqlite3 is not installed" >&2; exit 1; }

# Name the artefact for the last day it covers, not for today: two runs on
# different days over the same data should produce the same name, and a
# reader wants to know what is in it rather than when it was packed.
# `to_ts` is an exclusive midnight bound, so the last day actually covered
# is the day before it -- a store whose record ends 2026-08-20 holds data
# through the 19th, and naming the file for the bound tells a reader it has
# a day it does not.
FIRST=$(sqlite3 "file:$STORE?mode=ro" \
    "SELECT date(min(from_ts), 'unixepoch') FROM listed;" 2>/dev/null || true)
COVERS=$(sqlite3 "file:$STORE?mode=ro" \
    "SELECT date(max(to_ts), 'unixepoch', '-1 day') FROM listed;" 2>/dev/null || true)
# The schema the store was written by. `Store::open` refuses one it is too
# old to read, so this is not a safety mechanism -- it is so a reader can
# tell which asset their build can use without downloading a gigabyte first.
SCHEMA=$(sqlite3 "file:$STORE?mode=ro" \
    "SELECT value FROM meta WHERE key = 'schema_version';" 2>/dev/null || true)
[[ -n $COVERS && -n $FIRST ]] || { echo "error: $STORE has no coverage record" >&2; exit 1; }
[[ -n $SCHEMA ]] || { echo "error: $STORE records no schema version" >&2; exit 1; }
NAME="lag-$FIRST-to-$COVERS-schema$SCHEMA.sqlite.zst"

mkdir -p "$OUTDIR"
SNAP=$(mktemp "${TMPDIR:-/tmp}/lag-publish.XXXXXX.sqlite")
trap 'rm -f "$SNAP"' EXIT

before=$(stat -c %s "$STORE")
echo "snapshotting $STORE ($((before / 1024 / 1024))MB), covering to $COVERS"
sqlite3 "file:$STORE?mode=ro" "VACUUM INTO '$SNAP'"

# Verify before publishing, not after. A corrupt artefact that nobody
# checked is worse than no artefact, and this is the last moment the
# original is still to hand for comparison.
echo -n "verifying snapshot: "
check=$(sqlite3 "$SNAP" "PRAGMA integrity_check")
[[ $check == ok ]] || { echo "FAILED: $check"; exit 1; }
echo "ok"

echo "compressing to $OUTDIR/$NAME"
# Level 9, measured rather than guessed. On a store of this shape the
# levels form a plateau: -9 gives 30.0% and -11 through -15 give 29.9% for
# two to six times the time, so they are never worth reaching for. -17
# (27.6%) and -19 (26.5%) are real gains if minutes are acceptable.
#
# Decompression is flat at about half a second whatever the level, so this
# choice costs the packer and never the downloader.
#
# No --long: it buys half a percentage point, and --long=31 produces a frame
# that plain `zstd -d` refuses -- which fetch-store.sh uses, so it would
# only fail for the person downloading. --long=27 is safe but not worth it.
zstd -9 -q -f -T0 -o "$OUTDIR/$NAME" "$SNAP"
after=$(stat -c %s "$OUTDIR/$NAME")

(cd "$OUTDIR" && sha256sum "$NAME" > "$NAME.sha256")

printf '\n%s\n' "published $OUTDIR/$NAME"
printf '  %sMB from %sMB (%d%% of the original)\n' \
    "$((after / 1024 / 1024))" "$((before / 1024 / 1024))" \
    "$((after * 100 / before))"
printf '  %s\n' "$(cat "$OUTDIR/$NAME.sha256")"
# The fetch counterpart is named differently once packaged, and sits
# beside this script either way -- so ask where this one is running from
# rather than hardcoding the checkout path.
FETCH=$(dirname "$0")/koji-lag-fetch-store
[[ -x $FETCH ]] && FETCH=koji-lag-fetch-store || FETCH=scripts/fetch-store.sh
printf '\nto use it:\n  %s <url>/%s\n' "$FETCH" "$NAME"
