<p align="center">
  <img src="icon.svg" width="128" height="128" alt="sandogasa logo">
</p>

# sandogasa

A collection of tools and libraries for Fedora package maintenance
and contributor activity tracking, built around shared API clients
for Bugzilla, Bodhi, NVD, dist-git, Discourse, FASJSON, and HyperKitty.

The name **sandogasa** (菅笠) refers to a Japanese straw hat often
associated with "slum" or post-apocalyptic robots in popular culture.

## Tools

- **[ebranch](tools/ebranch/)** — build dependency resolver for cross-branch package porting
- **[fedora-cve-triage](tools/fedora-cve-triage/)** — triage CVEs reported against Fedora components in Red Hat Bugzilla
- **[hs-intake](tools/hs-intake/)** — Haskell package intake analysis for Fedora
- **[hs-relmon](tools/hs-relmon/)** — Haskell release monitoring via Repology
- **[koji-diff](tools/koji-diff/)** — compare buildroot and build logs between Koji builds
- **[poi-tracker](tools/poi-tracker/)** — package-of-interest tracker for Fedora, EPEL, and CentOS SIGs
- **[sandogasa-hattrack](tools/sandogasa-hattrack/)** — look up a Fedora contributor's activity across services
- **[sandogasa-pkg-acl](tools/sandogasa-pkg-acl/)** — view and manage Fedora package ACLs via the Pagure dist-git API
- **[sandogasa-report](tools/sandogasa-report/)** — activity reporting for Fedora, EPEL, and CentOS SIG packaging

## Library crates

The underlying API clients and utilities are published as reusable
library crates:

- **sandogasa-bodhi** — Bodhi API client for Fedora update queries
- **sandogasa-bugzilla** — Bugzilla REST API client
- **sandogasa-cli** — shared CLI utilities (external tool availability checks)
- **sandogasa-config** — shared config file management and interactive prompting
- **sandogasa-depfilter** — RPM dependency filtering for cross-branch analysis
- **sandogasa-discourse** — Discourse forum API client
- **sandogasa-distgit** — Fedora dist-git client, ACL management, and RPM spec file parser
- **sandogasa-fasjson** — FASJSON (Fedora Account System) API client with Kerberos auth
- **sandogasa-fedrq** — wrapper for the fedrq RPM repository query tool
- **sandogasa-gitlab** — GitLab REST and GraphQL API client
- **sandogasa-inventory** — package-of-interest inventory data model and I/O
- **sandogasa-koji** — Koji build system CLI wrapper
- **sandogasa-mailman** — HyperKitty (Mailman 3) archive API client
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client
- **sandogasa-repology** — Repology package version tracking API client
- **sandogasa-rpmvercmp** — RPM version comparison algorithm

## Building

```
cargo build --release
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
