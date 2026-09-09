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
hs-relmon check-repos <manifest> [--package <list>] [--gitlab-group <url>]
    [--apply] [--yes] [--json] [--verbose]
hs-relmon config
hs-relmon dupe-subpkgs [--repositories <list>] [--release <list>]
    [--package <list>] [--fix] [--json] [--verbose]
hs-relmon file-conflicts [--repositories <list>] [--release <list>]
    [--package <list>] [--json] [--verbose]
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
hs-relmon check-stock <manifest> [--repositories <list>] [--skip <list>]
    [--verbose]
hs-relmon retire <package>... --manifest <path> [--gitlab-group <url>]
    [--dry-run] [--force] [--yes] [--verbose]
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

### Checking the repos' settings

Repos forked in from Fedora or created by hand drift from the SIG's
conventions in two settings: the default branch, which should be the
newest release's Hyperscale branch rather than the `rawhide` or `c10s`
the fork arrived with, and the merge method, which should be
fast-forward so branch history stays linear. `check-repos <manifest>`
reads every manifest package's repo under `--gitlab-group` and reports
what differs; `--apply` then asks per repo — `y` sets it, `s` (Enter)
skips it, `a` sets the rest, `q` stops — and `--yes` sets every one
unasked, since a change right for most repos can be wrong for one
whose builds have moved elsewhere:

```
$ hs-relmon check-repos packages.toml
awscli2: default branch c10s → c9s-hs; merge method merge → ff
dnsmasq: default branch rawhide → c10s-hs; merge method merge → ff
kernel: merge method merge → ff
mesa: default branch c10s → c10s-hs+asahi; merge method merge → ff
socat: merge method merge → ff [archived: read-only, not changed]
atuin: merge method merge → ff; no Hyperscale branch (c*s-hs, -hsx, -hs+fb, -sig-hyperscale…)
crun: ok
63 repo(s): 48 to change, 14 ok, 1 archived
```

Hyperscale branches come in several spellings — `c10s-hs`, the
variants `c10s-hsx` and `c10s-hsk`, the flavors `c10s-hs+fb` and
`c10s-hs+asahi`, and the older `c10s-sig-hyperscale[-…]` — and all
count, but only when somebody builds from them: a build's release tag
names its branch (`wprof-0.6-2.hsx.el9` came from `c9s-hsx`), and a
branch with no build tagged in any hyperscale `-release`/`-testing`
tag is passed over. The newest live release wins; within it, a default
that already is one of its live branches stays (a kernel repo on
`c10s-hsk` is deliberate), otherwise `-hs` is preferred, then a
variant, then a flavor, then the old spelling. A default pointing at a
Hyperscale branch nobody builds from is flagged, as is a repo none of
whose Hyperscale branches has a build; a repo with no Hyperscale branch
at all keeps its default and is said so. The merge method is set in
every case. CBS is read once for all of this, every release and testing
tag.
Archived repos are read-only and only reported. Needs a GitLab token
with Maintainer access on the repos (`GITLAB_TOKEN` or `hs-relmon
config`).

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

`dupe-subpkgs` scans each repository's `-release` and `-testing`
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
$ hs-relmon dupe-subpkgs
Found 4 duplicate binary RPM(s) across 1 tag(s):

hyperscale9s-packages-main-release:
  perf shipped by 2 sources:
    kernel-tools (kernel-tools-6.4.13-200.1.hs.el9)
    perf (perf-6.19~rc6-4.hs.el9)
  ...
```

Pass `--repositories` to scan repositories other than `main` (CSV),
`--release` to limit the scan to specific Hyperscale releases (CSV
of `9`, `9s`, `10`, `10s`; default all), `--package` to report only
collisions involving named source packages (CSV), `--json` for
machine-readable output, and `--verbose` to see each tag as it is
scanned. Both selectors take repeated flags or a comma-separated
list. Narrowing also makes a run much faster — e.g. `--release 9s
--package perf` touches a single tag.

`--fix` adds an interactive resolution pass. For each collision it
recommends untagging the oldest build (the likely stale leftover)
but lists, for every candidate, the binaries that *only* it provides
— those would disappear from the tag if it were untagged — so you
act with full context:

```
$ hs-relmon dupe-subpkgs --fix
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

### Detecting file conflicts

