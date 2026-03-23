# sandogasa-hattrack

Look up a Fedora contributor's activity across services.

## Installation

```
cargo binstall sandogasa-hattrack
```

Or build from source:

```
cargo install sandogasa-hattrack
```

## Usage

### Discourse profile

```
$ sandogasa-hattrack discourse mattdm
Discourse: mattdm
  Name:        Matthew Miller
  Title:       Fedora Project Leader
  Timezone:    America/New_York
  Location:    Somerville, MA
  Last post:   2026-03-17 14:50:30 UTC
  Last seen:   2026-03-22 05:36:12 UTC
```

Use `--url` to query a different Discourse instance (defaults to
`https://discussion.fedoraproject.org`).

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-hattrack --json discourse mattdm
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
