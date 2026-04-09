<!-- SPDX-License-Identifier: MPL-2.0 -->

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

[MPL-2.0](LICENSE)
