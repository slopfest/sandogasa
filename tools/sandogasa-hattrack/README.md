# sandogasa-hattrack

Look up a Fedora contributor's activity across services, and whether
they are away, and file your own absence in Fedora Calendar.

## Installation

```
cargo install sandogasa-hattrack
```

## Usage

### Quick summary

```
$ sandogasa-hattrack last-seen salimma --no-fas
Last seen: salimma

  Dist-git       2026-03-20T23:59:59+00:00 (3 days ago)
                 last active on 2026-03-20
  Bodhi          2026-03-20T23:44:44+00:00 (3 days ago)
                 last update submitted
  Bugzilla       2026-03-20T15:14:06+00:00 (3 days ago)
                 #2449640 Tracker for invalid, cross-ecosystem CVE
  Discourse      2026-03-18T10:51:27+00:00 (5 days ago)
                 last post
                 status:  🏖️ on vacation
                 expires: 2026-04-01 00:00 UTC (in 1 week)
  Mailing lists  2026-03-13T09:58:20+00:00 (1 week ago)
                 Retiring python-sphinx-hoverxref
  Calendar       away 2026-03-23 to 2026-04-01 (back Thu 2026-04-02)
```

`Calendar` is Fedora Calendar's vacation calendar: the current or next
absence filed for the coming 90 days, and any later ones. The row is
left out when none is filed. Days are counted as the last full day
away, with the day back; Fedora Calendar's own pages show the day back
as the end (`End: Mon, January 04, 2027 - 00:00 UTC`).

### Subcommands

- `away` — file your own absence in Fedora Calendar's vacation calendar
- `bodhi` — recent Bodhi updates and comments
- `bugzilla` — recent Bugzilla activity
- `distgit` — dist-git activity, PRs filed, and PRs awaiting review
- `discourse` — Discourse profile and activity
- `forge` — Forgejo activity per repository, e.g. in the FESCo tracker
- `group` — last activity of every member of a FAS group, most recent
  first, with a cap on group size
- `last-seen` — summary of last activity across all services, including
  Discourse custom status and expiration
- `mailman` — mailing list posts via HyperKitty
- `meetings` — attendance at a recurring meetbot meeting, e.g. FESCo

### Activity in a meeting or an issue tracker

Two questions a committee asks about its members: did they come to the
meetings, and did they take part in the tickets. `meetings` answers the
first from meetbot: every meeting with the given `!meetingname` topic in
the window, and whether the user's Matrix ID appears in its minutes'
"People Present" list, with how many lines they said:

```
$ sandogasa-hattrack meetings salimma --meeting fesco --days 120
Meetings: salimma in 'fesco' (last 120 days)

  Matrix IDs: @salimma:fedora.im
  Attended 13 of 14 meeting(s); last attended 2026-09-01

  2026-09-01  present  (127 line(s) said)
  2026-08-25  present  (21 line(s) said)
  2026-07-14  absent
```

`@<username>:fedora.im` is assumed to be the user, and the Matrix IDs on
their FAS profile are added through FASJSON (Kerberos; `--no-fas`
skips it, a failed lookup only warns). `--matrix @nirik:matrix.scrye.com`
adds one FAS does not know. `--meeting` defaults to `fesco` and `--days`
to 180; `--since 2026-07-01` starts the window on a date instead — the
day a member joined, say. A member newer than the window is also read
off the data: when meetings in the window predate their first
attendance, the summary adds `since first seen 2026-07-07, 2 of 3`, so
the meetings before they joined do not read as absences.

`forge` answers the second from the user's public Forgejo activity
feed, grouped by repository. Without `--repo` each repository gets one
summary line; with `--repo fesco/tickets` every event in that tracker
is listed, newest first:

```
$ sandogasa-hattrack forge salimma --repo fesco/tickets --days 60
Forge: salimma (last 60 days)

  fesco/tickets                    9 closed, 38 commented, 1 opened, 1 reopened — last 2026-08-17
    2026-08-17  commented    #3671  yeah, and unless there is a serious issue with a package, …
    2026-08-12  commented    #3677  +1 (as Change owner)
```

`--days` defaults to 90. Both subcommands take `--json`.

