#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Search the source of every crate in Cargo.lock for a pattern, e.g.
# the API a RustSec advisory names:
#
#   scripts/find-api-use.sh 'AsyncFd::(new|try_new|with_interest|try_with_interest)'
#
# Lints are capped for dependency code, so a dependency calling a
# deprecated, vulnerable API never warns; this is how to find one.
# Prints `crate version path` per file that matches (an advisory's own
# crate will usually match in its tests and changelog), and exits 1
# when anything outside that crate's tests, examples, benches and
# changelog matches.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ $# -ne 1 ] || [ -z "$1" ]; then
  echo "usage: $0 <extended-regex>" >&2
  exit 2
fi

cargo fetch --quiet
cargo metadata --format-version 1 | python3 -c '
import json, os, re, subprocess, sys

pattern = sys.argv[1]
meta = json.load(sys.stdin)
locked = {n["id"] for n in meta["resolve"]["nodes"]}
# Matches here are the advisory crate documenting or testing itself.
incidental = re.compile(r"^(tests|examples|benches)/|^CHANGELOG")
searched = 0
real = 0
for pkg in meta["packages"]:
    if pkg["id"] not in locked or pkg["source"] is None:
        continue  # our own crates are covered by the deprecated lint
    searched += 1
    root = os.path.dirname(pkg["manifest_path"])
    found = subprocess.run(
        ["grep", "-rlE", "--", pattern, root], capture_output=True, text=True
    ).stdout.split()
    for path in sorted(found):
        rel = os.path.relpath(path, root)
        note = "" if not incidental.search(rel) else "  (tests/docs only)"
        if not note:
            real += 1
        name, version = pkg["name"], pkg["version"]
        print(f"{name} {version} {rel}{note}")
print(f"{searched} locked dependencies searched; {real} file(s) outside tests and docs match",
      file=sys.stderr)
sys.exit(1 if real else 0)
' "$1"
