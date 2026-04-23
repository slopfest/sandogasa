# cpu-sig-tracker

Track [CentOS Proposed Updates (CPU) SIG][cpu-sig] package state across
Koji, GitLab, and JIRA.

[cpu-sig]: https://sigs.centos.org/proposed-updates/

The CPU SIG temporarily ships fixed packages (typically CVE backports)
while CentOS Stream catches up to RHEL's security fixes. This tool
automates the polling and nudging around that workflow — classifying
each tracking issue's state, flagging rebase-against-newer-Stream
needs, keeping GitLab metadata (dates, work-item status) in sync
with reality, and driving the untag + retire flow once Stream has
caught up.

Per-package tracking issues live in the
[proposed_updates GitLab group][cpu-gitlab], one issue per
`(package, release)` pair. The tool keys off a
[`sandogasa-inventory`](../../crates/sandogasa-inventory/) TOML file
populated by `dump-inventory` (typically regenerated before each
status sweep).

[cpu-gitlab]: https://gitlab.com/groups/CentOS/proposed_updates/-/work_items

## Installation

```sh
cargo install cpu-sig-tracker
```

Requires the [`koji`](https://pagure.io/koji) CLI with a `cbs` profile
configured (CentOS Build System), and
[`fedrq`](https://github.com/gotmax23/fedrq) for CentOS Stream repo
queries. Both are looked up on `$PATH`.

## Configuration

Before any subcommand that talks to GitLab or JIRA:

```sh
cpu-sig-tracker config
```

Prompts for a GitLab personal access token (validated against
gitlab.com) and an optional Red Hat JIRA token. Tokens are written to
`~/.config/cpu-sig-tracker/config.toml` with 0600 permissions. Both
can also be supplied via `GITLAB_TOKEN` / `JIRA_TOKEN` env vars.
Anonymous JIRA access works for public issues.

## Subcommands

### `config`

Interactive token setup (see above).

### `dump-inventory`

```sh
cpu-sig-tracker dump-inventory --release c9s,c10s -o inventory.toml
```

Enumerates packages tagged into `proposed_updates<N>s-packages-main-release`
for each release and writes a `sandogasa-inventory` TOML file. Safe
to re-run — existing entries are preserved, newly-discovered packages
are added, and each release becomes its own workload.

Use `--prune` to drop packages from the workload that are no longer
tagged in either `-release` or `-testing`. Orphan `[[package]]`
metadata blocks are left in place so user-entered fields (poc,
reason, team, …) survive a re-build.

### `file-issue`

```sh
cpu-sig-tracker file-issue <mr-url> \
    [--affected VER-REL] [--expected-fix VER-REL] \
    [--jira RHEL-N] [--release cNs] [--type security] \
    [--note "context"] [--dry-run]
```

Given a CentOS Stream MR URL, files a standardized tracking issue in
the corresponding `CentOS/proposed_updates/rpms/<pkg>` project.
Derives package / release / JIRA key automatically from the MR
(overridable), applies `cpu-sig-tracker`, release, and optional
type labels, sets the GitLab work-item status to `In progress`, and
stamps `start_date` from the Koji build's creation time.

The issue body follows a canonical format that `status` parses back
(MR, JIRA, Release, Affected build, Expected fix).

### `retire`

```sh
cpu-sig-tracker retire <issue-url> [--yes] [--force]
```

Closes a tracking issue after verifying the linked JIRA is resolved
and the package is no longer tagged in `-release` Koji. Sets GitLab
work-item status to `Done` / `Won't do` (mirroring the JIRA
resolution), stamps `due_date` from JIRA's `resolutiondate`, leaves
an audit-trail comment, and transitions the issue to closed.
`--yes` skips the prompt; `--force` bypasses the precondition
checks.

### `status`

```sh
cpu-sig-tracker status -i inventory.toml \
    [--release cNs] [--package PKG,...] \
    [--refresh] [--include-closed] [--json]
```

Per-package report of every active tracking issue in the inventory's
releases: JIRA key + status, currently-tagged NVR, current Stream
NVR, and a suggested next action (`in-progress`, `rebase`,
`untag-candidate`, `retire-issue`, `not-yet-tagged`, `no-jira`, or
`—` once closed with no build tagged). `--json` emits a flat
serde-serialized array.

`--refresh` turns the read pass into a write pass: rewrites any body
that's drifted from the canonical format, backfills stale MR / JIRA
status lines, reconciles the GitLab work-item status against live
JIRA + Koji (Done / Won't do / In progress / To do), and sets
missing `start_date` / `due_date` via the GraphQL work-item API.
`--include-closed` extends the refresh scan to historical tracking
issues so their dates can be backfilled after the fact.

### `sync-issues`

```sh
cpu-sig-tracker sync-issues -i inventory.toml [--release cNs] [--json]
```

Gap analysis: for every inventory package, checks whether a tracking
issue exists. Classifies each as `active` (open per-package issue),
`proposed` (only in the central `proposed_updates/package_tracker`),
or `missing`. Read-only; file new tracking issues explicitly via
`file-issue`.

### `untag`

```sh
cpu-sig-tracker untag <package|nvr> --release cNs [--yes] [--force]
```

Removes a proposed_updates build from its CBS `-release` and
`-testing` tags after verifying the linked JIRA is resolved. Accepts
either a package name (auto-discovers currently-tagged NVRs across
both tags) or a specific NVR. Pair with `retire` to close the
tracking issue after the build is gone.

## Typical workflow

```sh
# Refresh the inventory from CBS.
cpu-sig-tracker dump-inventory --release c9s,c10s -o cpu-sig.toml --prune

# File a tracking issue for a new MR.
cpu-sig-tracker file-issue https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42 \
    --type security --affected xz-5.6.2-3.el10 --expected-fix xz-5.6.4-1~proposed.el10

# See what needs attention.
cpu-sig-tracker status -i cpu-sig.toml

# Sync GitLab metadata (status, dates, body format).
cpu-sig-tracker status -i cpu-sig.toml --refresh

# Once Stream catches up: untag, then retire.
cpu-sig-tracker untag xz --release c10s --yes
cpu-sig-tracker retire https://gitlab.com/CentOS/proposed_updates/rpms/xz/-/work_items/1 --yes
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