`dupe-subpkgs` catches two sources shipping the same binary RPM
*name* in one tag. The sharper breakage is a **file** conflict
between differently-named RPMs in *different* repos that are enabled
together: the `kernel` source ships `/usr/bin/ynl` and the `pyynl`
tree inside `python3-kernel-tools` (kernel repo), while a standalone
`python3-ynl` (main repo) ships the same paths — dnf hits a file
conflict, but the RPM names differ and they live in separate tags, so
name matching misses it.

`file-conflicts` scans, per EL version, the set of repos enabled
together (default `main` + `kernel` on EL10/10s; `main` only on
EL9/9s, which has no kernel repo), pulls each binary RPM's file list
from Koji — batched via `multicall`, so a whole tag is a handful of
requests, not one per RPM — and flags any path owned by two or more
distinct source packages. Directories, `%ghost` entries, and debug
payloads under `/usr/lib/debug` / `/usr/src/debug` are excluded.

```
$ hs-relmon file-conflicts
Found 192 conflicting file(s) across 1 source set(s):

hyperscale9s (repos: main):
  kernel-tools + perf — 192 file(s):
    /usr/bin/perf
    /usr/lib64/libperf.so.0
    …
```

Pass `--repositories` (CSV) to override the per-EL enabled set,
`--release` to limit which Hyperscale releases are scanned (`9`,
`9s`, `10`, `10s`; default all), `--package` to report only
conflicts involving named source packages, `--json` for
machine-readable output, and `--verbose` to watch the scan.
`--release` and `--package` each take repeated flags or a comma-
separated list. The full
repo set is still scanned even with `--package` (a package's
conflicts are only found by comparing it against everything else), so
that flag narrows the report, not the work. The scan is read-only and
exits non-zero when any conflict is found.

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
them. A build of stock's version but a **newer release** is
treated the same way, with its own prompt: a system running it
keeps it until stock's version moves (dnf does not downgrade; a
configuration manager told to upgrade would). Stock's release
comes from Repology's full `version-release`, so the tie-break
costs no extra request. Stock versions come from Repology, and only from what stands in
for the SIG package: a stock source of the same name, or one shipping
a binary of that name, since a consumer needs the binary whoever
builds it. CentOS Stream 9 ships `autoconf` 2.69 and `autoconf-latest`
2.71 (binaries `autoconf-latest` and `autoconf271`) and Repology files
both under one project, but only stock's `autoconf` says whether the
SIG's has been caught up with. `prune-archived`
requires `koji` with the `cbs` profile.

### Checking stock, and retiring a package it has caught up with

Some packages the SIG carries only until CentOS Stream catches up;
once it has, the SIG copy is noise. Others carry packaging changes of
the SIG's own — patches, configuration, subpackage layout — and when
stock moves the action is a rebase, never a retirement. CBS and
Repology cannot tell the two apart (a SIG build behind stock looks the
same either way), so the manifest declares it:

```toml
[[package]]
name = "pykickstart"
divergent = true     # carries SIG changes: rebase when stock moves

[[package]]
name = "crun"
divergent = false    # tracked until stock catches up: then retire
```

`check-stock <manifest>` compares every manifest package's builds in
its `-release`/`-testing` tags against the stock version for each
tag's channel (CentOS Stream N for a Stream tag, AlmaLinux N for a
RHEL tag, from Repology, the rule `prune-archived` applies) and says
what follows, one verdict word per line: `ahead` (the SIG is newer
than stock, its live work) first, then `release` (stock has the
version but the SIG's release is newer — retiring would leave a system
running the SIG build on it until stock's version moves, since dnf
does not downgrade while a configuration manager told to upgrade
would), then the caught-up packages —
`retire` when declared `divergent = false`, `rebase` when `true`,
`declare` when the field is unset — and last `none`, no build in the
managed tags at all (gone from CBS, or carried in a repository other
than `--repositories`). Stock means what ships the package by name, as
for `prune-archived`: `autoconf-latest` in stock, with no binary called
`autoconf`, does not catch up with the SIG's `autoconf`. Tags are shown without their boilerplate,
`10s-main-release` for `hyperscale10s-packages-main-release`; the
verdicts are colored on a terminal (`--color[=WHEN]` as in `ls`:
`auto`, `always`, `never`), and a legend closes the listing:

