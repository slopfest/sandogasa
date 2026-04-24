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
- **GitLab**: MRs opened / merged / approved / commented on, plus
  pushed commit counts per project. Optionally scoped by group
  prefix (`CentOS/Hyperscale`, `CentOS/Hyperscale/rpms`, etc.)

## Installation

```sh
cargo install sandogasa-report
```

Requires `koji` CLI for CentOS SIG reporting
(`sudo dnf install koji`).

## Usage

Two subcommands:

- `sandogasa-report report` — generate an activity report. Takes
  the date range, domain list, and output format.
- `sandogasa-report config` — interactive setup of the per-user
  overlay (see [Configuration](#configuration)). Walks each
  GitLab-enabled domain from the main config and prompts for your
  username on that instance.

```sh
# Interactive overlay setup
sandogasa-report config -c config.toml

# Report on Fedora activity for Q1 2026
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1

# Detailed report with per-item listings
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1 --detailed

# Multiple domains in one report
sandogasa-report report -c config.toml -d fedora -d hyperscale \
    --user username --period 2026Q1

# Arbitrary date range (inclusive)
sandogasa-report report -c config.toml -d epel \
    --user username --since 2026-01-01 --until 2026-06-30

# Full year, half year, and quarter periods
sandogasa-report report -c config.toml -d hyperscale --period 2025
sandogasa-report report -c config.toml -d hyperscale --period 2025H2
sandogasa-report report -c config.toml -d hyperscale --period 2025Q4

# JSON output to file
sandogasa-report report -c config.toml -d fedora \
    --user username --period 2026Q1 --json -o report.json

# Skip specific data sources for faster testing
sandogasa-report report -c config.toml -d fedora \
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
- `--no-bugzilla` / `--no-bodhi` / `--no-koji` / `--no-gitlab` —
  skip data sources
- `-v, --verbose` — print progress to stderr

## Configuration

Configuration is layered: a **main config** passed via `-c` holds
the shared structure (domains, groups, koji tags, GitLab instance
URLs), and a **user overlay** at
`~/.config/sandogasa-report/config.toml` is auto-loaded and
deep-merged on top. Overlay values win at every nesting level;
missing overlay keys leave the main value unchanged.

This lets a team check in one shared `config.toml` and each user
keep their personal bits (GitLab usernames, Bugzilla emails,
instance-specific tweaks) in their own home config.

See `configs/sandogasa-report/config.toml` for a full main-config
example.

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
[domains.hyperscale.gitlab]
instance = "https://gitlab.com"
group = "CentOS/Hyperscale"
# user = "michel-slm"  # optional, if GitLab username ≠ FAS login

[domains.debian.gitlab]
instance = "https://salsa.debian.org"
# No group filter → all user activity on this instance counts.
user = "michel"  # salsa username often differs from FAS login

# Package groups for categorical reporting.
# Group keys are prettified for headings (e.g. "developer-tools"
# becomes "Developer Tools"). Optional description appears below.
[groups.hardware-enablement]
description = "Hardware enablement and GPU support"
packages = ["intel-gpu-tools", "libdrm", "mesa"]

[groups.developer-tools]
packages = ["neovim", "helix", "fish"]
```

### Per-user overlay

A typical overlay at `~/.config/sandogasa-report/config.toml`
might look like:

```toml
# A profile ties together the usernames one person uses across
# services. `sandogasa-report report --user michel` then looks
# this up and queries each backend with the right identity.
[users.michel]
fas = "salimma"                       # default if omitted: the profile key
bugzilla_email = "michel@example.com" # optional; FASJSON fallback otherwise

[users.michel.gitlab]
"gitlab.com" = "michel-slm"
"salsa.debian.org" = "michel"

# Persisted GitLab API tokens — populated by
# `sandogasa-report config`. Env vars still win if set.
[gitlab_tokens]
"gitlab.com" = "glpat-..."
"salsa.debian.org" = "glpat-..."
```

### Koji tag patterns

Tag patterns support shell-style brace expansion:
`hyperscale{9,10}{,s}-packages-main-release` expands to all
combinations (hyperscale9-packages-main-release,
hyperscale9s-packages-main-release, etc.).

### GitLab authentication

Each GitLab instance needs an API token. The tool looks up the
token from, in order:

1. `GITLAB_TOKEN_<HOSTNAME>` — instance-specific, hostname dots
   replaced by underscores and uppercased. For `gitlab.com` this
   is `GITLAB_TOKEN_GITLAB_COM`; for `salsa.debian.org` it's
   `GITLAB_TOKEN_SALSA_DEBIAN_ORG`.
2. `GITLAB_TOKEN` — the generic fallback shared with other
   sandogasa tools.
3. `gitlab_tokens.<hostname>` in the overlay — persisted by
   `sandogasa-report config`, so tokens survive across shell
   sessions without re-exporting env vars.

Env vars win over config, so a one-off shell override still
works even with a persisted token. The overlay file is kept at
`~/.config/sandogasa-report/config.toml` with 0600 permissions.

### FTBFS/FTI tracking

Bugzilla bugs that block known FTBFS (`F{ver}FTBFS`,
`RAWHIDEFTBFS`) or FTI (`F{ver}FailsToInstall`,
`RAWHIDEFailsToInstall`) trackers are classified separately.
Set `fedora_versions` on the domain to specify which Fedora
release trackers to look up.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
