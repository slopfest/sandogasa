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
hs-meetings list                       # default topic: centos-hyperscale-sig
hs-meetings list --topic some-other    # any meetbot topic
hs-meetings list --json                # machine-readable
```

Fetches every meeting whose topic contains the search string (default
`centos-hyperscale-sig`), sorted ascending by date. Prints a table
of `DATE | TOPIC | SUMMARY` or a JSON array with `--json`.

### `sync`

Planned — merges missing meetings into the Hyperscale SIG docs'
`meetings.md`. Not yet implemented.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