`last-seen` now includes Forgejo — the newest event anywhere, or with
`--repo fesco/tickets` the newest in those repositories — and, with
`--meeting fesco`, the last attended meeting of that topic with a
one-year attendance count (and the since-first-seen count when that
differs); `--matrix` applies there too.

### A whole group at a glance

"Has the Btrfs SIG been responding?" and "who in the SIG is around right
now?" are the same question asked of every member, so `group` runs
`last-seen` for each member of a FAS group and lists them most recent
first, each with their local time and every service that was asked
for — a SIG that answers Bugzilla but never Bodhi shows as exactly that:

```
$ sandogasa-hattrack group rust-sig
Group: rust-sig (10 members)

  music  local: 2026-09-08 11:29:04 BST (Tue — weekday)
      Dist-git       2026-09-08T23:59:59+00:00 (today)
      Bodhi          2026-09-08T06:42:47+00:00 (3 hours ago)
      Forge          2026-09-07T21:28:06+00:00 (13 hours ago)
      Bugzilla       2026-09-05T10:19:58+00:00 (3 days ago)
      Discourse      2026-07-14T20:24:13.063+00:00 (1 month ago)

  decathorpe  local: 2026-09-08 12:29:04 CEST (Tue — weekday)
      Dist-git       2026-09-07T23:59:59+00:00 (10 hours ago)
      Bodhi          2026-09-07T21:10:58+00:00 (13 hours ago)
      Forge          2026-09-07T21:02:34+00:00 (13 hours ago)
      Bugzilla       2026-09-05T13:45:55+00:00 (2 days ago)
      Discourse      2026-09-02T21:21:37.786+00:00 (5 days ago)
  …
```

Each member's services are queried at once, and members four at a
time, so ten members take about fifteen seconds, Mailman skipped.

Membership is FAS's, read live, so the group's Matrix IDs, emails and
time zones come along without flags. Each member costs a round of
service queries, so a group larger than `--max-members` (25) is refused
before anything is fetched — `packager` has thousands — and Mailman is
skipped unless asked for with `--only`, its archive walk per member
being what would dominate the run. `--skip`, `--only`, `--meeting` and
`--repo` mean what they mean for `last-seen`; `--json` returns every
member's `last-seen` summary under `members`.

With `--meeting`, the meetbot listing for the topic and each meeting's
log are kept under `~/.cache/sandogasa-hattrack/` (`$XDG_CACHE_HOME`):
the listing for a day, the logs indefinitely, since a past meeting's
log does not change. A group run therefore fetches each log once
rather than once per member, and `meetings` and `last-seen --meeting`
read the same cache. `--refresh` ignores it and fetches afresh.

### Email discovery

Subcommands that need an email address (bugzilla, mailman) will:

1. Always try `username@fedoraproject.org`
2. Query FASJSON for additional emails (requires Kerberos)
3. Use `--email` for direct override, `--no-fas` to skip FASJSON

### Local time and weekend signal

The `discourse` subcommand resolves the user's IANA timezone to
a country (via the tzdb `zone1970.tab` table), then reports the
local time and whether it's currently the weekend in that
country. Weekends default to Sat+Sun, with overrides for places
where the workweek is shifted (Fri+Sat across most of MENA,
Fri only in Iran, Sat only in Nepal).

The lookup table is read from `/usr/share/zoneinfo/zone1970.tab`
by default; if that's older than the copy bundled with this
tool, an `info:` line on stderr notes that the bundled copy is
being used instead. Force one or the other with `--tz-source
system` / `--tz-source bundled`.

The weekday tag is colored (green for weekday, yellow for
weekend) and the time itself is dimmed when the local hour
falls outside working hours. Defaults to `9-18`; override with
`--working-hours <START-END>`. Colors follow the grep/ls
convention: `--color auto` (default) enables them on a TTY
when `NO_COLOR` is unset; `--color always` / `--color never`
force a choice.

The same `Local time:` / `Country:` block also appears at the
top of `last-seen` output. Both FAS (via FASJSON) and
Discourse are queried independently: matching timezones
collapse to one entry, mismatched ones are shown side-by-side
with a `[FAS]` / `[Discourse]` suffix so you can spot a
traveler who's updated one source but not the other.

