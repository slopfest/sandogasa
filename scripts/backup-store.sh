#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Copy a koji-lag store safely, even while a sync is writing to it.
#
# Not `cp`. The store runs in WAL mode, so at any moment committed data
# lives partly in `lag.sqlite` and partly in `lag.sqlite-wal`; copying the
# main file alone can therefore capture a database missing its most recent
# transactions, and copying all three files while a writer is mid-commit can
# capture an inconsistent set. Neither failure announces itself — the copy
# opens and queries fine, with rows quietly missing.
#
# `VACUUM INTO` uses SQLite's own machinery: it reads a single consistent
# snapshot under the same locking a reader uses, and writes a fully
# checkpointed database with no sidecar files. It also rebuilds the file
# compactly, so the copy is usually smaller than the original.
#
# Usage: scripts/backup-store.sh SOURCE DEST
#   e.g. scripts/backup-store.sh scratch/lag.sqlite ~/Nextcloud/lag.sqlite
set -euo pipefail

usage() {
    echo "usage: $(basename "$0") SOURCE DEST" >&2
    echo "  Copies a koji-lag SQLite store, consistent even under a live sync." >&2
    exit 2
}

[[ $# -eq 2 ]] || usage
SOURCE=$1
DEST=$2

[[ -f $SOURCE ]] || { echo "error: no such store: $SOURCE" >&2; exit 1; }
# Refusing beats overwriting: the destination is usually a backup, and a
# half-written one is worse than none.
[[ -e $DEST ]] && { echo "error: $DEST exists; remove it or pick another name" >&2; exit 1; }

DEST_DIR=$(dirname "$DEST")
mkdir -p "$DEST_DIR"

# VACUUM INTO writes a whole second copy, so check there is room for one
# before spending minutes discovering there is not.
need=$(stat -c %s "$SOURCE")
free=$(df -B1 --output=avail "$DEST_DIR" | tail -1)
if (( free < need )); then
    echo "error: $DEST_DIR has $((free / 1024 / 1024))MB free, the store is $((need / 1024 / 1024))MB" >&2
    exit 1
fi

echo "copying $SOURCE -> $DEST ($((need / 1024 / 1024))MB)"
# The sqlite3 CLI where it exists, python3 otherwise: both use the same
# library, and neither is guaranteed to be installed.
if command -v sqlite3 >/dev/null 2>&1; then
    sqlite3 "$SOURCE" "VACUUM INTO '$DEST'"
elif command -v python3 >/dev/null 2>&1; then
    python3 - "$SOURCE" "$DEST" <<'PY'
import sqlite3, sys
src, dest = sys.argv[1], sys.argv[2]
with sqlite3.connect(f"file:{src}?mode=ro", uri=True) as conn:
    conn.execute("VACUUM INTO ?", (dest,))
PY
else
    echo "error: neither sqlite3 nor python3 found" >&2
    exit 1
fi

# A copy nobody checked is a hope, not a backup. This reads every page.
echo -n "verifying: "
check=$(sqlite3 "$DEST" "PRAGMA integrity_check" 2>/dev/null \
    || python3 -c "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute('PRAGMA integrity_check').fetchone()[0])" "$DEST")
if [[ $check != "ok" ]]; then
    echo "FAILED"
    echo "error: the copy did not verify: $check" >&2
    exit 1
fi

rows=$(sqlite3 "$DEST" "SELECT (SELECT count(*) FROM builds) || ' build(s), ' || (SELECT count(*) FROM tasks) || ' task(s)'" 2>/dev/null \
    || python3 -c "
import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
b = c.execute('select count(*) from builds').fetchone()[0]
t = c.execute('select count(*) from tasks').fetchone()[0]
print(f'{b} build(s), {t} task(s)')" "$DEST")
echo "ok — $rows"
echo "$(stat -c %s "$DEST" | awk '{printf "%.0f", $1/1024/1024}')MB written to $DEST"
