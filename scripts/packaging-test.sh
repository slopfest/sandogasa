#!/bin/sh
# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Run the workspace test suite the way a distro packaging build does, to
# catch a test that wouldn't pass in a Koji / Debian buildd %check:
#
#   1. external network blocked, loopback up — so a test that calls a
#      real remote service fails, but localhost mocks (wiremock, used by
#      the HTTP-client crates) still bind; and
#   2. distro-specific tools absent — a buildroot has the base toolchain
#      + git, not gbp/dput/koji/fedrq/distro-info/... So we shadow those
#      with failing stubs: a test that shells out to one errors instead
#      of silently passing just because it's installed on the dev box.
#
# Local git fixtures and coreutils stay real (every buildroot has them).
# Requires unprivileged user namespaces (for `unshare -rn`), enabled by
# default on current Debian/Ubuntu/Fedora. Extra args are forwarded to
# `cargo test` — e.g. `scripts/packaging-test.sh -p dbranch`.
set -eu

# Shadow distro tools with stubs that fail loudly if a test invokes one.
# (git, touch, false, cc, rustc, cargo, … are intentionally left real.)
STUB="$(mktemp -d)"
trap 'rm -rf "$STUB"' EXIT
for t in gbp dput debuild dh uscan pristine-tar lintian pbuilder-dist \
    ubuntu-distro-info debian-distro-info glab koji fedrq curl \
    rpm dnf rpmbuild mock osc dpkg dpkg-buildpackage sbuild; do
    printf '#!/bin/sh\necho "packaging-test: %s must not be run in tests (distro tool)" >&2\nexit 127\n' \
        "$t" >"$STUB/$t"
    chmod +x "$STUB/$t"
done

# Default to the whole workspace; any args (e.g. `-p dbranch`, a test
# name filter) replace that so you can scope the run.
if [ "$#" -eq 0 ]; then
    set -- --workspace
fi

# Build the test binaries first, WITH network and the real PATH — the
# isolated run below can't reach crates.io to fetch or build deps.
cargo test --no-run "$@"

# Re-run with distro tools shadowed and in a fresh network namespace (no
# route off the box), loopback up so localhost mock servers can still
# bind. A bare `unshare -rn` leaves loopback DOWN, which makes wiremock
# tests fail spuriously — don't diagnose without `ip link set lo up`.
PATH="$STUB:$PATH" unshare -rn sh -c 'ip link set lo up && cargo test "$@"' sh "$@"
