#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Regenerate every tool's man page from its clap definition.
#
# Each tool checks its committed page in a unit test; setting
# SANDOGASA_UPDATE_MAN makes that same test rewrite the page instead
# (see crates/sandogasa-cli/src/man.rs). Review the result with
# `git diff` — the pages are committed so packagers do not have to
# run the binaries to build them.
set -euo pipefail
cd "$(dirname "$0")/.."
SANDOGASA_UPDATE_MAN=1 cargo test --workspace man_page_matches_cli "$@"
echo
echo "Regenerated:"
git status --porcelain -- 'tools/*/man/*.1' | sed 's/^/  /'
