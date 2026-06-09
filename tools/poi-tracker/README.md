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
`0*`–`9*`) by default: the unfiltered per-user query scans every
project's ACLs server-side and routinely exceeds Pagure's gateway
timeout (HTTP 504). `--pattern` switches to a single patterned
query instead, and `--no-auto-prefix` forces a single unfiltered
query. `--start-pattern` / `--end-pattern` bound the prefix scan
(e.g. to resume an interrupted sync: start at this prefix / stop
before this prefix) and imply prefix mode, as does
`--auto-prefix` — which is how a group sync opts into scanning.
If both `--auto-prefix` and `--no-auto-prefix` are given, the
last one wins.

The pre-0.12.1 scan-resume spelling `--auto-prefix --pattern
<start>` still works but is deprecated and will be removed in
0.13.0; using it prints a warning (see the repository's
[DEPRECATIONS.md](../../DEPRECATIONS.md)).

Packages where the user has both direct and group-based access
are always included, regardless of group filters.

Without `--prune`, packages in the inventory that are no longer
in the dist-git results are listed as a warning but kept.

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

Bugs that are already `CLOSED` are skipped. Each closure adds a
short comment naming the package and the retired branch.

Interactive runs offer to claim ownership of each closed bug
(set `assigned_to` to your configured Bugzilla email). Pass
`--claim` to claim without prompting — under `-y` this is the
only way to opt in. The email is set via `poi-tracker config`.

Useful flags for big inventories:

- `--package <name>` — only check this one package. Handy for
  testing or when re-running after fixing a single entry.
- `--start-from <name>` — resume from this package onwards in
  the inventory's iteration order, e.g. to continue an
  interrupted run.
- `--end-with <name>` — stop after this package (inclusive).
  Combine with `--start-from` to scope to a name-range, e.g.
  `--start-from rust-nu-cli --end-with rust-nu-utils` to test
  the change against every `rust-nu-*` package in one shot.

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