```
$ hs-relmon check-stock packages.toml --repositories main,facebook,experimental,kernel
ahead     dnsmasq                     2 build(s) newer than stock (10s-main-release 2.90; 9s-main-release 2.85)
…
retire    crun                        stock covers 2 build(s) (10s-main-testing 1.29.1; 9s-main-testing 1.29.1)
rebase    pykickstart                 stock covers 2 build(s) (10s-main-release 3.52.13)
declare   openssh                     stock covers 20 build(s) (10s-facebook-release 9.9p1; 10s-facebook-testing 9.9p1; 9s-facebook-release 9.9p1; 9s-facebook-testing 9.9p1)
…
none      sqlite                      no builds in the managed tags

ahead: the SIG is newer than stock  retire: stock has caught up, the package is temporary  rebase: …  declare: stock has caught up — set `divergent = false` (retire) or `true` (rebase) on the manifest entry  none: no builds in the managed tags
65 package(s): 40 ahead of stock, 1 to retire, 1 to rebase, 13 caught up but undeclared, 10 without builds
openssh: stock has caught up — temporary (retire) or divergent (rebase)? (t)emporary / (d)ivergent / (s)kip [s]:
```

On a terminal, the `declare` packages are then asked about one by one
— `t` writes `divergent = false` to the manifest entry, `d` writes
`true`, Enter or `s` leaves it undeclared, `q` stops asking — so the
declaration is made where the evidence is on screen. A piped run
(stdin not a terminal) only lists.

The managed tags are read once — the candidate names that exist on
the hub, then each tag's contents — and Repology is asked once per
package, a second apart as its terms require, so a manifest of
sixty-odd takes about a minute. Equal versions count as caught up.
`poi-tracker export hs-relmon` preserves per-package fields across
re-exports, so a `divergent` set at the prompt or by hand stays.

`retire <package>` then takes a package out, in one confirmation (none
with `--yes`; `--dry-run` shows the plan): its builds are untagged from
every hyperscale tag they are in — candidate tags included, which the
`prune-*` commands leave alone — it is removed from the manifest, and
its GitLab repo under `--gitlab-group` (the SIG's `rpms` group by
default) is archived:

```
$ hs-relmon retire crun --manifest packages.toml --dry-run
crun: hyperscale10s-packages-main-candidate [stock 1.29.1]
    untag (<= stock): crun-1.28-1.1.hs.el10
crun: hyperscale10s-packages-main-testing [stock 1.29.1]
    untag (<= stock): crun-1.28-1.1.hs.el10
crun: hyperscale9s-packages-main-candidate [stock 1.29.1]
    untag (<= stock): crun-1.28-1.1.hs.el9
crun: hyperscale9s-packages-main-testing [stock 1.29.1]
    untag (<= stock): crun-1.28-1.1.hs.el9
crun: retirable
    manifest: remove entry
    https://gitlab.com/CentOS/Hyperscale/rpms/crun: archive
```

Builds in the tags of a release the SIG no longer tracks —
`hyperscale8s-*`, CentOS Stream 8 being EOL — stay tagged, and are
listed as such: there is no stock to compare against and nothing to
gain by rewriting history. The manifest entry is removed and the repo
archived regardless. A package declared `divergent` is
refused — rebase it instead — unless `--force`. A build newer than
stock is never untagged under `--yes`,
and is prompted for individually otherwise (default no); while any such
build stays tagged the package is not retired — the untags already done
stand, but the manifest entry and the repo remain, since the SIG is
still that build's only source. `retire` requires `koji` with the
`cbs` profile and a GitLab token (`GITLAB_TOKEN` or `hs-relmon
config`); a dry run needs neither.

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

## System-wide configuration

Settings are read from `/etc/hs-relmon/config.toml` first, then
overridden per key by `~/.config/hs-relmon/config.toml`, with
command-line flags overriding both. A system file alone is enough — no
per-user file is required — and either may also carry a `[defaults]`
table pinning flag defaults (see the root `DEVELOPMENT.md`).

`hs-relmon config` writes the user file only, with 700 on the
directory and 600 on the file. Nothing writes under `/etc`: a system
file is admin-authored, holds shared non-secret settings, and is
normally shipped `root:root 0644`.

Credentials belong in the per-user file, which is 600, or in an
environment variable — a token under `/etc` is readable by every local
user. For an unattended machine, give the job its own user and its own
600 config.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
