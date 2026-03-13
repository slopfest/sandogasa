# sandogasa

A collection of tools and libraries for Fedora package maintenance,
built around shared API clients for Bugzilla, Bodhi, NVD, and dist-git.

The name **sandogasa** (菅笠) refers to a Japanese straw hat often
associated with "slum" or post-apocalyptic robots in popular culture.

## Tools

- **[fedora-cve-triage](tools/fedora-cve-triage/)** — triage CVEs reported against Fedora components in Red Hat Bugzilla

## Library crates

The underlying API clients are published as reusable library crates:

- **sandogasa-bodhi** — Bodhi API client for Fedora update queries
- **sandogasa-bugzilla** — Bugzilla REST API client
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client
- **sandogasa-distgit** — Fedora dist-git client and RPM spec file parser

## Building

```
cargo build --release
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
