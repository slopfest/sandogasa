#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Build sandogasa's development trajectory report (#20): measure every
# release tag with reports/trajectory/trajectory.py, then draw it with
# typst into reports/trajectory/trajectory.pdf. The JSON and the PDF
# are git-ignored: build output, attached to each release instead.
#
#   UNTIL=<tag>  stop at that tag (the release job builds a release's own)
#   HEAD=1       add a row for HEAD after the last tag
#   PRIVATE=1    add the weekday x hour heatmap of commit times, into
#                trajectory-private.pdf; for your own use, never attached
set -euo pipefail

cd "$(dirname "$0")/.."

for tool in git python3 tokei typst; do
    if ! command -v "$tool" >/dev/null; then
        echo "$tool not found; the report needs git, python3, tokei and typst" >&2
        exit 1
    fi
done

dir=reports/trajectory
args=()
[ -n "${UNTIL:-}" ] && args+=(--until "$UNTIL")
[ -n "${HEAD:-}" ] && args+=(--head)
name=trajectory
if [ -n "${PRIVATE:-}" ]; then
    args+=(--private)
    name=trajectory-private
fi

python3 "$dir/trajectory.py" "${args[@]}" -o "$dir/$name.json"
echo "typst compile $dir/trajectory.typ $dir/$name.pdf"
typst compile --input "data=$name.json" "$dir/trajectory.typ" "$dir/$name.pdf"
