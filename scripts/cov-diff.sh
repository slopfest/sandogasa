#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Line coverage of the Rust files a change touches: every file changed
# since BASE (default: where HEAD left origin/main), committed or not,
# plus new files not yet added. Runs the tests of the crates those files
# belong to once, under cargo-llvm-cov, and lists each file's coverage,
# marking those under the workspace gate. This is the per-issue check
# CLAUDE.md asks for before a commit that closes an issue; `make cov`
# remains the release gate over the whole workspace.
#
# Usage: scripts/cov-diff.sh [BASE]
# Exits 1 when a changed file is under the gate, 2 on a usage error.
set -euo pipefail
cd "$(dirname "$0")/.."

# Keep in step with `cov` in .cargo/config.toml.
gate=$(sed -n '/"--fail-under-lines",/{n;s/[^0-9.]//g;p;}' .cargo/config.toml)
[ -n "$gate" ] || { echo "error: no --fail-under-lines in .cargo/config.toml" >&2; exit 2; }

base=${1:-$(git merge-base HEAD origin/main)}
git rev-parse --verify --quiet "$base^{commit}" >/dev/null ||
  { echo "error: $base is not a commit" >&2; exit 2; }

# Changed .rs files that still exist, binary entry points left out as
# `make cov` leaves them out.
mapfile -t files < <(
  { git diff --name-only "$base" -- '*.rs'
    git ls-files --others --exclude-standard -- '*.rs'; } |
    grep -E '^(crates|tools)/[^/]+/' | grep -v '/src/main\.rs$' |
    sort -u | while read -r f; do [ -f "$f" ] && echo "$f"; done
)
if [ "${#files[@]}" -eq 0 ]; then
  echo "no Rust files changed since $(git rev-parse --short "$base")"
  exit 0
fi

# A crate's directory is named after the package (checked by hand for
# every member; see the workspace layout in CLAUDE.md).
mapfile -t crates < <(printf '%s\n' "${files[@]}" | cut -d/ -f2 | sort -u)
echo "Measuring ${#files[@]} changed file(s) in: ${crates[*]}" >&2

json=$(mktemp)
trap 'rm -f "$json"' EXIT
cargo llvm-cov test --json --summary-only \
  --ignore-filename-regex 'src/main\.rs$' \
  $(printf -- '-p %s ' "${crates[@]}") >"$json"

root=$(pwd)
status=0
printf '%8s  %s\n' "lines" "file"
for f in "${files[@]}"; do
  pct=$(jq -r --arg f "$root/$f" \
    '[.data[0].files[] | select(.filename == $f) | .summary.lines.percent][0] // empty' "$json")
  if [ -z "$pct" ]; then
    # No code llvm-cov counts, e.g. a file of tests or constants only.
    printf '%8s  %s\n' "-" "$f"
    continue
  fi
  mark=""
  if awk -v p="$pct" -v g="$gate" 'BEGIN { exit !(p < g) }'; then
    mark="  under the $gate% gate"
    status=1
  fi
  printf '%7.2f%%  %s%s\n' "$pct" "$f" "$mark"
done
exit "$status"
