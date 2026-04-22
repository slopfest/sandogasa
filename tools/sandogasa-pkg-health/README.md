# sandogasa-pkg-health

Audit package health across a [sandogasa](../..) inventory.

Each package is scored against a set of pluggable health checks —
open bugs, maintainer coverage, build status, etc. Checks are
classified by cost tier (cheap / medium / expensive) so you can
run them on different schedules.

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
```

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

MVP complete — framework, two checks (`maintainer_count`,
`bug_count`), report persistence with selective update,
per-package parallelism, human-readable summary, JSON output,
and `show` subcommand. See [PLAN.md](PLAN.md) for architecture
and [TODO.md](TODO.md) for post-MVP roadmap.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
