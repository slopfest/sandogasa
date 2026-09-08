# sandogasa-pkg-health

Audit package health across a [sandogasa](../..) inventory.

Each package is scored against a set of pluggable health checks —
open bugs, maintainer coverage, build status, etc. Checks are
classified by cost tier (cheap / medium / expensive) so you can
run them on different schedules.

pkg-health is the **observe** side of the inventory tooling:
read-only, no credentials needed, safe to run from cron. Acting on
what it finds — triaging and closing bugs, curating the inventory —
is [poi-tracker](../poi-tracker/)'s job. Rule of thumb: anything
that produces a report to watch over time belongs here; anything
that writes (to Bugzilla or the inventory) belongs in poi-tracker.

Reports persist to TOML and update incrementally: re-running a
single check (or just a subset of packages) preserves the stored
results of every other (package, check, variant) triple. Results
for version-parameterized checks like `bug_count` are tracked per
release so `f44`, `f45`, `epel10`, etc. can be aged independently.

## Installation

```sh
cargo install sandogasa-pkg-health
```

Requires a [sandogasa-inventory](../../crates/sandogasa-inventory/)
TOML file describing the packages to audit.

## Usage

### List available checks

```sh
sandogasa-pkg-health checks
```

### Run checks

```sh
# All cheap checks across the inventory.
sandogasa-pkg-health run -i inventory.toml -o health.toml --cheap

# A specific check.
sandogasa-pkg-health run -i inventory.toml -o health.toml \
    --check maintainer_count

# Bug count (Medium tier) across rawhide + specific releases.
sandogasa-pkg-health run -i inventory.toml -o health.toml \
    --check bug_count --fedora-version 44,45 --epel-version 10

# Only re-run results older than 7 days.
sandogasa-pkg-health run -i inventory.toml -o health.toml \
    --all --max-age 7d

# Only refresh one package.
sandogasa-pkg-health run -i inventory.toml -o health.toml \
    --all --package rust-arrow

# From a poi-tracker workspace: its `owned` inventory, and its closures'
# saved dependency graphs feed the dependency_health check.
sandogasa-pkg-health run -w kondo.toml -o health.toml --cheap

# The same with an explicit graph, two levels of dependencies deep.
sandogasa-pkg-health run -i inventory.toml -o health.toml --cheap \
    --graph fedora-build-deps-graph.json --dependency-depth 2
```

## Checks

- `bug_count` (Medium) — open bugs by category (security, FTBFS,
  update request, …) per release variant, classified via
  [sandogasa-bugclass](../../crates/sandogasa-bugclass/)
- `maintainer_count` (Cheap) — effective committer count from
  dist-git ACLs with Pagure group expansion. Also flags
  **orphaned** packages (dist-git owner is the `orphan` sentinel
  user, which is never counted as a maintainer) — an orphaned
  package is retired ~6 weeks after orphaning unless adopted;
  `poi-tracker adopt` is the action counterpart that takes
  ownership
