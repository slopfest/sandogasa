# hs-meetings

List and (eventually) sync CentOS Hyperscale SIG meeting archives
from [meetbot.fedoraproject.org](https://meetbot.fedoraproject.org).

The SIG holds biweekly meetings on Matrix; zodbot logs them as
`centos-hyperscale-sig` topic sessions. This tool wraps meetbot's
search endpoint so the archive list in the SIG docs can be
maintained from the command line rather than by hand.

## Installation

```sh
cargo install hs-meetings
```

## Subcommands

### `list`

```sh
hs-meetings list                               # every meeting on the default topic
hs-meetings list --topic some-other            # any meetbot topic
hs-meetings list --period 2026Q1               # calendar period filter
hs-meetings list --period 2025H2               # … halves also work
hs-meetings list --since 2026-03-01            # open-ended from a date
hs-meetings list --since 2026-03-01 --until 2026-04-30  # explicit range
hs-meetings list --json                        # machine-readable
```

Fetches every meeting whose topic contains the search string (default
`centos-hyperscale-sig`), sorted ascending by date. `--period`
accepts `YYYY`, `YYYYQ1..Q4`, `YYYYH1..H2`; `--since` /
`--until` take `YYYY-MM-DD`. Output is a `DATE | TOPIC | SUMMARY`
table or a JSON array with `--json`.

### `sync`

Planned — merges missing meetings into the Hyperscale SIG docs'
`meetings.md`. Not yet implemented.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
