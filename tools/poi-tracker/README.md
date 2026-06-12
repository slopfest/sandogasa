# poi-tracker

Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs.

Manages TOML-based inventories of packages that an organization
tracks across distributions. Supports exporting to content-resolver
YAML and hs-relmon manifest formats.

## Installation

```sh
cargo install poi-tracker
```

## Usage

### Show inventory

```sh
poi-tracker show -i inventory.toml
poi-tracker show -i inventory.toml --workload hyperscale
poi-tracker show -i inventory.toml --json
```

### Add / remove packages

```sh
poi-tracker add systemd -i inventory.toml \
    --poc "Team <team@example.com>" \
    --rpm systemd-networkd \
    --workload hyperscale \
    --track upstream

poi-tracker remove systemd -i inventory.toml
poi-tracker remove systemd -i inventory.toml --rpm systemd-networkd
```

### Export to content-resolver YAML

```sh
# Export all workloads (one YAML per workload)
poi-tracker export content-resolver -i inventory.toml

# Export a single workload
poi-tracker export content-resolver -i inventory.toml \
    --workload hyperscale

# Custom output path (single workload only)
poi-tracker export content-resolver -i inventory.toml \
    --workload hyperscale -o custom.yaml
```

### Export to hs-relmon manifest

```sh
# Merge multiple inventories into one manifest
poi-tracker export hs-relmon \
    -i inv-cloud.toml -i inv-hw.toml -o manifest.toml

# Filter by workload
poi-tracker export hs-relmon -i inventory.toml \
    --workload hyperscale -o manifest.toml
```

### Find a package

```sh
poi-tracker find systemd -i inv1.toml -i inv2.toml
```

### Sync from dist-git

Create or update an inventory from packages a user or group has
access to on Fedora dist-git (Pagure). Re-running merges new
packages without overwriting existing entries or annotations.

```sh
# All packages for a user
poi-tracker sync-distgit --user salimma -o my.toml

# All packages for a group
poi-tracker sync-distgit --group kde-sig -o kde.toml

# Exclude packages with only group-based access
poi-tracker sync-distgit --user salimma --no-groups

# Only packages from specific groups
poi-tracker sync-distgit --user salimma \
    --include-group rust-sig,python-packagers-sig

# Exclude specific groups
poi-tracker sync-distgit --user salimma \
    --exclude-group rust-sig

# Add workload tags to all imported packages
poi-tracker sync-distgit --group kde-sig \
    --workload kde -o kde.toml

# Remove packages no longer in dist-git results
poi-tracker sync-distgit --user salimma --prune -o my.toml

# Resume an interrupted sync from f*, stop before m*
poi-tracker sync-distgit --user salimma \
    --start-pattern f --end-pattern m
```

User syncs query Pagure one name prefix at a time (`a*`–`z*`,
`0*`–`9*`) by default: an unfiltered per-user project query is too
expensive for Pagure to answer within its gateway timeout, so it
returns HTTP 504. Splitting the query by name prefix keeps each
request small enough to succeed. (See
[`sandogasa-distgit`'s development notes](../../crates/sandogasa-distgit/DEVELOPMENT.md)
for the details — including why group syncs need no such
workaround.) `--pattern` switches to a single patterned query
instead, and `--no-auto-prefix` forces a single unfiltered query. `--start-pattern` / `--end-pattern` bound the prefix scan
(e.g. to resume an interrupted sync: start at this prefix / stop
before this prefix) and imply prefix mode, as does
`--auto-prefix` — which is how a group sync opts into scanning.
If both `--auto-prefix` and `--no-auto-prefix` are given, the
last one wins.

Packages where the user has both direct and group-based access
are always included, regardless of group filters.

Without `--prune`, packages in the inventory that are no longer
in the dist-git results are listed as a warning but kept.

Transient network failures are retried with backoff (both 5xx
responses and connection errors). If a fetch still fails, the
progress so far is saved to `<output>.partial` along with the
failed pattern in `<output>.partial.state` — re-running the same
command resumes from the failed pattern, and a completed run
replaces `<output>` and removes both files. Delete the
`.partial` to start over instead.

For user syncs, `--fast` replaces the whole prefix scan with one
request against Pagure's owner-alias dump. The trade-off: the
dump only records direct owner/admin/commit maintainers, so
collaborator- and ticket-level grants won't appear (and
`--prune --fast` would *remove* them from an inventory the full
scan had populated). It implies `--no-groups`; `--pattern` and
`--exclude` still apply, client-side.