- `pending_update` (Medium) — pending upstream update from the
  open release-monitoring bug, classified by semver impact
  (breaking / non-breaking, via
  [sandogasa-bugclass](../../crates/sandogasa-bugclass/)'s shared
  classifier — the same one poi-tracker's `semver-audit` uses).
  A spec already matching the advertised version is verified
  against rawhide's Koji tag chain before being called a stale
  bug: a build still in a side tag or gating reports as
  *committed, awaiting release* instead. Uses the `koji` CLI when
  available (`sudo dnf install koji`; queries are anonymous — no
  credentials involved) and degrades to the spec-only verdict
  with a startup warning when it's missing
- `dependency_health` (Cheap) — the health of what a package depends
  on, from a saved dependency graph; see below. Computed after the
  other checks rather than run on its own, and only when a graph is
  given (`--graph`, or the closures of a `-w` workspace)

Four checks read the workspace file (`-w kondo.toml`) rather than a
service, and are skipped when no workspace is given; they are the
kondo maintenance loop's questions asked as standing facts:

- `justified` (Cheap) — whether any essential inventory of the
  workspace names the package: a keep, a walked closure, a derived
  dependency inventory, a retired-but-kept list. One that none does is
  one `poi-tracker kondo` would offer to cull
- `dependents` (Cheap) — who depends on the package, off the branch's
  graph: a leaf nothing needs, one carried by other inventory packages,
  one needed only from outside the inventory; a package whose binaries
  are all `-devel` is marked `[devel-only]`, the library shape that is
  almost always a mere dependency
- `retired_kept` (Cheap) — retired in rawhide (dist-git says) yet kept
  as essential outside a `retired` inventory: a keep nobody revisited,
  which the walk will never find
- `orphan_acl` (Cheap) — orphaned in dist-git while the workspace's
  user still holds an ACL on it, the shape that keeps a package you gave
  up on your lists; adopt it back or drop the ACL

### Dependency health

A package can be in good shape itself while a library it depends on is
orphaned with a year-old security bug, and that is the package's
problem too. Given a graph saved by `poi-tracker deps --graph` — passed
with `--graph`, or found through the closures of a workspace file
(`-w kondo.toml`, which also supplies the `owned` inventory as the
default `-i`) — `run` reads each package's dependencies off it at the
source level, runs `maintainer_count` and `bug_count` (rawhide) on the
dependencies that are not in the inventory, and stores a
`dependency_health` reading per package, kept apart from the package's
own results so the report can say "the package is fine, its
dependencies are not".

The reading aggregates by worst offender with attribution, not by
average — one orphaned dependency among fifty healthy ones is the
finding — and names the fix's shape: the worst dependency and why, as
a direct run-time dependency, a direct build-only one, or a transitive
one; counts of dependencies with open security bugs, orphaned, or down
to a single maintainer; and the age of open security bugs pooled over
the bugs across the set, median and p90 with the n. Direct
dependencies are read in full. Deeper levels are walked only to
`--dependency-depth` (default 1, the direct ones): through build
dependencies nearly every package reaches the whole toolchain within a
few levels, and a dependency's own dependency problems belong in its
row rather than up every path.

```
rust-radix-heap:
  dependency_health: 34 (8 direct, 26 transitive)
    worst: rust-srpm-macros  orphaned  [transitive]
    6 with open security bugs, 1 orphaned
    security bug age across deps: p50 35 d, p90 70 d (n=37)
    transitive: 7 need attention (curl, gawk, glibc, libssh2, openssl, rust-srpm-macros, sqlite)
  maintainer_count: 10 effective (1 direct via rust-sig)
```

The dependencies' own results are stored in the report too, under
their names, so `show` lists them and a later run reuses them under
`--max-age`. A dependency the graph names but no check has reached yet
is counted as "not yet checked". A graph is a snapshot of the
repositories: `run` says when one is more than a month old, and before
a dependency is reported as needing attention it asks fedrq whether the
branch still has a package of that name — one that is gone (retired
since the walk, or a name from another branch's repositories) is
listed as such and not counted.

### Show a previously-generated report

```sh
sandogasa-pkg-health show health.toml
sandogasa-pkg-health show health.toml --package rust-arrow
sandogasa-pkg-health show health.toml --json
```

`show` does not touch the report file or query any external services;
it just renders what's already stored.

## JSON Schema

A JSON Schema for the report format is checked in at
[`data/health-report.schema.json`](data/health-report.schema.json).
It is generated from the Rust types via `schemars` and verified by
a test.

When the data model changes, update the schema:

```sh
UPDATE_SCHEMA=1 cargo test -p sandogasa-pkg-health schema_up_to_date
```

## Project status

MVP complete — framework, three checks (`bug_count`,
`maintainer_count`, `pending_update`), report persistence with
selective update, per-package parallelism, human-readable summary,
JSON output, and `show` subcommand. See [PLAN.md](PLAN.md) for
architecture and [TODO.md](TODO.md) for post-MVP roadmap.

## System-wide configuration

This tool keeps no settings of its own, but it does read a `[defaults]`
table — for pinning the flags you always pass — from
`/etc/sandogasa-pkg-health/config.toml` and
`~/.config/sandogasa-pkg-health/config.toml`, the user file overriding
the system one per key and command-line flags overriding both. Either
path may be absent. See the root `DEVELOPMENT.md` for the table format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
