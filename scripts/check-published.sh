#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Verify every publishable workspace crate is on crates.io at the
# workspace version, and that each one still declares the
# dev-dependencies its source does. Run this after `cargo ws publish`
# and before tagging: publish can stop partway (a 429, an interrupted
# upload, a crate whose packaging fails) and a tag pointing at half a
# released workspace is worse than a tag that arrives late.
#
# The second check exists because four releases of dbranch shipped a
# Cargo.toml with no [dev-dependencies] at all while this script
# reported every crate as published. Our tests live in
# `#[cfg(test)] mod tests` inside src/, so they ship in the tarball,
# and debcargo runs `cargo test` at build time for any crate that
# looks dependency-free -- so the Debian build ran them and failed on
# a missing tempfile. Asking whether a crate landed is not the same as
# asking whether what landed can build its own tests.
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

# Crates whose source declares a dev-dependency: what the published
# tarball is then checked against.
with_dev=$(printf '%s' "$metadata" | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p.get("publish") == []:
        continue
    if any(d.get("kind") == "dev" for d in p["dependencies"]):
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
if [ ${#missing[@]} -ne 0 ]; then
    echo "${#missing[@]} of $total crates are not on crates.io at $version:"
    printf '  %s\n' "${missing[@]}"
    echo
    echo "re-run the publish — --publish-as-is skips what already landed:"
    echo "  cargo ws publish --publish-as-is --publish-interval 20 \\"
    echo "    --no-git-commit --allow-dirty"
    exit 1
fi

# Every crate is there; now look inside the ones that should carry
# dev-dependencies. Fetching from static.crates.io rather than the
# API: it serves the tarball that was actually uploaded.
ndev=$(printf '%s\n' "$with_dev" | grep -c . || true)
echo "checking the published Cargo.toml of $ndev crates with dev-dependencies"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

stripped=()
for crate in $with_dev; do
    tarball="$work/$crate.crate"
    if ! curl -sSL --max-time 60 -A "$UA" \
        "https://static.crates.io/crates/$crate/$crate-$version.crate" \
        -o "$tarball"; then
        printf '  SKIP     %s (could not download)\n' "$crate"
        continue
    fi
    if tar -xOf "$tarball" "$crate-$version/Cargo.toml" 2>/dev/null |
        grep -q '^\[dev-dependencies'; then
        printf '  ok       %s\n' "$crate"
    else
        printf '  STRIPPED %s\n' "$crate"
        stripped+=("$crate")
    fi
    sleep 0.3
done

echo
if [ ${#stripped[@]} -ne 0 ]; then
    echo "${#stripped[@]} of $ndev crates lost their dev-dependencies when published:"
    printf '  %s\n' "${stripped[@]}"
    echo
    echo "their source declares dev-dependencies, the published Cargo.toml"
    echo "has none, so the tests that ship in the tarball cannot build."
    echo "Compare for yourself:"
    echo "  cargo package -p ${stripped[0]} --no-verify --allow-dirty"
    echo "  tar -xOf target/package/${stripped[0]}-$version.crate \\"
    echo "    ${stripped[0]}-$version/Cargo.toml | grep -A3 dev-dependencies"
    exit 1
fi
if [ "$ndev" -gt 0 ]; then
    echo "all $ndev kept their dev-dependencies"
fi
echo "all $total crates are published at $version"
exit 0
