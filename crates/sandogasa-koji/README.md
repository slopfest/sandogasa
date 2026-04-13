# sandogasa-koji

Koji build system CLI wrapper for the sandogasa workspace.

Provides functions for querying Koji tags and builds by shelling out
to the `koji` CLI. Supports multiple Koji profiles (e.g. `cbs` for
CentOS Build System).

## Functions

- `list_tagged(tag, profile, timestamp)` — list builds with NVR, tag,
  and owner (optional timestamp for historical queries)
- `list_tagged_nvrs(tag, profile)` — list NVRs only (quiet mode)
- `build_rpms(nvr, profile)` — list binary RPM names from buildinfo
- `parse_nvr(nvr)` — split NVR into (name, version, release)
- `parse_nvr_name(nvr)` — extract just the package name from an NVR

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
