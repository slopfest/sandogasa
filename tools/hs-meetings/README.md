# hs-meetings

List and sync CentOS Hyperscale SIG meeting archives from
[meetbot.fedoraproject.org](https://meetbot.fedoraproject.org).

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
accepts `YYYY`, `YYYYQ1..Q4`, `YYYYH1..H2`; `--since` / `--until`
take `YYYY-MM-DD`. Output is a two-line-per-meeting table (date +
stacked summary/logs URLs) or a JSON array with `--json`.

### `sync`

```sh
hs-meetings sync --file meetings-list.md              # fetch + merge
hs-meetings sync --file meetings-list.md --dry-run    # preview only
hs-meetings sync --file meetings-list.md --period 2026  # limit to a year
```

Fetches meetings from meetbot, deduplicates against entries
already present in `--file` (matching by date extracted from
the URL), and inserts any new ones into the correct `## YYYY`
section in reverse-chronological order. New year sections are
created as needed, newest-first.

Meetings from 2023 and earlier are dropped before insertion:
pre-2024 sections predate meetbot and often carry hand-curated
`[agenda](...)` links, so the tool leaves them untouched. The
recommended docs layout is therefore to keep only the 2024+
sections in the tool-managed file and leave the legacy years
inline in `meetings.md`, which the docs site pulls together via
`pymdownx.snippets`:

```yaml
# mkdocs.yml
markdown_extensions:
  - pymdownx.snippets
```

```markdown
<!-- meetings.md -->
## Meeting minutes

--8<-- "communication/meetings-list.md"

## 2023

* Jan 18: [agenda](https://hackmd.io/...),
          [summary](...),
          [logs](...)

## 2022
...
```

`hs-meetings sync` then owns `meetings-list.md` and can rewrite
it freely. New entries are rendered without an `agenda,` prefix
since no SIG meeting has had an external agenda link since
January 2023.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
