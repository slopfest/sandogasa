# sandogasa-inventory

Package-of-interest inventory data model and I/O.

Provides a TOML-based inventory format for tracking packages across
Fedora, EPEL, and CentOS SIGs. Supports exporting to content-resolver
YAML (feedback-pipeline-workload) and hs-relmon manifest formats.

## JSON Schema

A JSON Schema for the inventory format is checked in at
[`data/inventory.schema.json`](data/inventory.schema.json). It is
generated from the Rust types via `schemars` and verified by a test.

When the data model changes, update the schema:

```sh
UPDATE_SCHEMA=1 cargo test -p sandogasa-inventory schema_up_to_date
```

Review the diff before committing — new required fields are a
breaking change (major/minor bump), new optional fields are a minor
change.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
