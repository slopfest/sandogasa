#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Compact a koji-lag store in place, refusing if anything else has it open.
#
# Why it needs the store to itself: VACUUM rebuilds the whole database under
# an exclusive lock, minutes for a large store. A sync running at the same
# time would block on every batch and then fail once its busy timeout ran
# out — losing an hour of hub time to a housekeeping job. So this checks
# three ways before starting, and none of them alone is enough:
#
#   1. The -wal and -shm sidecars. SQLite deletes them when the last
#      connection closes cleanly, so their presence means somebody has the
#      store open right now (or a process died holding it).
#   2. fuser, where available: catches a process holding the file whose
#      sidecars have already been checkpointed away.
#   3. BEGIN IMMEDIATE, which fails when a writer holds the write lock.
#      This one is necessary but far from sufficient: a sync writes in
#      batches and is idle between them, so a probe that happens to land in
#      a gap sees nothing. That is exactly why check 1 exists.
#
# Usage: scripts/vacuum-store.sh STORE
#   e.g. scripts/vacuum-store.sh scratch/lag.sqlite
set -euo pipefail

usage() {
    echo "usage: $(basename "$0") STORE" >&2
    echo "  Compacts a koji-lag store in place. Refuses if it is in use." >&2
    exit 2
}

[[ $# -eq 1 ]] || usage
STORE=$1
[[ -f $STORE ]] || { echo "error: no such store: $STORE" >&2; exit 1; }

busy() {
    echo "error: $STORE is in use ($1)" >&2
    echo "       VACUUM needs the store to itself; wait for the sync to finish." >&2
    exit 1
}

# 1. Sidecars, checked before this script opens anything itself — our own
#    connection would create them.
for sidecar in "$STORE-wal" "$STORE-shm"; do
    [[ -e $sidecar ]] && busy "$(basename "$sidecar") exists, so a connection is open"
done

# 2. Anyone holding the file. Only advisory: fuser is not everywhere, and a
#    reader counts too, which for VACUUM is the right answer anyway.
if command -v fuser >/dev/null 2>&1 && fuser -s "$STORE" 2>/dev/null; then
    busy "another process has it open"
fi

# 3. An active writer.
if ! sqlite3 "$STORE" "BEGIN IMMEDIATE; ROLLBACK;" 2>/dev/null; then
    busy "a writer holds the lock"
fi

before=$(stat -c %s "$STORE")
# VACUUM builds a complete second copy before swapping it in, so the peak
# need is roughly twice the store. The temp copy lands in SQLITE_TMPDIR (or
# the store's own directory) — check both when they differ.
need=$((before * 2))
for dir in "$(dirname "$STORE")" "${SQLITE_TMPDIR:-/tmp}"; do
    free=$(df -B1 --output=avail "$dir" | tail -1)
    if (( free < need )); then
        echo "error: $dir has $((free / 1024 / 1024))MB free, VACUUM needs about \
$((need / 1024 / 1024))MB (twice the store)" >&2
        exit 1
    fi
done

echo "compacting $STORE ($((before / 1024 / 1024))MB)"
start=$SECONDS
sqlite3 "$STORE" "VACUUM"
elapsed=$((SECONDS - start))

# A rebuilt database that nobody checked is a rebuilt database nobody knows
# is sound, and this one replaced the original in place.
echo -n "verifying: "
check=$(sqlite3 "$STORE" "PRAGMA integrity_check")
if [[ $check != "ok" ]]; then
    echo "FAILED"
    echo "error: the compacted store did not verify: $check" >&2
    exit 1
fi
after=$(stat -c %s "$STORE")
echo "ok"
printf 'reclaimed %sMB of %sMB in %ss (now %sMB)\n' \
    "$(( (before - after) / 1024 / 1024 ))" \
    "$((before / 1024 / 1024))" "$elapsed" "$((after / 1024 / 1024))"
