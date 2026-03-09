<!-- SPDX-License-Identifier: MPL-2.0 -->

# hs-relmon

Release monitoring tool for [CentOS Hyperscale SIG](https://sigs.centos.org/hyperscale/) packages.

Compares package versions across upstream, Fedora, CentOS Stream, and
Hyperscale to identify outdated packages.

## Usage

```
hs-relmon check-latest <package> [--distros <list>] [--track <distro>]
    [--repology-name <project>] [--json] [--file-issue [<url>]]
hs-relmon config
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
Override the project URL:

```
$ hs-relmon check-latest ethtool --file-issue https://gitlab.com/other/project
```

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
