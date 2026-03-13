# sandogasa

A collection of tools and libraries for Fedora package maintenance,
built around shared API clients for Bugzilla, Bodhi, NVD, and dist-git.

The name **sandogasa** (菅笠) refers to a Japanese straw hat often
associated with "slum" or post-apocalyptic robots in popular culture.

## Tools

- **[fedora-cve-triage](tools/fedora-cve-triage/)** — triage CVEs reported against Fedora components in Red Hat Bugzilla
- **[sandogasa-pkg-acl](tools/sandogasa-pkg-acl/)** — view and manage Fedora package ACLs via the Pagure dist-git API

## Library crates

The underlying API clients are published as reusable library crates:

- **sandogasa-bodhi** — Bodhi API client for Fedora update queries
- **sandogasa-bugzilla** — Bugzilla REST API client
- **sandogasa-config** — shared config file management and interactive prompting
- **sandogasa-distgit** — Fedora dist-git client, ACL management, and RPM spec file parser
- **sandogasa-nvd** — NVD (National Vulnerability Database) API client

## Building

```
cargo build --release
```

## License

This project is licensed under the [Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/).
