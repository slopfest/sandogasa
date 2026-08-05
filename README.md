<p align="center">
  <img src="icon.svg" width="128" height="128" alt="sandogasa logo">
</p>

# sandogasa

[![Packaging status](https://repology.org/badge/vertical-allrepos/sandogasa.svg)](https://repology.org/project/sandogasa/versions)

Cross-distribution packaging tools and libraries, focused primarily on
Fedora, CentOS and Debian, built around shared API clients for Bugzilla,
Bodhi, Koji, NVD, dist-git, GitLab, Forgejo, Discourse, FASJSON and
HyperKitty.

Most of it is packaging work — branching, CVE triage, build-queue
analysis, ACLs, release monitoring. The activity-tracking parts are
narrower in their assumptions: `sandogasa-report` and
`sandogasa-hattrack` collect a person's contributions across forges and
mailing lists, which is useful to anyone tracking their own work, with
or without a distribution attached.

The name **sandogasa** (菅笠) refers to a Japanese straw hat often
associated with "slum" or post-apocalyptic robots in popular culture.

## Tools

- **[cpu-sig-tracker](tools/cpu-sig-tracker/)** — CentOS Proposed Updates SIG package-state tracker across Koji, GitLab, and JIRA
- **[dbranch](tools/dbranch/)** — propagate a Debian package across its downstream branches in `rpmbuild`-style stages: rebuild Ubuntu PPA and Debian stable proposed-update (`debian/<codename>`) branches, and update the Debian branch to a new upstream (merge/import + changelog entry; optional pbuilder build, lintian, push + GitLab CI watch via `glab`, dput upload, and tag); doubles as a learning tool via `--explain`
- **[ebranch](tools/ebranch/)** — cross-branch porting helper: build-order resolution, branch requests, and update checking with Bodhi karma
- **[fedora-cve-triage](tools/fedora-cve-triage/)** — triage CVEs reported against Fedora components in Red Hat Bugzilla
- **[fedora-review-digest](tools/fedora-review-digest/)** — condense a `fedora-review` run of an auto-generated spec (rust2rpm) into a short rust-sig-style review comment
- **[fesco-chair](tools/fesco-chair/)** — FESCo meeting chair helper: agenda announcement email, day-of meetbot script, and post-meeting summary email
- **[hs-intake](tools/hs-intake/)** — Hyperscale package intake analysis
- **[hs-meetings](tools/hs-meetings/)** — list and sync CentOS Hyperscale SIG meeting archives from meetbot
- **[hs-relmon](tools/hs-relmon/)** — Hyperscale release monitoring via Repology
- **[koji-diff](tools/koji-diff/)** — compare buildroot and build logs between Koji builds
- **[koji-lag](tools/koji-lag/)** — quantify Koji build queue lag and per-arch build-time drag
- **[poi-tracker](tools/poi-tracker/)** — package-of-interest tracker for Fedora, EPEL, and CentOS SIGs
- **[sandogasa-hattrack](tools/sandogasa-hattrack/)** — look up a Fedora contributor's activity across services
- **[sandogasa-pkg-acl](tools/sandogasa-pkg-acl/)** — view and manage Fedora package ACLs via the Pagure dist-git API
- **[sandogasa-pkg-health](tools/sandogasa-pkg-health/)** — audit package health across a sandogasa inventory (pluggable checks, selective update)
- **[sandogasa-report](tools/sandogasa-report/)** — activity reporting for Fedora, EPEL, and CentOS SIG packaging

## Library crates

The underlying API clients and utilities are published as reusable
library crates:

- **sandogasa-bodhi** — Bodhi API client for Fedora update queries
- **sandogasa-bugclass** — bug classification (CVE, FTBFS, update request, etc.) across issue trackers
- **sandogasa-bugzilla** — Bugzilla REST API client
- **sandogasa-cli** — shared CLI utilities (external tool availability
  checks, yes/no prompts, word wrapping, and an optional `http` feature
  with the API clients' shared reqwest plumbing)
- **sandogasa-config** — shared config file management and interactive prompting
- **sandogasa-copr** — COPR API client for read-only project monitoring
- **sandogasa-depfilter** — RPM dependency filtering for cross-branch analysis
- **sandogasa-discourse** — Discourse forum API client
- **sandogasa-distgit** — Fedora dist-git client, ACL management, and RPM spec file parser
- **sandogasa-fasjson** — FASJSON (Fedora Account System) API client with Kerberos auth
- **sandogasa-fedrq** — wrapper for the fedrq RPM repository query tool
- **sandogasa-forgejo** — Forgejo / Gitea REST API client (PR activity + issue filing)
- **sandogasa-github** — GitHub REST API client (user identity + activity)
- **sandogasa-gitlab** — GitLab REST and GraphQL API client
- **sandogasa-inventory** — package-of-interest inventory data model and I/O
- **sandogasa-jira** — minimal JIRA REST API client (issue status lookup)
- **sandogasa-koji** — Koji build system CLI wrapper
- **sandogasa-kojihub** — Koji hub XML-RPC client
- **sandogasa-mailman** — HyperKitty (Mailman 3) archive API client
- **sandogasa-meetbot** — meetbot.fedoraproject.org meeting search client
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client
- **sandogasa-repology** — Repology package version tracking API client
- **sandogasa-review** — shared keep/explain/remove resolution for
  reviewer-curated findings
- **sandogasa-rpmvercmp** — RPM version comparison algorithm
- **sandogasa-sourcehut** — Sourcehut (sr.ht) GraphQL API client for user
  activity reporting

## Installation

On Fedora:

```
sudo dnf install sandogasa
```

From source:

```
cargo build --release
```

## Development

`make help` lists the available targets — checks, coverage, man page
generation, and the release gates:

```
make check          # fmt, clippy, tests, and the packaging-build test
make man            # regenerate the man pages after a CLI change
make release-checks # the above plus audit, semver-checks and coverage
```

The Makefile is a task runner over cargo and `scripts/`; cargo remains
the build system, and distro packaging drives it directly.

## Man pages

Each tool ships a man page at `tools/<tool>/man/<tool>.1`, covering the
tool and all of its subcommands in one page. The pages are generated
from the same clap definitions that produce `--help`, so the two cannot
disagree; each tool's test suite fails if its page stops documenting a
flag or falls behind the current version. Regenerate them with `make
man` after changing a command-line interface or bumping the version.

The pages are committed and included in the published crates, so
packagers can install them without building or running the binaries:

```
install -Dm644 tools/koji-lag/man/koji-lag.1 \
  "$RPM_BUILD_ROOT/usr/share/man/man1/koji-lag.1"
```

## Deprecations

Deprecated functionality, its replacement, and the release it
will be removed in are tracked in
[DEPRECATIONS.md](DEPRECATIONS.md).

## Contributing

Issues and pull requests are welcome. See
[CONTRIBUTING.md](CONTRIBUTING.md) for what a contribution needs —
tests that pass offline, a specific issue to fix, a signed-off commit —
and for how to disclose AI assistance with an `Assisted-by:` trailer.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
