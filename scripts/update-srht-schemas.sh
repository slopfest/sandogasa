#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Refresh the vendored sr.ht GraphQL schemas under
# crates/sandogasa-sourcehut/schema/. These are reference-only (not build
# inputs); re-run when sr.ht evolves its API and diff the result.
#
# NOTE: sr.ht UA-blocks some automated fetchers, but curl works fine. Run
# this on a normal machine (it needs outbound network to git.sr.ht).
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$repo_root/crates/sandogasa-sourcehut/schema"
mkdir -p "$dest"

# service -> upstream repo slug on git.sr.ht (schema at api/graph/schema.graphqls)
services=(git todo lists)

for svc in "${services[@]}"; do
    url="https://git.sr.ht/~sircmpwn/${svc}.sr.ht/blob/master/api/graph/schema.graphqls"
    out="$dest/${svc}.graphqls"
    echo "fetching ${svc}.sr.ht schema -> ${out#"$repo_root"/}"
    curl -fsSL "$url" -o "$out"
done

echo "done. review changes with: git -C '$repo_root' diff -- crates/sandogasa-sourcehut/schema"
