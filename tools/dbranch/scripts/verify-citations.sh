#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Check dbranch's citations of Debian Policy and the Developer's
# Reference against local clones of both, after making sure the
# clones are current: a citation checked against a stale clone can be
# wrong in the same way the documents can.
#
#   tools/dbranch/scripts/verify-citations.sh POLICY_CLONE DEVREF_CLONE
#
# POLICY_CLONE is a clone of https://salsa.debian.org/dbnpolicy/policy
# and DEVREF_CLONE one of
# https://salsa.debian.org/debian/developers-reference.
#
# A clone counts as stale when it was last fetched more than
# DBRANCH_DOCS_MAX_AGE_DAYS days ago (default 14), or is behind the
# upstream it has already fetched. At a terminal the script then
# offers to refresh: it fetches the stale clones' upstream remotes in
# parallel, and fast-forwards a clone only when it is on its default
# branch with no local changes and the merge is a fast-forward.
# Anything else is reported and left alone, so other remotes (such as
# a fork kept for documentation patches) and local branches are never
# touched. Without a terminal it only reports.

set -u

if [ $# -ne 2 ]; then
    echo "usage: $0 POLICY_CLONE DEVREF_CLONE" >&2
    exit 2
fi
max_days=${DBRANCH_DOCS_MAX_AGE_DAYS:-14}
case $max_days in
    '' | *[!0-9]*)
        echo "DBRANCH_DOCS_MAX_AGE_DAYS must be a number of days, not '$max_days'" >&2
        exit 2
        ;;
esac
policy=$(realpath -e "$1") || exit 2
devref=$(realpath -e "$2") || exit 2
now=$(date +%s)

for repo in "$policy" "$devref"; do
    if ! git -C "$repo" rev-parse --git-dir >/dev/null 2>&1; then
        echo "$repo is not a git clone" >&2
        exit 2
    fi
done

# The upstream of the checked-out branch (origin/master), or nothing.
upstream_of() {
    git -C "$1" rev-parse --abbrev-ref '@{u}' 2>/dev/null
}

# When the clone last heard from its upstream, in seconds since the
# epoch: FETCH_HEAD's age when the last fetch was from the upstream's
# remote (a fetch of another remote, such as a fork, rewrites it too),
# or the last change to the upstream ref, which the clone recorded
# itself, as for a clone never fetched since it was made. The date of
# the last commit is not used: a quiet repository is not a stale
# clone.
last_fetch() {
    local repo=$1 upstream=$2 newest=0 t fetch_head url
    fetch_head=$(git -C "$repo" rev-parse --path-format=absolute --git-path FETCH_HEAD)
    url=$(git -C "$repo" remote get-url "${upstream%%/*}" 2>/dev/null)
    if [ -e "$fetch_head" ] && [ -n "$url" ] && grep -qF " of $url" "$fetch_head"; then
        newest=$(stat -c %Y "$fetch_head")
    fi
    # A fresh clone records itself in the reflog of <remote>/HEAD,
    # not of the branch it checked out, so read both.
    for ref in "$upstream" "${upstream%%/*}/HEAD"; do
        # The entry's own time, not the commit's (%ct would be when
        # upstream last committed). %gd with unix dates is ref@{time}.
        t=$(git -C "$repo" log -g -1 --date=unix --format=%gd "$ref" 2>/dev/null |
            sed -n 's/.*@{\([0-9]*\)}$/\1/p')
        if [ -n "$t" ] && [ "$t" -gt "$newest" ]; then
            newest=$t
        fi
    done
    echo "$newest"
}

stale=()
for repo in "$policy" "$devref"; do
    upstream=$(upstream_of "$repo")
    if [ -z "$upstream" ]; then
        echo "$repo: the checked-out branch tracks nothing, so its age cannot be told"
        continue
    fi
    fetched=$(last_fetch "$repo" "$upstream")
    behind=$(git -C "$repo" rev-list --count "HEAD..$upstream")
    if [ "$fetched" -eq 0 ]; then
        echo "$repo: when it was last fetched is unknown, $behind commits behind $upstream"
        echo "    refresh by hand: mr -d $repo update"
        stale+=("$repo")
        continue
    fi
    days=$(( (now - fetched) / 86400 ))
    if [ "$days" -gt "$max_days" ] || [ "$behind" -gt 0 ]; then
        echo "$repo: last fetched $days days ago, $behind commits behind $upstream"
        echo "    refresh by hand: mr -d $repo update"
        stale+=("$repo")
    else
        echo "$repo: fetched $days days ago, up to date with $upstream"
    fi
done

if [ ${#stale[@]} -gt 0 ] && [ -t 0 ]; then
    read -r -p "Refresh the stale clones now? [Y/n] " answer
    case $answer in
        [nN]*) ;;
        *)
            echo "Fetching ${#stale[@]} documentation clone(s) ..."
            pids=()
            for repo in "${stale[@]}"; do
                upstream=$(upstream_of "$repo")
                git -C "$repo" fetch --quiet "${upstream%%/*}" &
                pids+=($!)
            done
            for i in "${!pids[@]}"; do
                wait "${pids[$i]}" || echo "${stale[$i]}: fetch failed" >&2
            done
            for repo in "${stale[@]}"; do
                upstream=$(upstream_of "$repo")
                default=$(git -C "$repo" rev-parse --abbrev-ref "${upstream%%/*}/HEAD" 2>/dev/null)
                if [ "$upstream" != "$default" ]; then
                    echo "$repo: left alone, not on its default branch (${default:-unknown})"
                elif [ -n "$(git -C "$repo" status --porcelain --untracked-files=no)" ]; then
                    echo "$repo: left alone, it has local changes"
                elif [ "$(git -C "$repo" rev-parse HEAD)" = "$(git -C "$repo" rev-parse "$upstream")" ]; then
                    echo "$repo: already at $upstream"
                elif ! git -C "$repo" merge-base --is-ancestor HEAD "$upstream"; then
                    echo "$repo: left alone, it has commits $upstream does not"
                else
                    git -C "$repo" merge --ff-only --quiet "$upstream" &&
                        echo "$repo: fast-forwarded to $upstream"
                fi
            done
            ;;
    esac
fi

# A citation checked against a stale clone proves nothing, so a clone
# still stale now fails the run even if the citations match it.
still_stale=()
for repo in "${stale[@]}"; do
    upstream=$(upstream_of "$repo")
    fetched=$(last_fetch "$repo" "$upstream")
    behind=$(git -C "$repo" rev-list --count "HEAD..$upstream")
    if [ "$fetched" -eq 0 ] || [ $(( (now - fetched) / 86400 )) -gt "$max_days" ] ||
        [ "$behind" -gt 0 ]; then
        still_stale+=("$repo")
    fi
done

cd "$(dirname "$0")/../../.." || exit 2
DBRANCH_POLICY_DIR=$policy/policy \
DBRANCH_DEVREF_DIR=$devref/source \
    cargo test -p dbranch --lib citations_match_the_local_documentation_clones \
    -- --ignored --nocapture || exit 1

if [ ${#still_stale[@]} -gt 0 ]; then
    echo "checked against out-of-date clones, so not verified:" >&2
    printf '    %s\n' "${still_stale[@]}" >&2
    exit 1
fi
