<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# hs-relmon

Release monitoring tool for [CentOS Hyperscale SIG](https://sigs.centos.org/hyperscale/) packages.

Compares package versions across upstream, Fedora, CentOS Stream, and
Hyperscale to identify outdated packages.

## Installation

```
cargo install hs-relmon
```

## Usage

```
hs-relmon check-latest <package> [--distros <list>] [--track <distro>]
    [--repology-name <project>] [--json] [--file-issue [<url>]]
hs-relmon check-manifest <manifest> [--json]
    [--issue-status <status>] [--issue-assignee <username>]
hs-relmon config
hs-relmon list-issues [--group <url>] [--json]
    [--issue-status <status>] [--issue-assignee <username>]
    [--manifest <path>] [--add-missing]
hs-relmon prune-tags <package>
    [--release-keep <N>] [--testing-keep <N>] [--repositories <list>]
    [--dry-run] [--yes] [--verbose]
hs-relmon prune-manifest <manifest>
    [--release-keep <N>] [--testing-keep <N>] [--repositories <list>]
    [--skip <list>] [--dry-run] [--yes] [--verbose]
hs-relmon prune-archived <manifest>
    [--repositories <list>] [--skip <list>] [--dry-run] [--yes]
    [--verbose]
hs-relmon review [<package>|<nvr>]
    [--repositories <list>] [--skip <list>] [--dry-run] [--verbose]
```

### Examples

Check all distributions (default):

```
$ hs-relmon check-latest ethtool
ethtool
  Distribution    Version  Detail                  Status
  ──────────────  ───────  ──────────────────────  ──────
  Upstream        6.19
  Fedora Rawhide  6.19
  Fedora Stable   6.19     fedora_43
  CentOS Stream   6.15     centos_stream_10
  Hyperscale 9    6.15     ethtool-6.15-3.hs.el9   outdated
  Hyperscale 10   6.15     ethtool-6.15-3.hs.el10  outdated
```

Track against CentOS Stream instead of upstream:

```
$ hs-relmon check-latest ethtool --track centos-stream
```

Override the Repology project name:

```
$ hs-relmon check-latest perf --repology-name linux
```

Check only upstream and Hyperscale:

```
$ hs-relmon check-latest systemd --distros upstream,hyperscale
```

JSON output:

```
$ hs-relmon check-latest ethtool --json
```

### Distribution names for `--distros`

| Name | What it checks |
|------|---------------|
| `upstream` | Newest version across all repos (via Repology) |
| `fedora` | Fedora Rawhide + latest stable |
| `fedora-rawhide` | Fedora Rawhide only |
| `fedora-stable` | Latest stable Fedora only |
| `centos` / `centos-stream` | Latest CentOS Stream |
| `hyperscale` / `hs` | Hyperscale EL9 + EL10 |
| `hs9` | Hyperscale EL9 only |
| `hs10` | Hyperscale EL10 only |

### `--track` reference distributions

| Name | What it tracks against |
|------|----------------------|
| `upstream` | Newest version across all repos (default) |
| `fedora-rawhide` | Fedora Rawhide |
| `fedora-stable` | Latest stable Fedora |
| `centos` / `centos-stream` | Latest CentOS Stream |

### Filing GitLab issues

Automatically file or update a GitLab issue when a package is outdated:

```
$ hs-relmon check-latest ethtool --file-issue
```

This creates (or updates) an issue labeled `rfe::new-version` in the
default project `https://gitlab.com/CentOS/Hyperscale/rpms/ethtool`.
If a closed issue with the same title already exists, it is reopened
and labeled `reopened` instead of creating a duplicate.

Override the project URL:

```
$ hs-relmon check-latest ethtool --file-issue https://gitlab.com/other/project
```

### Checking a manifest

Check all packages listed in a TOML manifest file:

```
$ hs-relmon check-manifest packages.toml
```

The manifest uses `[defaults]` for shared settings and `[[package]]` entries
for each package:

```toml
[defaults]
distros = "upstream,fedora,centos,hyperscale"
track = "upstream"
file_issue = true

[[package]]
name = "ethtool"

[[package]]
name = "perf"
repology_name = "linux"

[[package]]
name = "systemd"
track = "fedora-rawhide"
file_issue = false
```

Filter by GitLab issue status or assignee:

```
$ hs-relmon check-manifest packages.toml --issue-status "To do"
$ hs-relmon check-manifest packages.toml --issue-assignee alice
```

Available issue statuses: `To do`, `In progress`, `Done`, `Canceled`.

### Configuration

Set up GitLab authentication (token stored in
`~/.config/hs-relmon/config.toml`):

```
$ hs-relmon config
Paste a GitLab personal access token with 'api' scope:
Validating token... valid.
Saved to /home/user/.config/hs-relmon/config.toml.
```

The `GITLAB_TOKEN` environment variable overrides the config file token.

### Detecting duplicate binaries

Hyperscale overrides stock CentOS packages and occasionally moves
where a binary RPM is built from — e.g. splitting `perf` out of
`kernel-tools` into its own source package. Mid-move, two source
packages can end up shipping the same binary RPM in the same tag;
whichever the depsolver picks is undefined, and the redundant
source should be retired.

`dupe-binaries` scans each repository's `-release` and `-testing`
tags (across EL9/EL10 and the Stream variants), asks Koji for the
binary RPMs in each (latest build per source, no inherited
base-distro content), and flags any binary name produced by two or
more distinct sources. Detection is per-tag, since a collision only
matters when both providers land in the same enabled repository.
`-debuginfo`/`-debugsource` RPMs are excluded — a collision there
only mirrors the base binary's. The scan is read-only (no Koji
authentication needed) and exits non-zero when any collision is
found.

