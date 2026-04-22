# cpu-sig-tracker

Track [CentOS Proposed Updates (CPU) SIG][cpu-sig] package state across
Koji, GitLab, and JIRA.

[cpu-sig]: https://sigs.centos.org/proposed-updates/

The CPU SIG temporarily ships fixed packages (typically CVE backports)
while CentOS Stream catches up to RHEL's security fixes. This tool
automates the polling and nudging around that workflow — detecting
when upstream has caught up (so a proposed-updates build can be
retired), flagging rebase-against-newer-Stream needs, and keeping
tracking issues up to date in the
[proposed_updates GitLab group][cpu-gitlab].

[cpu-gitlab]: https://gitlab.com/groups/CentOS/proposed_updates/-/work_items

## Installation

```sh
cargo install cpu-sig-tracker
```

Requires the [`koji`](https://pagure.io/koji) CLI with a `cbs` profile
configured (CentOS Build System).

## Project status

Early development. See [PLAN.md](PLAN.md) for architecture and
[TODO.md](TODO.md) for the MVP checklist.

## Usage

### Dump an inventory of currently-tagged packages

```sh
cpu-sig-tracker dump-inventory --release c10s -o cpu-sig.toml
```

Enumerates packages tagged into `c10s-proposed_updates` in Koji and
writes a [`sandogasa-inventory`](../../crates/sandogasa-inventory/)
TOML file with all of them. Safe to re-run: existing entries are
preserved, newly-discovered packages are added, and the per-release
workload is updated.

Run once per release you care about (e.g. both `c9s` and `c10s`) —
both workloads will be written to the same output file.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
