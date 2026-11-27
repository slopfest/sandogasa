#!/bin/sh
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Lint the tree's Markdown with markdownlint-cli2, the same rules the
# Markdown workflow runs (see .markdownlint-cli2.jsonc).
set -eu

if ! command -v markdownlint-cli2 >/dev/null 2>&1; then
    echo "check-markdown: markdownlint-cli2 not found; install it with" \
        "'dnf install markdownlint-cli2' or" \
        "'npm install -g markdownlint-cli2'" >&2
    exit 1
fi

cd "$(dirname "$0")/.."
markdownlint-cli2
