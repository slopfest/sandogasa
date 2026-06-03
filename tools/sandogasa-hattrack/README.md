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
- `last-seen` — summary of last activity across all services, including
  Discourse custom status and expiration
- `mailman` — mailing list posts via HyperKitty

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

`last-seen` queries five services (Bodhi, Bugzilla, Discourse,
dist-git, Mailman). The Mailman scan is the slow path because
it walks HyperKitty archives page by page, so skipping it is
the common speed-up when the user clearly doesn't post:

```sh
sandogasa-hattrack last-seen alice --skip mailman
sandogasa-hattrack last-seen alice --skip mailman,bugzilla
sandogasa-hattrack last-seen alice --only discourse,bodhi
```

`--skip` and `--only` are mutually exclusive. Both accept a
comma-separated list (or can be repeated). Values:
`bodhi`, `bugzilla`, `discourse`, `distgit`, `mailman`.

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-hattrack --json last-seen salimma --no-fas
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
