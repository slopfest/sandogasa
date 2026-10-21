#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Render a markdown document for people who would rather read a
# document than a file in a git forge — FINDINGS by default.
#
# The output is an artefact, not content: it is gitignored, attached
# to a release, and regenerated from the tag rather than committed.
#
# HTML needs nothing but pandoc and is always produced. PDF needs
# xelatex and a score of style files, and LaTeX stops at the *first*
# one it cannot find — so a minimal TeX install is otherwise
# discovered one package per run. The list is read from the LaTeX
# pandoc actually generates, with the same options as the real
# render, rather than hardcoded here where it would drift.
set -euo pipefail
cd "$(dirname "$0")/.."

SRC=${1:-tools/koji-lag/FINDINGS.md}
OUT=${SRC%.md}
# Shared so the probe sees what the render will: `geometry` and
# `hyperref` are pulled in by these options and by nothing else.
PDFOPTS=(--toc -V geometry:margin=1in -V colorlinks=true)

command -v pandoc >/dev/null || {
    echo "render-findings: pandoc is not installed" >&2
    exit 1
}
[ -f "$SRC" ] || { echo "render-findings: no such file: $SRC" >&2; exit 1; }

pandoc "$SRC" -o "$OUT.html" -s --toc --embed-resources
echo "wrote $OUT.html"

if ! command -v xelatex >/dev/null; then
    echo "render-findings: xelatex not installed; HTML only." >&2
    echo "  print the HTML from a browser, or: sudo dnf install texlive-xetex" >&2
    exit 1
fi

# Over-reports slightly: pandoc emits both branches of its engine
# conditionals, so a pdflatex-only package counts as missing even
# though xelatex would not ask for it. Installing one is harmless.
TEX=$(pandoc -t latex -s "${PDFOPTS[@]}" "$SRC")

# Pandoc wraps the packages it can do without in \IfFileExists and
# falls back when they are absent — microtype, parskip, upquote, xurl,
# footnotehyper. Treating those as required is worse than not checking
# at all: it blocks a render that would have succeeded and sends you
# installing things pandoc never needed.
optional=$(printf '%s\n' "$TEX" |
    grep -o '\\IfFileExists{[^}]*\.sty}' | sed 's/.*{\(.*\)\.sty}/\1/' | sort -u)

# Asked for by `tex(NAME.sty)` rather than `texlive-NAME`: a style file
# need not be named after the package carrying it, and guessing that it
# is gets `footnote.sty` wrong — it lives in texlive-mdwtools. The
# virtual provide is what dnf resolves against, so it is right by
# construction for every one of them.
missing=()
while read -r sty; do
    [ -n "$sty" ] || continue
    printf '%s\n' "$optional" | grep -qx "$sty" && continue
    kpsewhich "$sty.sty" >/dev/null 2>&1 || missing+=("'tex($sty.sty)'")
done < <(printf '%s\n' "$TEX" |
    grep -o '\\usepackage\(\[[^]]*\]\)\?{[^}]*}' |
    sed 's/.*{\(.*\)}/\1/' | tr ',' '\n' | tr -d ' ' | sort -u)

if [ ${#missing[@]} -gt 0 ]; then
    echo "render-findings: HTML written; PDF needs these first:" >&2
    echo "  sudo dnf install -y ${missing[*]}" >&2
    echo "  (Debian: the same files are in texlive-latex-recommended and" >&2
    echo "   texlive-latex-extra; apt-file search <name>.sty to confirm.)" >&2
    exit 1
fi

pandoc "$SRC" -o "$OUT.pdf" --pdf-engine=xelatex "${PDFOPTS[@]}"
echo "wrote $OUT.pdf"
