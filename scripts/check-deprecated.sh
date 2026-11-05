#!/bin/bash
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Fail when our code uses a deprecated API, with every feature on, or
# silences the lint for one. A RustSec "patched" release can fix an
# advisory by deprecating the unsafe API rather than removing it —
# tokio 1.54.1 deprecated the unsound AsyncFd constructors of
# RUSTSEC-2026-0386 and kept them — so a deprecation can be the only
# trace of a security issue. `make clippy` already fails on one, but
# only for default features; this covers the rest. Dependencies are
# another matter, as lints are capped for their code: search their
# sources with scripts/find-api-use.sh.
set -euo pipefail
cd "$(dirname "$0")/.."

status=0

# A crate-wide or local #[allow(deprecated)] would hide exactly what
# this looks for.
if grep -rnE 'allow\((.*[, ])?deprecated([,)]| )' --include='*.rs' crates tools; then
  echo "error: the lines above allow deprecated APIs; call the replacement instead" >&2
  status=1
fi

log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! cargo check --workspace --all-targets --all-features --color never >"$log" 2>&1; then
  cat "$log" >&2
  echo "error: cargo check --all-features failed" >&2
  exit 1
fi
if grep -q 'use of deprecated' "$log"; then
  grep -A6 'use of deprecated' "$log" >&2
  echo "error: deprecated APIs are used (see above)" >&2
  status=1
fi

[ "$status" -eq 0 ] && echo "no deprecated APIs used, with all features"
exit "$status"
