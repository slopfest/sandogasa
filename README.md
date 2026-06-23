<p align="center">
  <img src="icon.svg" width="128" height="128" alt="sandogasa logo">
</p>

# sandogasa

[![Packaging status](https://repology.org/badge/vertical-allrepos/sandogasa.svg)](https://repology.org/project/sandogasa/versions)

A collection of tools and libraries for Fedora package maintenance
and contributor activity tracking, built around shared API clients
for Bugzilla, Bodhi, NVD, dist-git, Discourse, FASJSON, and HyperKitty.

The name **sandogasa** (菅笠) refers to a Japanese straw hat often
associated with "slum" or post-apocalyptic robots in popular culture.

## Tools

- **[cpu-sig-tracker](tools/cpu-sig-tracker/)** — CentOS Proposed Updates SIG package-state tracker across Koji, GitLab, and JIRA
- **[dbranch](tools/dbranch/)** — propagate a Debian package across its downstream branches in `rpmbuild`-style stages: rebuild Ubuntu PPA and Debian stable proposed-update (`debian/<codename>`) branches, and update the Debian branch to a new upstream (merge/import + changelog entry; optional pbuilder build, lintian, push + GitLab CI watch via `glab`, dput upload, and tag); doubles as a learning tool via `--explain`
- **[ebranch](tools/ebranch/)** — cross-branch porting helper: build-order resolution, branch requests, and update checking with Bodhi karma
- **[fedora-cve-triage](tools/fedora-cve-triage/)** — triage CVEs reported against Fedora components in Red Hat Bugzilla
- **[fedora-review-digest](tools/fedora-review-digest/)** — condense a `fedora-review` run of an auto-generated spec (rust2rpm) into a short rust-sig-style review comment
- **[hs-intake](tools/hs-intake/)** — Hyperscale package intake analysis
- **[hs-meetings](tools/hs-meetings/)** — list and sync CentOS Hyperscale SIG meeting archives from meetbot
- **[hs-relmon](tools/hs-relmon/)** — Hyperscale release monitoring via Repology
- **[koji-diff](tools/koji-diff/)** — compare buildroot and build logs between Koji builds
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
- **sandogasa-cli** — shared CLI utilities (external tool availability checks)
- **sandogasa-config** — shared config file management and interactive prompting
- **sandogasa-depfilter** — RPM dependency filtering for cross-branch analysis
- **sandogasa-discourse** — Discourse forum API client
- **sandogasa-distgit** — Fedora dist-git client, ACL management, and RPM spec file parser
- **sandogasa-fasjson** — FASJSON (Fedora Account System) API client with Kerberos auth
- **sandogasa-fedrq** — wrapper for the fedrq RPM repository query tool
- **sandogasa-github** — GitHub REST API client (user identity + activity)
- **sandogasa-gitlab** — GitLab REST and GraphQL API client
- **sandogasa-inventory** — package-of-interest inventory data model and I/O
- **sandogasa-jira** — minimal JIRA REST API client (issue status lookup)
- **sandogasa-koji** — Koji build system CLI wrapper
- **sandogasa-mailman** — HyperKitty (Mailman 3) archive API client
- **sandogasa-meetbot** — meetbot.fedoraproject.org meeting search client
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client
- **sandogasa-repology** — Repology package version tracking API client
- **sandogasa-rpmvercmp** — RPM version comparison algorithm

## Installation

On Fedora:

```
sudo dnf install sandogasa
```

From source:

```
cargo build --release
```

## Deprecations

Deprecated functionality, its replacement, and the release it
will be removed in are tracked in
[DEPRECATIONS.md](DEPRECATIONS.md).

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