### Public-holiday flag

When the user's resolved country has a nationwide public
holiday on their local date, a `Holiday:` line appears under
`Country:`. Data comes from the [Nager.Date](https://date.nager.at)
public API (122 countries) and is cached per country-per-year
under `$XDG_CACHE_HOME/sandogasa-hattrack/holidays/`, so
repeat lookups never touch the network.

- `--no-holidays` skips the lookup entirely.
- `--refresh-holidays` force-refetches the cached data.
- `--now <YYYY-MM-DD>` overrides the date for testing, e.g.
  `--now 2026-03-17 discourse salimma` to see what
  St. Patrick's Day looks like.

### Filing your own absence

```
$ sandogasa-hattrack away 2026-12-21 2027-01-03 --note "Back on the 4th"
New entry in https://calendar.fedoraproject.org/vacation/:
  title:     salimma
  days:      2026-12-21 to 2027-01-03 (full days, 14 day(s))
  note:      Back on the 4th

Logging in to Fedora Calendar with your Kerberos ticket...
File this absence? [Y/n]:
Filed: https://calendar.fedoraproject.org/meeting/11460/
```

`away` files a full-day entry in the vacation calendar for the FAS
account in `~/.fedora.upn`, titled with its username unless `--title`
says otherwise. Fedora Calendar has no API for adding entries, so
`away` logs in to it as a browser would, with your Kerberos ticket
instead of a password: get one with `fkinit -u <FAS username>`, or let
`away` renew an expired one or run `fkinit` (or `kinit` where fkinit
is not installed) for you, once it knows the
days need filing. The login takes about ten seconds. An absence already filed for the same days is not
filed again; one that overlaps only in part is listed, and filing
another needs a "yes".

`--update` changes the filed absence instead, to the `--title` and
`--note` given, showing each change before and after; everything else
in the entry is kept. The days given find the absence: they must cover
at least half of it, or it at least half of them. When they differ
from the filed days, `away` asks whether to change the days to them,
defaulting to keeping the filed ones; with `--yes` the days given are
taken, and said.

```
$ sandogasa-hattrack away 2026-12-01 2026-12-05 --update --note "Back on the 4th"
Change its days from 2026-11-24 to 2027-01-03 (back Mon 2027-01-04) to 2026-12-01 to 2026-12-05 (back Sun 2026-12-06)? [y/N]:
Changes to https://calendar.fedoraproject.org/meeting/11459/:
  note:      (none) -> "Back on the 4th"
```

After filing, the entry is read back from the calendar. `--dry-run` logs
in and checks that you may add to the vacation calendar, but files
nothing; `--yes` files without asking, which is also how to run it
without a terminal, given a valid ticket. With `--json` (and `--yes` or
`--dry-run`) it prints the outcome (`already_filed`, `declined`,
`dry_run`, `filed`, `unchanged` or `updated`, with the entry's page),
its messages going to standard error. It needs `curl` and `klist`
(krb5-workstation).

### Narrowing the `last-seen` service set

`last-seen` queries seven services (Bodhi, Bugzilla, Calendar,
Discourse, dist-git, Forgejo, Mailman), and meetings when asked. The Mailman scan is the slow path because
it walks HyperKitty archives page by page, so skipping it is
the common speed-up when the user clearly doesn't post:

```sh
sandogasa-hattrack last-seen alice --skip mailman
sandogasa-hattrack last-seen alice --skip mailman,bugzilla
sandogasa-hattrack last-seen alice --only discourse,bodhi
```

`--skip` and `--only` are mutually exclusive. Both accept a
comma-separated list (or can be repeated). Values:
`bodhi`, `bugzilla`, `calendar`, `discourse`, `distgit`, `forge`,
`mailman`, `meetings` (the last only does anything with `--meeting`).

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-hattrack --json last-seen salimma --no-fas
```

## System-wide configuration

This tool keeps no settings of its own, but it does read a `[defaults]`
table — for pinning the flags you always pass — from
`/etc/sandogasa-hattrack/config.toml` and
`~/.config/sandogasa-hattrack/config.toml`, the user file overriding the
system one per key and command-line flags overriding both. Either path
may be absent. See the root `DEVELOPMENT.md` for the table format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
