#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Render each terminal recording under casts/ (casts/<name>.cast, made
# with `asciinema rec -i 2`) to an animated GIF next to it, with agg
# (https://github.com/asciinema/agg, 1.6 or later for asciinema 3's
# format). The GIFs are git-ignored: they are build output, attached to
# the release instead, as the talks' PDFs are. Pass .cast files to
# render only those.
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v agg >/dev/null; then
    # Not on crates.io: the crate named agg there is another project.
    echo "agg not found: install it with" \
        "'cargo binstall --git https://github.com/asciinema/agg agg'" >&2
    exit 1
fi

casts=("$@")
if [ ${#casts[@]} -eq 0 ]; then
    casts=(casts/*.cast)
fi

for cast in "${casts[@]}"; do
    if [ ! -f "$cast" ]; then
        echo "skipping $cast: no such file" >&2
        continue
    fi
    gif="${cast%.cast}.gif"
    echo "agg $cast $gif"
    # The recording's own idle-time limit applies; the fonts are a list
    # so the prompt's Powerline and symbol glyphs and emoji are found.
    agg --font-family "${AGG_FONTS:-DejaVu Sans Mono,PowerlineSymbols,Noto Sans Symbols 2,Noto Color Emoji}" \
        "$cast" "$gif" > /dev/null
done