A fast layout that covers nearly everything: keep one `--fast`
inventory for your own packages plus one group inventory per SIG
you're in (each a single cheap query) —

```sh
poi-tracker sync-distgit --user salimma --fast -o mine.toml
poi-tracker sync-distgit --group rust-sig -o rust-sig.toml
poi-tracker sync-distgit --group go-sig -o go-sig.toml
```

Together those cover everything except user-level
collaborator/ticket grants, which only the full prefix scan can
see — run one occasionally to true up. The full trade-off
analysis lives in
[`sandogasa-distgit`'s development notes](../../crates/sandogasa-distgit/DEVELOPMENT.md).

### Import from legacy JSON

```sh
poi-tracker import old-inventory.json -o inventory.toml \
    --private-fields poc,reason,team,task \
    --workload hyperscale
```

### Validate

```sh
poi-tracker validate -i inventory.toml
```

### Configure (Bugzilla API key)

```sh
poi-tracker config
```

Prompts for a Bugzilla API key, validates it with a quick test
search, and saves it to `~/.config/poi-tracker/config.toml`.
Lookup order at runtime: `--api-key` flag → `BUGZILLA_API_KEY`
env var → config file.

Generate an API key at
<https://bugzilla.redhat.com/userprefs.cgi?tab=apikey>.

### Audit pending updates by semver impact

`semver-audit` looks at each maintained package's pending upstream
release notification (the open `upstream-release-monitoring@`
"X is available" bug) and classifies the version bump against the
version currently packaged in rawhide dist-git, so you can see
which updates are safe to push and which need care:

```sh
# All pending updates, grouped by impact
poi-tracker -i inventory.toml semver-audit

# Just the safe ones for your Rust packages
poi-tracker -i inventory.toml semver-audit --pattern 'rust-*' --non-breaking

# Machine-readable
poi-tracker -i inventory.toml semver-audit --json
```

Bumps are classified with Cargo's compatibility rule (the Rust
convention): a change at or before the version's leftmost non-zero
component is **breaking**. So `1.4 → 1.5` is non-breaking, but
`0.4 → 0.5` is breaking (pre-1.0 minor bumps can break), and
`0.0.3 → 0.0.4` is breaking too. Versions that aren't plain dotted
integers — pre-releases, dates, git snapshots — are reported as
**needs review** rather than guessed at. A package whose packaged
version already equals the "available" version (a stale
release-monitoring bug, nothing to push) is reported as **up to
date (stale bug)**. A package that's retired on rawhide (a
`dead.package` marker — the same signal `triage-retired` uses) is
reported as **retired (update request invalid)**, since there's no
live package to update; run `triage-retired` to close those bugs.

`--pattern <glob>` (comma-separated or repeated, e.g. `rust-*`)
limits the audit to matching packages, and `--non-breaking` shows
only the safe updates. The audit makes a Bugzilla search and a
dist-git spec fetch per matching package, so scope it with
`--pattern` for a large inventory — or use `--batch [EMAIL]`,
which replaces the per-package searches with **one** Bugzilla
query for all open release-monitoring bugs assigned to or CC'ing
EMAIL (default: the email set via `poi-tracker config`), matched
against the inventory locally. Batch mode misses bugs where that
email is neither assignee nor CC'd, so it fits inventories of
packages you (co-)maintain or watch.

### Triage update bugs

Some packages reliably need attention when a new upstream version
appears — `python-django*` updates almost always fix CVEs, for
instance. Mark them in the inventory with a `priority` field (or
a workload-level `default_priority`), then have poi-tracker
triage the auto-filed release-monitoring bugs by raising their
Bugzilla priority:

```sh
poi-tracker -i inventory.toml triage-updates --dry-run
poi-tracker -i inventory.toml triage-updates
```

For each inventoried package with a resolved priority, this
queries OPEN bugs reported by `upstream-release-monitoring@
fedoraproject.org` (against `Fedora` and `Fedora EPEL`) and
raises any whose priority is `unspecified`. Bugs already
triaged by a human are left alone.

