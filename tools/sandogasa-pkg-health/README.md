# sandogasa-pkg-health

Audit package health across a sandogasa inventory.

Each package is scored against a set of pluggable health checks —
open bugs, maintainer coverage, build status, etc. Checks are
classified by cost tier (cheap / medium / expensive) so you can
run them on different schedules.

Reports persist to TOML and update incrementally: re-running a
single check preserves the stored results of all other checks.

## Installation

```sh
cargo install sandogasa-pkg-health
```

## Usage

List available checks:

```sh
sandogasa-pkg-health checks
```

Run all cheap checks against an inventory, writing/updating a
report file:

```sh
sandogasa-pkg-health run \
    -i inventory.toml \
    -o health.toml \
    --cheap
```

Run specific checks:

```sh
sandogasa-pkg-health run \
    -i inventory.toml -o health.toml \
    --check maintainer_count
```

Limit to specific packages:

```sh
sandogasa-pkg-health run \
    -i inventory.toml -o health.toml \
    --all --package rust-arrow --package rust-tokio
```

## Project status

Early development — see [PLAN.md](PLAN.md) for architecture and
[TODO.md](TODO.md) for the current MVP checklist.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
