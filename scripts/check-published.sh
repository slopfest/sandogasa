#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Verify every publishable workspace crate is on crates.io at the
# workspace version. Run this after `cargo ws publish` and before
# tagging: publish can stop partway (a 429, an interrupted upload, a
# crate whose packaging fails) and a tag pointing at half a released
# workspace is worse than a tag that arrives late.
#
# Pass a version to check one other than the current workspace
# version:  scripts/check-published.sh 0.19.0
#
# crates.io requires a User-Agent under its API data access policy
# (https://crates.io/data-access). Without one it answers 200 with an
# error body instead of a version, so a check that omits the header
# reports every crate as missing — override with CRATES_IO_USER_AGENT
# if you want your own contact string.
set -euo pipefail
cd "$(dirname "$0")/.."

UA="${CRATES_IO_USER_AGENT:-sandogasa-release-check (https://github.com/slopfest/sandogasa)}"

# Publishable workspace members, and the version to look for. Taken
# from cargo metadata rather than a directory listing so a crate
# marked `publish = false` is not demanded on crates.io.
metadata=$(cargo metadata --format-version 1 --no-deps)
version="${1:-$(printf '%s' "$metadata" | python3 -c '
import json, sys
packages = json.load(sys.stdin)["packages"]
print(sorted({p["version"] for p in packages})[-1])')}"
crates=$(printf '%s' "$metadata" | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    # `publish` is None when unrestricted, [] when disabled.
    if p.get("publish") != []:
        print(p["name"])' | sort)

total=$(printf '%s\n' "$crates" | wc -l)
echo "checking $total crates at $version on crates.io"

missing=()
for crate in $crates; do
    found=$(curl -sS --max-time 20 -A "$UA" \
        "https://crates.io/api/v1/crates/$crate/$version" |
        python3 -c '
import json, sys
try:
    print(json.load(sys.stdin)["version"]["num"])
except Exception:
    print("")' 2>/dev/null || true)
    if [ "$found" = "$version" ]; then
        printf '  ok       %s\n' "$crate"
    else
        printf '  MISSING  %s\n' "$crate"
        missing+=("$crate")
    fi
    # Stay well clear of the API's request limits.
    sleep 0.3
done

echo
if [ ${#missing[@]} -eq 0 ]; then
    echo "all $total crates are published at $version"
    exit 0
fi
echo "${#missing[@]} of $total crates are not on crates.io at $version:"
printf '  %s\n' "${missing[@]}"
echo
echo "re-run the publish — --publish-as-is skips what already landed:"
echo "  cargo ws publish --publish-as-is --publish-interval 20 \\"
echo "    --no-git-commit --allow-dirty"
exit 1
