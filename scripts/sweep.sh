#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Delete the build trees that exist only to answer a question already
# answered. The release gates are what fill this repository's target
# directory: `cargo semver-checks` builds rustdoc for the current *and*
# the published version of every library crate, and `cargo cov` builds a
# separately instrumented copy of the whole workspace. Both are caches
# keyed on versions that just changed, so after a release they are dead
# weight -- 25GB of it, measured at 0.19.3 -- and a machine that runs out
# of disk mid-build is a worse outcome than a cold gate run next time.
#
# Deliberately left alone: target/debug and its incremental cache (1.3GB
# of the 3.3GB a full dev build writes), which is what makes day-to-day
# builds fast. Turning incremental off would trade the edit loop for
# disk, the wrong direction now that [profile.dev] debug =
# "line-tables-only" has halved what a build writes. When target/debug
# does have to go, `cargo clean` is the honest tool and it reports what
# it removed.
#
# The point is not that this frees more than `cargo clean` -- it frees
# less -- but that it costs nothing. `cargo clean` bills a full cold
# rebuild (two minutes for the workspace and its tests) and takes the
# incremental cache with it; this removes only caches whose baseline has
# already moved on, so the next build is still incremental.
#
# When to run it:
#   - right after a release, which is what the checklist says: the
#     baseline semver-checks cached is the version just superseded, so
#     that tree gets rebuilt next time regardless
#   - when the disk is tight but work continues
#   - after a one-off `make cov` or `make semver-checks`, since neither
#     runs again until the next release
#
# When not to: while iterating on coverage or on a suspected semver
# break, running the same gate repeatedly. Then those trees are live
# caches and removing them makes every iteration a full instrumented
# rebuild.
#
# Invoked through the task runner:
#   make sweep
set -euo pipefail

cd "$(dirname "$0")/.."

if ! grep -q '^\[workspace\]' Cargo.toml 2>/dev/null; then
    echo "sweep: not at the workspace root (no [workspace] in Cargo.toml)" >&2
    exit 1
fi

if [[ ! -d target ]]; then
    echo "sweep: no target directory; nothing to do"
    exit 0
fi

# Sizes are reported from before and after rather than summed per path,
# so the number is what the filesystem actually gave back.
before=$(du -sk target | cut -f1)

for tree in target/semver-checks target/llvm-cov-target target/package target/doc; do
    if [[ -d $tree ]]; then
        echo "sweep: removing $tree ($(du -sh "$tree" | cut -f1))"
        rm -rf "$tree"
    fi
done

# Coverage runs leave raw profiles behind even when their target tree is
# already gone.
profraw=$(find target -name '*.profraw' -type f 2>/dev/null | wc -l)
if [[ $profraw -gt 0 ]]; then
    echo "sweep: removing $profraw *.profraw file(s)"
    find target -name '*.profraw' -type f -delete
fi

after=$(du -sk target | cut -f1)
freed=$(( (before - after) / 1024 ))
echo "sweep: reclaimed ${freed}MB; target is now $(du -sh target | cut -f1)"
echo "sweep: target/debug is left in place -- use 'cargo clean' to drop it too"