```
$ hs-relmon dupe-binaries
Found 4 duplicate binary RPM(s) across 1 tag(s):

hyperscale9s-packages-main-release:
  perf shipped by 2 sources:
    kernel-tools (kernel-tools-6.4.13-200.1.hs.el9)
    perf (perf-6.19~rc6-4.hs.el9)
  ...
```

Pass `--repositories` to scan repositories other than `main`
(CSV), `--json` for machine-readable output, and `--verbose` to see
each tag as it is scanned.

`--fix` adds an interactive resolution pass. For each collision it
recommends untagging the oldest build (the likely stale leftover)
but lists, for every candidate, the binaries that *only* it provides
— those would disappear from the tag if it were untagged — so you
act with full context:

```
$ hs-relmon dupe-binaries --fix
hyperscale9s-packages-main-release:
  duplicate binaries: libperf, libperf-devel, perf, python3-perf
    [1] untag kernel-tools-6.4.13-200.1.hs.el9 (build 50532) [recommended, oldest]
        also removes from the tag (only provided here): kernel-tools, kernel-tools-libs, rtla, rv
    [2] untag perf-6.19~rc6-4.hs.el9 (build 74013)
        removes nothing else — ships only duplicated binaries
Untag which build? [1-2, Enter to skip]:
```

Here untagging the recommended `kernel-tools` would also drop
`rtla`, `rv`, and the `kernel-tools` binaries — so the right move is
a rebuilt `kernel-tools` that no longer ships `perf`, not a blind
untag. The default is to skip; `--fix` requires CBS write
authentication (`koji` configured for the `cbs` profile). In
`--json` mode or when stdout is not a terminal the plan is printed
and nothing is untagged. Archiving the redundant upstream project is
still left to `prune-archived` and the GitLab tooling.

### Listing issues

List all `rfe::new-version` issues under a GitLab group:

```
$ hs-relmon list-issues
```

Filter by status or assignee:

```
$ hs-relmon list-issues --issue-status "To do"
$ hs-relmon list-issues --issue-assignee none
```

Compare against a manifest to find packages with issues but not yet tracked:

```
$ hs-relmon list-issues --manifest packages.toml
```

Automatically add missing packages to the manifest (preserves comments):

```
$ hs-relmon list-issues --manifest packages.toml --add-missing
```

### Pruning old tagged builds

CBS Koji's hyperscale `-testing` and `-release` tags accumulate
old builds because nothing untags them automatically. `prune-tags`
walks a package's hyperscale builds, groups by tag, and untags
anything past the retention threshold. Output lists the builds
that will stay tagged alongside the ones to be untagged so you
can sanity-check before confirming:

```
$ hs-relmon prune-tags ethtool --dry-run
ethtool: would untag 7 build(s)
  hyperscale10s-packages-main-release: keep 2, untag 1
    keep:
      ethtool-6.19-1.hs.el10
      ethtool-6.18-1.hs.el10
    untag:
      ethtool-6.14-1.hs.el10
  hyperscale10s-packages-main-testing: keep 1, untag 3
    keep:
      ethtool-6.19-1.hs.el10
    untag:
      ethtool-6.18-1.hs.el10
      ethtool-6.15-3.hs.el10
      ethtool-6.14-1.hs.el10
  ...
```

