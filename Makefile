# SPDX-License-Identifier: Apache-2.0 OR MIT
#
# Task runner for development. Cargo remains the build system — this
# only gives the workspace's checks and scripts one discoverable place,
# so `make help` lists what is available instead of leaving it to be
# found in scripts/ and .cargo/config.toml. Distro packaging drives
# cargo directly and does not use this file.

CARGO ?= cargo
# Library crates only: semver-checks has nothing to say about binaries.
LIB_CRATES := $(notdir $(wildcard crates/*))

.DEFAULT_GOAL := help

.PHONY: help
help: ## List the available targets
	@awk 'BEGIN {FS = ":.*## "} /^[a-z][a-z-]*:.*## / {printf "  %-16s %s\n", $$1, $$2}' $(MAKEFILE_LIST)

.PHONY: build
build: ## Build the workspace (debug)
	$(CARGO) build --workspace

.PHONY: release
release: ## Build the workspace with optimizations
	$(CARGO) build --workspace --release

.PHONY: test
test: ## Run the workspace test suite
	$(CARGO) test --workspace

.PHONY: fmt
fmt: ## Format the workspace in place
	$(CARGO) fmt --all

.PHONY: clippy
clippy: ## Lint the workspace, tests included
	$(CARGO) clippy --workspace --all-targets

.PHONY: man
man: ## Regenerate every tool's man page from its clap definition
	./scripts/gen-man.sh

.PHONY: cov
cov: ## Report test coverage, failing under 80% lines
	$(CARGO) cov

.PHONY: audit
audit: ## Check dependencies against RustSec advisories
	$(CARGO) audit

.PHONY: semver-checks
semver-checks: ## Check the library crates for semver breakage
	$(CARGO) semver-checks $(addprefix --package ,$(LIB_CRATES))

.PHONY: packaging-test
packaging-test: ## Run the tests as a distro build does (offline, no distro tools)
	./scripts/packaging-test.sh

.PHONY: check-published
check-published: ## Verify every crate reached crates.io (run after publishing)
	./scripts/check-published.sh

.PHONY: backup-store
backup-store: ## Copy a koji-lag store safely (STORE=... DEST=...)
	./scripts/backup-store.sh $(STORE) $(DEST)

.PHONY: vacuum-store
vacuum-store: ## Compact a koji-lag store in place (STORE=...)
	./scripts/vacuum-store.sh $(STORE)

.PHONY: sweep
sweep: ## Delete the release gates' build trees (keeps target/debug)
	./scripts/sweep.sh

.PHONY: srht-schemas
srht-schemas: ## Refresh the vendored sr.ht GraphQL schemas (needs network)
	./scripts/update-srht-schemas.sh

.PHONY: check
check: ## Everything a pull request should pass
	$(CARGO) fmt --all --check
	$(MAKE) clippy
	$(MAKE) test
	$(MAKE) packaging-test

.PHONY: release-checks
release-checks: check ## The pre-tagging gates: check, plus audit, semver, coverage
	$(MAKE) audit
	$(MAKE) semver-checks
	$(MAKE) cov

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
