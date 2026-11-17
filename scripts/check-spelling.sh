#!/bin/sh
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Check the tree's spelling with typos-cli, in en-US (see _typos.toml).
# CHANGELOG.md is excluded there because released entries are not
# edited, so its Unreleased section is checked here on its own.
set -eu

if ! command -v typos >/dev/null 2>&1; then
    echo "check-spelling: typos not found; install it with" \
        "'cargo binstall typos-cli' or 'cargo install typos-cli'" >&2
    exit 1
fi

cd "$(dirname "$0")/.."
typos
awk '/^## Unreleased/ { on = 1; next } /^## / { on = 0 } on' CHANGELOG.md |
    typos -
