# hs-meetings

Helper for running [CentOS Hyperscale SIG
meetings](https://sigs.centos.org/hyperscale/communication/meetings/):
the day-of zodbot script for whoever chairs, and the meeting archive
list in the SIG docs, both built from
[meetbot.fedoraproject.org](https://meetbot.fedoraproject.org) (and,
for the script, the SIG's open GitLab issues). It prepares text for
you to paste — it does not post to Matrix.

The SIG holds biweekly meetings on Matrix; zodbot logs them as
`centos-hyperscale-sig` topic sessions. `list` and `sync` wrap
meetbot's search endpoint so the archive list can be maintained from
the command line rather than by hand; `script` is the counterpart of
`fesco-chair script` for the SIG's own meeting checklist.

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

### `script`

```sh
hs-meetings script > meeting.txt       # the coming Wednesday's meeting
hs-meetings script --date 2026-09-23   # an explicit meeting date
hs-meetings script --json              # the script plus its sources
```

Prints the meeting checklist context to stderr and the zodbot
command script to stdout, `!startmeeting CentOS Hyperscale SIG`
through `!endmeeting`, with the checklist's topics pre-filled:

- **Followups** links the previous meeting's minutes (the newest
  `centos-hyperscale-sig` meeting before the date) and carries each
  of its action items as an `!info followup from <date>: …` line.
- **Tickets** links the group's open-issue view on GitLab, then each
  open issue in the [CentOS/Hyperscale](https://gitlab.com/CentOS/Hyperscale)
  group, `meeting`-labelled ones first and then newest first.
  hs-relmon's automated `rfe::new-version` issues and the frozen
  Pagure imports under the `archive` subgroup are left out.
- **Membership** appears only when a `membership`-labelled issue is
  open, and links those.

The stderr side names the date, counts the followups, and lists every
linked ticket with its reference, title and labels, marking the ones
opened since the previous meeting `new`, then reminds you to run
`sync` on the docs checkout afterwards. Copy/paste the stdout lines
into #meeting:fedoraproject.org as the meeting progresses. Either
source being unreachable is a warning: the script still prints with
that topic empty. The GitLab reads are anonymous — no token is
needed.

### `sync`

```sh
hs-meetings sync --file meetings-list.md              # fetch + merge
hs-meetings sync --file meetings-list.md --dry-run    # preview only
hs-meetings sync --file meetings-list.md --period 2026  # limit to a year
```

Fetches meetings from meetbot, deduplicates against entries
already present in `--file` (matching by date extracted from
the URL), and inserts any new ones into the correct `### YYYY`
section in reverse-chronological order. New year sections are
created as needed, newest-first. Year headings are rendered at
`###` level so they nest under the docs site's `## Meeting
minutes` parent heading.

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

## System-wide configuration

This tool keeps no settings of its own, but it does read a `[defaults]`
table — for pinning the flags you always pass — from
`/etc/hs-meetings/config.toml` and `~/.config/hs-meetings/config.toml`,
the user file overriding the system one per key and command-line flags
overriding both. Either path may be absent. See the root
`DEVELOPMENT.md` for the table format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
