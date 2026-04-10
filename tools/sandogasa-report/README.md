# sandogasa-report

Activity reporting for Fedora, EPEL, and CentOS SIG packaging work.

Generates Markdown or JSON reports summarizing a contributor's
packaging activity across multiple systems:

- **Bugzilla**: review requests created/completed, CVE fixes
- **Bodhi**: updates pushed to stable, per-release breakdown
- **Koji CBS**: packages tagged into CentOS SIG release tags

## Installation

```sh
cargo install sandogasa-report
```

## Usage

```sh
# Report on Fedora activity for Q1 2026
sandogasa-report --user username --domain fedora --period 2026Q1

# Detailed EPEL report with per-package listing
sandogasa-report --user username --domain epel \
    --since 2026-01-01 --until 2026-06-30 --detailed

# Hyperscale SIG activity (no user filter)
sandogasa-report --domain hyperscale --period 2026H1

# JSON output to file
sandogasa-report --user username --domain fedora \
    --period 2026Q1 --json -o report.json
```

## Configuration

Create `~/.config/sandogasa-report/config.toml`:

```toml
# FAS username → Bugzilla email mapping.
# If not set, looked up via FASJSON (requires Kerberos).
[users]
username = "user@fedoraproject.org"

# Domain presets define which data sources to query.
[domains.fedora]
bugzilla = true
bodhi = true
bodhi_releases = ["F*", "EPEL-*"]

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
[groups]
hardware-enablement = ["intel-gpu-tools", "libdrm", "mesa"]
developer-tools = ["neovim", "helix", "fish"]
```

## License

[MPL-2.0](https://mozilla.org/MPL/2.0/)