Per-package `priority` wins over `default_priority`; if a
package is in multiple workloads, the highest workload
default applies. Set `priority = "unspecified"` on a package
to explicitly opt out of a workload default.

Independently of priorities, every open release-monitoring bug
is also checked against Bodhi for builds that already carry the
advertised version (or newer). When found, the latest addressing
build per release is recorded in the bug's **Fixed In Version**
field, and:

- stable in **every** active release the package has a branch
  for → the bug is closed as `ERRATA`, with a comment listing
  the Bodhi updates;
- any addressing update still in **testing** → the bug is moved
  to `MODIFIED` (a later run closes it once everything is
  stable);
- addressed only in **some** releases (commonly just rawhide,
  since stable branches often intentionally stay behind) → you
  are asked before closing. `--close-stale` closes these without
  asking; under `-y` they are skipped unless `--close-stale` is
  given.

Builds that shipped before the current releases existed have no
Bodhi record (they were inherited at branching); for those the
branch's dist-git spec is consulted instead, so years-old stale
bugs close too. Genuinely pending bugs are cheap: rawhide is
checked first, and since a stable release may never carry a newer
version than rawhide, a version absent from rawhide skips the
stable-release queries entirely (EPEL bugs, whose branches update
independently, are always checked in full). Pass `--skip-stale`
to disable the whole check (also restoring the cheaper
priority-only scan), and `--pattern <glob>` (e.g. `rust-*`) to
scope the run. `--batch [EMAIL]` works as in `semver-audit`: one
Bugzilla query for everything assigned to or CC'ing EMAIL
(default: the configured email) instead of one query per package.

### Mark packages no longer shipped anywhere

`prune-retired` finds inventory packages that are no longer
carried on **any** active branch — the dist-git project is gone
(404), it has no branch on an active release, or it carries a
`dead.package` marker on every active branch it has. The active
branch set is queried from Bodhi's active releases (plus
rawhide) or overridden with `--branch`:

```sh
poi-tracker -i inventory.toml prune-retired --dry-run
poi-tracker -i inventory.toml prune-retired
```

By default matches are *marked* with an `unshipped` reason in
the inventory rather than deleted: retired packages keep their
ACLs, so a deleted entry would come straight back on the next
`sync-distgit` run, and the marker is what lets the rest of the
tooling do the right thing. `triage-updates` and `semver-audit`
skip unshipped packages; `triage-retired` still processes them
so their remaining bugs get closed; the sync commands' `--prune`
preserves them. Markers are refreshed in both directions — a
revived package gets its marker cleared. Pass `--remove` to
delete the entries outright instead.

### Close retired packages' update bugs

When a package gets retired on a dist-git branch (a
`dead.package` file is committed), any open release-monitoring
bug for that branch is dead weight — there's no live spec to
update. `triage-retired` walks the inventory, checks dist-git
for retirement, and closes those bugs as `CLOSED/CANTFIX`:

```sh
poi-tracker -i inventory.toml triage-retired --dry-run
poi-tracker -i inventory.toml triage-retired
```

The `--branch` flag controls which dist-git branch(es) are
checked (default `rawhide`); each branch scopes its own Bugzilla
search, so an `epel9` retirement closes the
`Fedora EPEL`/`epel9` bug. Pass it more than once (or as a
comma-separated list) to check several branches in one run — a
package retired on some branches but live on others only has
its bugs closed for the branches where it's actually dead:

```sh
poi-tracker -i inventory.toml triage-retired --branch epel10
poi-tracker -i inventory.toml triage-retired --branch epel8,epel9
```

Note that retirement (a `dead.package` marker) is distinct from
"never existed": a bug filed against a branch the package was
never built for is *not* a retirement and is left untouched —
`triage-retired` only closes bugs on branches where a
`dead.package` is present.

By default only release-monitoring bugs (filed by the Anitya /
the-new-hotness bot) are closed — those are mechanical and safe
to bulk-close. Pass `--all-reporters` to instead close **every**
open bug on the retired branch, including human-filed ones (CVEs,
FTBFS, etc.). Use it deliberately: across a full inventory run it
closes a lot, and a CVE filed only against the retired branch
(with no live-branch counterpart) would be closed as CANTFIX too:

```sh
poi-tracker -i inventory.toml triage-retired \
    --branch epel8,epel9 --all-reporters
```

Bugs that are already `CLOSED` are skipped. Each closure adds a
short comment naming the package and the retired branch.

