# fesco-chair

Helper for [FESCo meeting chair
duties](https://fedoraproject.org/wiki/FESCo_meeting_process):
compose the agenda announcement email, generate the day-of meetbot
script (with the pre/during-meeting checklist), and compose the
post-meeting summary email. It prepares text for you to paste — it
does not send email or post to Matrix.

Agenda tickets come from the [FESCo
tracker](https://forge.fedoraproject.org/fesco/tickets) (Forgejo):
`meeting`-labeled tickets are the discussion agenda, and **open**
`pending announcement`-labeled tickets land in the "Discussed and
Voted in the Ticket" section (the flow is tag → announce → untag +
close, so a closed ticket still carrying the label is stale state and
is ignored). Whether a meeting ticket is a
**Followup** (discussed at a previous meeting) or **New business** is
inferred by scanning recent meeting minutes on
[meetbot](https://meetbot.fedoraproject.org/) for the ticket's
`TOPIC: #NNNN` line — and can always be overridden per ticket.

## Installation

```sh
cargo install fesco-chair
```

A Forgejo API token is required for the tracker queries (and for the
planned ticket-update support). Create one under Settings →
Applications on
[forge.fedoraproject.org](https://forge.fedoraproject.org/user/settings/applications),
then store it with `fesco-chair config`. Token scopes (Forgejo scopes
tokens by category, each read-only or read-and-write):

- **issue: read** — the agenda queries list tracker issues by label
- **repository: read** — the issue endpoints live under the `/repos`
  API group, so they need repository read access too
- **user: read** — only for `fesco-chair config`'s validation step
  (it calls `/api/v1/user` to check the token works)
- **issue: read-and-write** instead of read — optional today; choose
  it if you want the token ready for the planned ticket-update
  support (commenting on / closing tickets after the meeting)

The `FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG` and `FORGEJO_TOKEN`
environment variables override the stored token (matching
sandogasa-report's convention, so one env setup can serve both
tools).

## Subcommands

### `agenda`

```sh
fesco-chair agenda                     # announcement for the coming Tuesday
fesco-chair agenda --date 2026-07-14   # explicit meeting date
fesco-chair agenda --followup 3623     # force #3623 into Followups
fesco-chair agenda --new 3630 --voted 3610   # force other sections
fesco-chair agenda --docs 28           # add fesco/docs#28 to the agenda
fesco-chair agenda --history 20        # scan more past meetings
fesco-chair agenda --json              # machine-readable
```

Prints the announcement email (To/Subject plus the wiki-template
body) for `devel@lists.fedoraproject.org`. Sections are sorted by
ticket number. Each "Discussed and Voted in the Ticket" entry carries
its decision parsed from the ticket's comments — the vote concludes
with e.g. `After a week: APPROVED (+3, 0, 0)` right before the ticket
is tagged — falling back to a `DECISION (+X, Y, -Z)` placeholder
(with a warning) when no tally is found. After sending, comment
`Announced: <archive link>` on each announced ticket, untag
`pending announcement`, and close it — that's what keeps the next
agenda's query clean. The
`--voted`/`--followup`/`--new` overrides take ticket numbers
(repeated or CSV) and win over both the labels and the minutes-based
inference; a number carrying neither agenda label is fetched
individually so it can still be placed. If meetbot is unreachable the
tool warns and lists every meeting ticket under New business.

Open [fesco/docs](https://forge.fedoraproject.org/fesco/docs) issues
and pull requests (the wiki's pre-meeting step 3) are offered onto
the agenda: on a terminal each one is prompted for individually
(default no), `--docs <N,...>` adds them unprompted (issues and PRs
share one number space), and selected items land under New business
as `fesco/docs#NN` entries. In `--json` mode nothing is prompted —
unselected items are reported in a `docs_open` field instead. A
reminder to comment on each ticket ("This issue will be discussed at
the next meeting on …") is printed to stderr.

### `config`

```sh
fesco-chair config
```

Interactive setup: prompts for the Forgejo API token (hidden input),
validates it against forge.fedoraproject.org, and stores it at
`~/.config/fesco-chair/config.toml` (directory 700, file 600).

### `script`

```sh
fesco-chair script > meeting.txt       # same knobs as agenda
```

Prints the day-of checklist to stderr (the spam-filter-safe
`!group members fesco` reminder for #devel:fedoraproject.org, quorum,
the 15-minute topic rule) and the meetbot command script to stdout:
`!startmeeting FESCO (date)` through per-ticket
`!topic`/`!forge`/`!agreed` blocks (Followups first, then New
business) to `!endmeeting`. Lookups match the item: tracker tickets
use `!forge issue fesco tickets NNNN`, fesco/docs issues
`!forge issue fesco docs NNNN`, and fesco/docs pull requests
`!forge pr fesco docs NNNN`. Copy/paste lines as the meeting
progresses. Accepts the same flags as `agenda`.

> The ticket lookup is emitted as `!forge issue fesco tickets NNNN`
> for now: the `!fesco NNNN` alias is broken until
> [maubot-fedora#154](https://github.com/fedora-infra/maubot-fedora/pull/154)
> is merged and deployed.

### `summary`

```sh
fesco-chair summary                    # today's meeting
fesco-chair summary --date 2026-06-30  # an earlier meeting
fesco-chair summary --json             # machine-readable
```

Finds the meeting on meetbot (available right after `!endmeeting`),
and prints the summary email: subject
`Summary/Minutes from today's FESCo Meeting (date)`, the four
artefact links (minutes/log, HTML and text), and the full plain-text
minutes. Send it as a reply to the schedule announcement, then
comment/close the discussed tickets.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
