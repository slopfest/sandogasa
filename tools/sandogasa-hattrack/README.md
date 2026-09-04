# sandogasa-hattrack

Look up a Fedora contributor's activity across services.

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
```

### Subcommands

- `bodhi` — recent Bodhi updates and comments
- `bugzilla` — recent Bugzilla activity
- `distgit` — dist-git activity, PRs filed, and PRs awaiting review
- `discourse` — Discourse profile and activity
- `forge` — Forgejo activity per repository, e.g. in the FESCo tracker
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

The weekday tag is coloured (green for weekday, yellow for
weekend) and the time itself is dimmed when the local hour
falls outside working hours. Defaults to `9-18`; override with
`--working-hours <START-END>`. Colours follow the grep/ls
convention: `--color auto` (default) enables them on a TTY
when `NO_COLOR` is unset; `--color always` / `--color never`
force a choice.

The same `Local time:` / `Country:` block also appears at the
top of `last-seen` output. Both FAS (via FASJSON) and
Discourse are queried independently: matching timezones
collapse to one entry, mismatched ones are shown side-by-side
with a `[FAS]` / `[Discourse]` suffix so you can spot a
traveller who's updated one source but not the other.

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

### Narrowing the `last-seen` service set

`last-seen` queries six services (Bodhi, Bugzilla, Discourse,
dist-git, Forgejo, Mailman), and meetings when asked. The Mailman scan is the slow path because
it walks HyperKitty archives page by page, so skipping it is
the common speed-up when the user clearly doesn't post:

```sh
sandogasa-hattrack last-seen alice --skip mailman
sandogasa-hattrack last-seen alice --skip mailman,bugzilla
sandogasa-hattrack last-seen alice --only discourse,bodhi
```

`--skip` and `--only` are mutually exclusive. Both accept a
comma-separated list (or can be repeated). Values:
`bodhi`, `bugzilla`, `discourse`, `distgit`, `forge`, `mailman`,
`meetings` (the last only does anything with `--meeting`).

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