Interactive runs offer to claim ownership of each closed bug
(set `assigned_to` to your configured Bugzilla email). Pass
`--claim` to claim without prompting — under `-y` this is the
only way to opt in. The email is set via `poi-tracker config`.

Pass `--mark` (needs a single `-i` file; conflicts with
`--dry-run`) to record the run's findings in each package's
`retired_on` field — in both directions, so a branch found live
again is removed. `semver-audit` and `triage-updates` skip
packages marked retired on rawhide, saving their per-package
queries; re-running `triage-retired --mark` is how the markers
are refreshed.

Useful flags for big inventories (shared by all
inventory-walking commands — `semver-audit`, `triage-retired`,
and `triage-updates` — and freely combinable):

- `--pattern <glob>` — only process matching packages
  (comma-separated or repeated; a bare name matches exactly,
  e.g. `--pattern python-django3` to check a single package).
- `--start-from <name>` — resume from this package onwards in
  the inventory's iteration order, e.g. to continue an
  interrupted run.
- `--end-with <name>` — stop after this package (inclusive).
  Combine with `--start-from` to scope to a name-range, e.g.
  `--start-from rust-nu-cli --end-with rust-nu-utils` to test
  the change against every `rust-nu-*` package in one shot.
- `--batch [EMAIL]` — one Bugzilla query for everything assigned
  to or CC'ing EMAIL (default: the configured email) instead of
  one query per retired package per branch; with
  `--all-reporters` the batch query drops the reporter filter
  too.

Network reads (dist-git probes, Bugzilla searches) retry up to
3 times with exponential backoff, so a transient connection
hiccup against `src.fedoraproject.org` doesn't abort the whole
inventory.

## Inventory format

```toml
[inventory]
name = "hyperscale-packages"
description = "CentOS Hyperscale SIG packages"
maintainer = "centos-hyperscale"
labels = ["eln-extras"]
private_fields = ["poc", "reason", "team", "task"]

[inventory.workloads.hyperscale]
name = "hs-packages"
description = "Hyperscale SIG workload"
labels = ["eln-extras"]

[inventory.workloads.epel]
name = "hs-epel-packages"
description = "Hyperscale EPEL workload"

[[package]]
name = "systemd"
poc = "Linux Userspace <team@example.com>"
reason = "Core init system"
rpms = ["systemd-networkd"]
workloads = ["hyperscale"]
track = "upstream"

[package.arch_rpms]
x86_64 = ["systemd-boot-unsigned"]
aarch64 = ["systemd-boot-unsigned"]

[[package]]
name = "fish"
rpms = ["fish"]
workloads = ["hyperscale", "epel"]
track = "upstream"
```

### Fields

| Field | Level | Description |
|-------|-------|-------------|
| `name` | inventory/package | Name (required) |
| `description` | inventory | Human-readable description |
| `maintainer` | inventory | Maintainer (person or team) |
| `labels` | inventory | Default labels for content-resolver |
| `workloads` | inventory | Workload definitions (map) |
| `workloads` | package | Workload membership (list) |
| `private_fields` | inventory | Fields stripped on export |
| `poc` | package | Point of contact |
| `reason` | package | Reason for tracking |
| `team` | package | Team responsible |
| `task` | package | Internal task/ticket |
| `rpms` | package | Binary RPMs to track |
| `arch_rpms` | package | Architecture-specific RPMs |
| `track` | package | hs-relmon tracking branch |
| `repology_name` | package | Repology name override |
| `distros` | package | hs-relmon distribution list |
| `file_issue` | package | File GitLab issues |
| `priority` | package | Bugzilla priority for `triage-updates` (`unspecified`/`low`/`medium`/`high`/`urgent`) |
| `retired_on` | package | Dist-git branches where the package is retired; written by `triage-retired --mark` |
| `unshipped` | package | Reason the package is no longer shipped on any active branch; written by `prune-retired`. Skipped by most operations, still processed by `triage-retired`, preserved by sync `--prune` |
| `default_priority` | workload | Default Bugzilla priority for packages in this workload |

Each `[inventory.workloads.<key>]` section can override `name`,
`description`, `maintainer`, `labels`, and `default_priority`
for content-resolver export and `triage-updates`. Omitted
fields fall back to inventory-level values.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
