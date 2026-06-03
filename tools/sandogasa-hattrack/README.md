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
