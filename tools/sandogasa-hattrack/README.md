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

- `last-seen` — summary of last activity across all services, including
  Discourse custom status and expiration
- `discourse` — Discourse profile and activity
- `bodhi` — recent Bodhi updates and comments
- `bugzilla` — recent Bugzilla activity
- `distgit` — dist-git activity, PRs filed, and PRs awaiting review
- `mailman` — mailing list posts via HyperKitty

### Email discovery

Subcommands that need an email address (bugzilla, mailman) will:

1. Always try `username@fedoraproject.org`
2. Query FASJSON for additional emails (requires Kerberos)
3. Use `--email` for direct override, `--no-fas` to skip FASJSON

### JSON output

All subcommands support `--json` for machine-readable output:

```
$ sandogasa-hattrack --json last-seen salimma --no-fas
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
