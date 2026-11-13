#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Compile each talk under talks/ (talks/<name>/<name>.typ) to a PDF next
# to its source, with typst (`cargo binstall typst-cli`). The PDFs are
# git-ignored: they are build output, attached to the release instead.
# Pass talk directories to build only those.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v typst >/dev/null; then
    echo "typst not found: install it with 'cargo binstall typst-cli'" >&2
    exit 1
fi

dirs=("$@")
if [ ${#dirs[@]} -eq 0 ]; then
    dirs=(talks/*/)
fi

for dir in "${dirs[@]}"; do
    dir=${dir%/}
    src="$dir/$(basename "$dir").typ"
    if [ ! -f "$src" ]; then
        echo "skipping $dir: no $src" >&2
        continue
    fi
    echo "typst compile $src"
    typst compile "$src"
done
