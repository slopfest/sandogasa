#!/bin/sh
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Run the workspace test suite the way a distro packaging build does:
# external network blocked, loopback up. This mirrors how Koji and
# Debian buildds run %check / dh_auto_test, and catches any test that
# reaches a real remote service. Localhost mocks (e.g. wiremock, which
# the HTTP-client crates use) still work because loopback is up.
#
# Requires unprivileged user namespaces (for `unshare -rn`); these are
# enabled by default on current Debian/Ubuntu/Fedora. Extra arguments
# are forwarded to `cargo test` — e.g. `scripts/offline-test.sh -p dbranch`.
set -eu

# Default to the whole workspace; any args (e.g. `-p dbranch`, a test
# name filter) replace that so you can scope the run.
if [ "$#" -eq 0 ]; then
    set -- --workspace
fi

# Build the test binaries first, WITH network — the isolated run below
# can't reach crates.io to fetch or build dependencies.
cargo test --no-run "$@"

# Re-run in a fresh network namespace (no route off the box), bringing
# loopback up so localhost mock servers can still bind. A bare
# `unshare -rn` leaves loopback DOWN, which makes wiremock tests fail
# spuriously — don't diagnose offline-safety without `ip link set lo up`.
exec unshare -rn sh -c 'ip link set lo up && exec cargo test "$@"' sh "$@"
