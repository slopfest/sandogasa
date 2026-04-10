# sandogasa-report

Activity reporting for Fedora, EPEL, and CentOS SIG packaging work.

Generates Markdown or JSON reports summarizing a contributor's
packaging activity across multiple systems:

- **Bugzilla**: review requests submitted/completed, reviews done for
  others, CVE/security fixes, update requests, branch requests,
  FTBFS/FTI bugs
- **Bodhi**: updates submitted, pushed to testing, pushed to stable,
  per-release breakdown
- **Koji CBS**: new packages and version updates in CentOS SIG
  release tags, with date-range comparison

## Installation

```sh
cargo install sandogasa-report
```

Requires `koji` CLI for CentOS SIG reporting
(`sudo dnf install koji`).

## Usage

```sh
# Report on Fedora activity for Q1 2026
sandogasa-report -c config.toml -d fedora \
    --user username --period 2026Q1

# Detailed report with per-item listings
sandogasa-report -c config.toml -d fedora \
    --user username --period 2026Q1 --detailed

# Multiple domains in one report
sandogasa-report -c config.toml -d fedora -d hyperscale \
    --user username --period 2026Q1

# Arbitrary date range (inclusive)
sandogasa-report -c config.toml -d epel \
    --user username --since 2026-01-01 --until 2026-06-30

# Full year, half year, and quarter periods
sandogasa-report -c config.toml -d hyperscale --period 2025
sandogasa-report -c config.toml -d hyperscale --period 2025H2
sandogasa-report -c config.toml -d hyperscale --period 2025Q4

# JSON output to file
sandogasa-report -c config.toml -d fedora \
    --user username --period 2026Q1 --json -o report.json

# Skip specific data sources for faster testing
sandogasa-report -c config.toml -d fedora \
    --user username --period 2026Q1 --no-bugzilla --no-bodhi
```

### Useful flags

- `-c, --config <PATH>` — path to config file (required)
- `-d, --domain <DOMAIN>` — domain(s) to report on (repeatable)
- `-u, --user <USER>` — FAS username to report on
- `--period <PERIOD>` — reporting period (2026, 2026H1, 2026Q1)
- `--since <DATE>` / `--until <DATE>` — date range (inclusive)
- `--detailed` — include per-item details, not just counts
- `--json` — output as JSON instead of Markdown
- `-o, --output <PATH>` — write output to file
- `--no-bugzilla` / `--no-bodhi` / `--no-koji` — skip data sources
- `-v, --verbose` — print progress to stderr

## Configuration

A TOML config file defines domains, user email mappings, and
package groups. See `configs/sandogasa-report/config.toml` for a
full example.

```toml
# FAS username → Bugzilla email mapping.
# If not set, looked up via FASJSON (requires Kerberos).
[users]
# username = "user@example.com"

# Domain presets define which data sources to query.
[domains.fedora]
bugzilla = true
bodhi = true
bodhi_releases = ["F*", "EPEL-*"]
fedora_versions = [42, 43, 44]

[domains.epel]
bugzilla = true
bodhi = true
bodhi_releases = ["EPEL-*"]

[domains.hyperscale]
koji_profile = "cbs"
koji_tags = [
    "hyperscale{9,10}{,s}-packages-{main,facebook}-release",
]

# Package groups for categorical reporting.
# Group keys are prettified for headings (e.g. "developer-tools"
# becomes "Developer Tools"). Optional description appears below.
[groups.hardware-enablement]
description = "Hardware enablement and GPU support"
packages = ["intel-gpu-tools", "libdrm", "mesa"]

[groups.developer-tools]
packages = ["neovim", "helix", "fish"]
```

### Koji tag patterns

Tag patterns support shell-style brace expansion:
`hyperscale{9,10}{,s}-packages-main-release` expands to all
combinations (hyperscale9-packages-main-release,
hyperscale9s-packages-main-release, etc.).

### FTBFS/FTI tracking

Bugzilla bugs that block known FTBFS (`F{ver}FTBFS`,
`RAWHIDEFTBFS`) or FTI (`F{ver}FailsToInstall`,
`RAWHIDEFailsToInstall`) trackers are classified separately.
Set `fedora_versions` on the domain to specify which Fedora
release trackers to look up.

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