Defaults: 2 builds kept per `-release` tag, 1 per `-testing`,
repository `main` only. Override:

```
$ hs-relmon prune-tags ethtool --release-keep 3 --testing-keep 2
$ hs-relmon prune-tags ethtool --repositories main,facebook
```

Beyond the keep-N retention, a `-testing` build whose version
is *not newer* than the latest build in the sibling `-release`
tag is always untagged from testing — once release has caught up
to or past it (a promoted build, or an older leftover), keeping
it in testing is pure noise.

Without `--dry-run` you get a per-package `[y/N]` prompt; pass
`-y/--yes` to skip. Untag operations run via `koji untag-build`
against the `cbs` profile (install `koji` and configure CBS auth
beforehand).

For batch use, `prune-manifest <path>` walks every package in a
manifest with the same options:

```
$ hs-relmon prune-manifest packages.toml --dry-run
```

Exclude packages that manage their own tag cleanup with
`--skip`:

```
$ hs-relmon prune-manifest packages.toml --skip systemd,kernel
```

`-candidate` and tags whose repository isn't in `--repositories`
are left alone.

### Pruning builds for archived packages

When a package's upstream repo is archived (recorded as
`archived = true` in the manifest by
`poi-tracker sync-gitlab --mark-unshipped`), its CBS builds
should eventually be retired once stock catches up.
`prune-archived <manifest>` walks the archived packages and, for
each build in their `-release`/`-testing` tags, compares the
build version against the **stock** distro version for that
tag's channel:

- Stream tags (`hyperscaleNs-…`) compare against CentOS Stream N.
- RHEL tags (`hyperscaleN-…`) compare against AlmaLinux N.

```
$ hs-relmon prune-archived packages.toml --dry-run
nvme-cli: 2 build(s) at/behind stock to untag, 0 ahead of stock
  hyperscale9s-packages-main-release [stock 2.16]
    untag (<= stock): nvme-cli-2.8-1.hs.el9
socat: 0 build(s) at/behind stock to untag, 1 ahead of stock
  hyperscale9s-packages-main-release [stock 1.7.4.1]
    ahead of stock:   socat-1.7.4.4-4.hs.el9
```

Builds at or behind stock are redundant and untagged (one batch
confirmation per package). Builds **ahead** of stock — or for
which stock has no entry at all — are never untagged
automatically: the archived repo may be their only source, so
each is prompted individually, and `--yes` warns about and skips
them. Stock versions come from Repology; `prune-archived`
requires `koji` with the `cbs` profile.

### Reviewing testing builds

Interactively review builds sitting in `-testing` tags and act
on each, in the spirit of `fedora-easy-karma`:

```
$ hs-relmon review                 # every build in testing
$ hs-relmon review dnsmasq         # latest dnsmasq build(s) in testing
$ hs-relmon review dnsmasq-2.92rel2-9.hs.el10   # one specific build
```

For each build it prints the build metadata, the
currently-released NVR for comparison, and the relevant
changelog (via `koji buildinfo --changelog`), then prompts:

- `+1` / `1` — promote: tag the build into the sibling
  `-release` tag and untag it from `-testing`.
- `-1` — reject: untag from `-testing`.
- `0` / `s` / Enter — skip, leave the build as-is.
- `q` / Ctrl-D — stop reviewing.

The changelog is scoped to what changed: for a package already
in release, only the entries newer than the released build are
shown; for a brand-new package the changelog is capped at
`--changelog-lines` (default 20). If a testing build is *not
newer* than what's in release (same version already released, or
a downgrade), review warns and leaves it alone — cleaning up the
stale testing tag is `prune-tags`' job.

`--repositories` (default `main`) selects which testing
repositories to scan; `--dry-run` lists the builds that would
be reviewed and exits without prompting.

Exclude packages that have their own release pipeline with
`--skip` (repeatable or comma-separated):

```
$ hs-relmon review --skip systemd,kernel
```

Skip wins over an explicit target, so a skipped package can't
be promoted even if you name it directly.

When a package name is given, its latest build in each testing
tag is reviewed. When an NVR is given, only that build is
reviewed (an NVR is recognised by its `.el` dist marker).

## Data sources

- **Repology** ([repology.org](https://repology.org/)) for upstream, Fedora,
  and CentOS Stream versions
- **CBS Koji** ([cbs.centos.org](https://cbs.centos.org/koji/)) for
  Hyperscale builds and tag status

## Building

```
cargo build --release
```

## Testing

```
cargo cov
```

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
