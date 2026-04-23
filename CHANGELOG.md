# Changelog

## Unreleased

### New: hs-meetings tool + sandogasa-meetbot library

CentOS Hyperscale SIG meeting archive helper. `hs-meetings
list` queries meetbot.fedoraproject.org for meetings whose
topic matches `centos-hyperscale-sig` (overridable) and prints
them as a date/topic/summary table or `--json`. Supports
calendar filters via `--period 2026Q1` (or `YYYY`, `YYYYH1`)
and explicit `--since` / `--until`. A `sync` subcommand that
merges missing meetings into the SIG docs' `meetings.md` is
planned.

Backed by a new `sandogasa-meetbot` library crate that wraps
meetbot's `/fragedpt/` search endpoint behind a typed blocking
client.

### sandogasa-cli: shared date-range helpers

`sandogasa-cli::date::{parse_period, resolve_date_range}`
extracted from sandogasa-report so hs-meetings can share the
same `--since/--until/--period` grammar. sandogasa-report
switched to the shared implementation; the grammar is
unchanged (`YYYY`, `YYYYQ1..Q4`, `YYYYH1..H2`).

### New: cpu-sig-tracker tool

Track CentOS Proposed Updates SIG package state across Koji,
GitLab, and JIRA. Manages the full lifecycle of each tracking
issue — filed when an MR against CentOS Stream exists, watched
until JIRA closes or Stream catches up, then retired and
untagged.

Subcommands:

- `config` — interactive GitLab + JIRA token setup
- `dump-inventory` — enumerate `proposed_updates<N>s-packages-main-release`
  contents into a sandogasa-inventory TOML; `--prune` drops
  packages no longer tagged in either `-release` or `-testing`
- `file-issue` — file a standardized tracking issue for an MR;
  auto-extracts package / release / JIRA key from the MR,
  applies labels, transitions work-item status to In progress,
  stamps start_date from Koji build creation time
- `retire` — close a tracking issue after verifying JIRA
  resolved + build untagged; mirrors JIRA resolution to
  GitLab (Done vs Won't do), stamps due_date, leaves an
  audit-trail comment
- `status` — per-package report with JIRA state + Koji/Stream
  NVR compare + suggested action; `--refresh` reconciles body
  format, work-item status, and start/due dates against live
  data; `--include-closed` extends the refresh scan to
  historical issues; `--package` and `--release` narrow the
  scan
- `sync-issues` — gap analysis per (release, package):
  active / proposed / missing classification
- `untag` — remove a proposed_updates build from both
  `-release` and `-testing` after verifying JIRA resolved;
  accepts either a package name or a specific NVR

Issue bodies follow a canonical markdown format so the read
side can parse back what the write side wrote; work-item
status, `start_date`, and `due_date` go via GraphQL since the
REST `PUT /issues` endpoint ignores them for work items.

### New: sandogasa-jira library crate

Minimal Red Hat JIRA REST client — issue lookup with
status / resolution / resolution date. Used by cpu-sig-tracker
to drive the retire and status flows.

### cov

- Raised the workspace line-coverage gate from 75% to 80%.
- Excluded `src/main.rs` files from the measurement — they're
  structurally 0% (the harness doesn't invoke main()) and the
  logic they delegate to is exercised by module tests.

### New: sandogasa-pkg-health tool

Audit package health across a sandogasa inventory via pluggable
checks classified by cost tier (cheap / medium / expensive).
Reports persist to TOML with selective per-(package, check,
variant) update — re-running one check preserves every other
stored entry's timestamp.

- `HealthCheck` trait (id, description, cost_tier, variants, run,
  format_result)
- Cost tiers: Cheap / Medium / Expensive
- Variant-aware checks (e.g. `bug_count:f45` vs `bug_count:epel10`)
  with independent per-variant staleness
- CLI: `run`, `show`, `checks` subcommands
- `--fedora-version` and `--epel-version` (CSV + repeatable, sorted
  and deduped with duplicate warnings)
- `--max-age` for age-based selective re-run
- `--package` and check selection flags for scoped updates
- Per-package parallelism via rayon (~3.4x speedup on 44 packages)
- JSON Schema for the report format (checked in, snapshot-tested)
- MVP checks: `maintainer_count` (Cheap), `bug_count` (Medium)
- `show` subcommand: display an existing report without re-running

### New: sandogasa-bugclass library crate

Bug classifier extracted from `sandogasa-report` into a shared
library so `sandogasa-pkg-health` can reuse it. The `BugKind` enum
is the tracker-agnostic vocabulary (Security, Ftbfs, Fti, Update,
Branch, Review, Other); per-tracker submodules hold the
classification logic. Currently only Bugzilla is supported.

## v0.10.1

### ebranch

- `check-update`: add installability check for updated packages —
  catches missing dependencies (e.g. `comfy-table`) that would make
  subpackages uninstallable
- `check-update`: output Markdown for direct Bodhi copy-paste
- `check-update`: show repo class in report (e.g. "c10s (@epel)")
- `check-update`: fix stale side tag warning false positives
- `resolve`: verify requested packages exist on source before
  resolving (catches `--source-repo rawhide` misuse)
- Fix root README: Haskell → Hyperscale for hs-intake/hs-relmon

## v0.10.0

### ebranch

- `check-crate`: allow `-r` without `-b` for side tag repos
- `check-crate`: include dev deps in build-order edges (fixes
  incorrect phasing for packages with dev-only dependencies like
  arrow-row → arrow-cast)
- `check-crate`: add `--koji` and `--copr` output modes
- `check-crate`: include root crate as the final build phase
- `check-crate`: add `--refresh` flag
- `check-update`: add `--refresh` flag
- `resolve`: remove `--phases` flag (phases are always computed)
- `resolve`: auto-use `@koji-src:` for source RPM queries when
  `--source-repo @koji:<tag>` is given
- `resolve`: validate all configured repos on startup (catches
  nonexistent Koji repos early)
- `resolve`: reject bare `@koji:` repos as source with a clear
  error message

### poi-tracker

- **New: `sync-distgit` subcommand** — create or update an inventory
  from packages a user or group has access to on Fedora dist-git
  (Pagure). Merges new packages without overwriting existing entries.
  `--user` or `--group` mode with group-access filtering via
  `--no-groups`, `--include-group`, and `--exclude-group`
- Rename `domains` to `workloads` (matching content-resolver
  terminology)
- Workload membership is now declared at the workload level
  (`[inventory.workloads.<key>]` with a `packages` list) rather
  than inline on each package
- Per-workload metadata overrides (name, description, maintainer,
  labels) for content-resolver export
- Multi-workload export: omit `--workload` to produce one YAML
  per workload
- Rename `--domain` to `--workload` across all subcommands

### sandogasa-inventory

- Add `WorkloadMeta` struct with per-workload metadata and package
  list
- Replace `domains` with `workloads` (`BTreeMap<String, WorkloadMeta>`)
- Add `workloads_for_package()`, `add_to_workload()`,
  `workload_names()` methods
- Add JSON Schema generation via `schemars` (`json_schema()`)
- Check in schema at `data/inventory.schema.json` with snapshot test

### sandogasa-distgit

- Add `user_projects()` and `group_projects()` for listing RPM packages
  by user or group from the Pagure API
- Add `AccessGroups::contains_group()` helper

### sandogasa-pkg-acl

- Validate user/group existence before setting ACLs, replacing
  a generic 404 error with a clear message

### Workspace

- Relicense from MPL-2.0 to Apache-2.0 OR MIT

## v0.9.1

### New: sandogasa-inventory library crate

- TOML-based package-of-interest inventory data model
- Content-resolver YAML export (feedback-pipeline-workload format)
- hs-relmon manifest TOML export
- Import from legacy poi-tracker JSON format
- Domain-level defaults, private field stripping, multi-inventory merge

### New: poi-tracker tool

- Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs
- Commands: add, remove, show, validate, export, import
- Multi-inventory merge for exports
- Content-resolver export defaults to {name}.yaml filename

## v0.9.0

### New: sandogasa-koji library crate

- Shared Koji CLI wrappers: `list_tagged`, `list_tagged_nvrs`,
  `build_rpms`, `parse_nvr`, `parse_nvr_name`

### New: sandogasa-report tool

- Activity reporting for Fedora, EPEL, and CentOS SIG packaging work
- **Bugzilla**: review requests submitted/completed, reviews done for
  others, CVE/security, update requests, branch requests, FTBFS/FTI
  (classified via tracker bug aliases)
- **Bodhi**: updates submitted, pushed to testing, pushed to stable,
  with per-release breakdown sorted newest first
- **Koji CBS**: new packages and version updates detected by comparing
  tag snapshots at period start/end. Per-distro version merging,
  quarterly report style output
- Multi-domain support (`-d fedora -d hyperscale`)
- `--period` flag for years (2026), halves (2026H1), and quarters
  (2026Q1), plus `--since`/`--until` for arbitrary date ranges
- `--config` for project-level config (domains, groups, users)
- `--no-bugzilla`, `--no-bodhi`, `--no-koji` to skip data sources
- Brace expansion for Koji tag patterns
- Package groups with optional descriptions for categorical reporting
- User email resolution via FASJSON (rhbzemail) or config mapping

### ebranch

- **Breaking**: remove `build-order` subcommand; merged into
  `resolve --phases`
- `--exclude` flag for resolve: treat packages as already available
  on the target
- Rename `--no-auto-exclude` to `--no-auto-exclude-install`
- Fix side tag detection: use Bodhi's `from_tag` field (was
  incorrectly reading non-existent `from_side_tag`)

### sandogasa-bodhi (**breaking**)

- Rename `from_side_tag` to `from_tag` on `Update` struct (matching
  the actual Bodhi API field name)
- Add `date_testing` and `date_stable` fields to `Update`

### sandogasa-config

- Only enforce 600/700 permissions for user config files
  (`for_tool`), not project-level configs (`from_path`)

### sandogasa-cli

- New `require_tool_with_arg` for tools that use subcommands instead
  of `--version` (e.g. `koji version`)

## v0.8.1

### ebranch

- **New: `check-pkg-reviews` subcommand** — find and link Bugzilla
  package review requests based on the dependency graph from
  `check-crate --toml`. Caches bug IDs in the TOML file, batch-fetches
  bugs for speed, and prompts before applying changes
- **New: `config` subcommand** — interactive Bugzilla API key setup,
  stored securely at `~/.config/ebranch/config.toml`
- **New: `--toml` flag for `check-crate`** — save the full analysis
  (dependencies, edges, build phases) to a TOML file for reuse by
  `check-pkg-reviews` and other tools
- **New: `--dot` flag for `check-crate`** — output the dependency graph
  in Graphviz DOT format with version labels and build-phase grouping
- check-crate now resolves default Cargo features to find optional deps
  activated by default (e.g. `lexical-write-integer` via `lexical-core`)
- check-crate dev deps included by default (`--exclude-dev` to skip),
  matching Fedora's `%check`-enabled builds
- check-crate checks all RPM provider versions, finding compat packages
  (e.g. `rust-rand0.9`). Deps satisfied by compat packages are flagged
- check-crate resolves transitive dep versions matching the parent's
  semver requirement instead of always fetching the latest
- Rename `TooOld` to `Unmet` with full available-versions list
- Rename `--include-too-old` to `--include-unmet`
- Transitive deps now carry a `status` field (`missing` vs `unmet`)
  and a `package` field (RPM source package name)

### sandogasa-config

- Config files are now saved with 600 permissions and directories
  with 700, protecting API keys similar to SSH key files
- `load()` automatically fixes permissions on existing config files

### sandogasa-bugzilla

- New `bugs()` method for batch-fetching multiple bugs in one request

### hs-relmon

- Migrate config storage to `sandogasa-config`, gaining automatic
  secure file permissions for the GitLab access token

### Workspace

- Alphabetize subcommand sections in all tool READMEs to match
  `--help` output order

## v0.8.0

### New: sandogasa-cli library crate

- Shared `require_tool()` function for checking external tool
  availability at startup with clear install hints

### ebranch

- **New: `check-crate` command** — analyze a crates.io crate's
  dependencies against a target RPM repo
  - Shows missing, too-old, and satisfied dependencies with semver
    version matching
  - `--transitive` / `-t` expands missing deps recursively with
    phased build order (topological sort)
  - `--include-dev`, `--include-optional`, `--include-too-old` to
    widen transitive expansion
  - `--exclude CRATE,...` to skip crates (e.g. criterion) from
    transitive expansion
  - Partial version resolution: `57` resolves to highest `57.x.y`,
    `57.3` to highest `57.3.y`
  - Deduped crate counts when the same crate appears with different
    dependency kinds
- **`check-update` improvements**:
  - Prefer `@testing` repo (authoritative metadata) over side tag
  - Auto-detect testing branch from EPEL side tag names and Bodhi
    release metadata
  - Warn on stale side tag repos
  - Document EPEL 10 `@testing` limitation
- Parallelize fedrq queries with rayon (~4x speedup on 4 cores)
- Check for `fedrq` and `koji` availability at startup with clear
  error messages

### hs-relmon

- Reopen closed GitLab issues with matching title instead of creating
  duplicates

### sandogasa-bodhi (**breaking**)

- Add `from_side_tag` field to `Update` struct
- Add `branch` field to `Release` struct
- Add `update_by_alias()` for single-update API lookup

### Workspace

- External tool dependency checks: tools that shell out to fedrq or
  koji now verify availability at startup
- Move tool configs to top-level `configs/` directory
- Add source file ordering convention to CLAUDE.md
- Add dependency management guidelines to CLAUDE.md

## v0.7.0

### New: sandogasa-depfilter library crate

- Shared RPM dependency filtering for cross-branch analysis
- Classifies solib symbol version deps, soname deps, and RPM-internal
  deps (rpmlib, auto, config)

### ebranch

- Auto-exclude solib symbol version deps (e.g.
  `libc.so.6(GLIBC_2.38)(64bit)`) from installability checks — removes
  the need to manually `--exclude-install glibc` in most cases
- `--no-auto-exclude` flag to disable auto-exclusion
- Use shared dep filtering from sandogasa-depfilter

### koji-diff

- Fall back to build storage HTTP download when task logs have been
  garbage collected (requires build reference, not task reference)
- Retry with exponential backoff on transient server errors (502/503/504)
- **Breaking**: `BuildInfo` struct has new public fields (`name`,
  `version`, `release`)

### hs-intake

- Use shared solib detection from sandogasa-depfilter

### Workspace

- Fix all clippy warnings across workspace
- Add clippy cleanliness rule to CLAUDE.md

## v0.6.3

### New: koji-diff tool

- Compare buildroot and build logs between two Koji builds
- Accepts Koji build URLs, task URLs, or `build:<ID>`/`task:<ID>` refs
- Resolves builds to buildArch tasks via Koji XML-RPC API
- Downloads logs using `koji download-logs` with profile support
  (koji.fedoraproject.org, cbs.centos.org, kojihub.stream.centos.org)
- Parses installed packages from the DNF transaction table in root.log
  (supports both DNF4 and DNF5)
- Color-coded version change output using Rust semver rules:
  green (same version), yellow (compatible), orange (0.x minor break),
  red (major break)
- Shows mock_output.log for dependency resolution failures, build.log
  for rpmbuild failures
- `--json` flag for machine-readable output
- `--arch` to select architecture (default: x86_64)

### New: ebranch tool

- Build dependency resolver for cross-branch package porting
  (Rust rewrite of the Python ebranch tool)
- Compute build order for porting packages between branches
- `--koji` flag for chain build command output
- `--copr` flag for batch build script generation
- `--check-install` for subpackage installability verification

### New library crates

- **sandogasa-fedrq**: wrapper for the fedrq CLI tool (RPM repo queries)
- **sandogasa-rpmvercmp**: pure Rust implementation of RPM's rpmvercmp
  algorithm with epoch-version-release comparison
- **sandogasa-gitlab**: GitLab REST and GraphQL API client
- **sandogasa-repology**: Repology package version tracking API client

### Workspace

- Unify all tool versions to use `version.workspace = true`
- Integrate hs-intake and hs-relmon into the workspace, refactored to
  use shared library crates (sandogasa-fedrq, sandogasa-rpmvercmp,
  sandogasa-gitlab, sandogasa-repology)

## v0.6.2

### sandogasa-hattrack

- Display Discourse custom status (emoji + description) and expiration
  in the `last-seen` summary

## v0.6.1

### sandogasa-mailman

- Fix sender search to check all candidate email addresses per page
  instead of exhaustively scanning all pages for one address at a time

### sandogasa-hattrack

- Fix slow mailing list lookups for users who post from a non-primary
  email address

## v0.6.0

### New: sandogasa-hattrack tool

- Look up a Fedora contributor's activity across multiple services
- Subcommands: `discourse`, `bodhi`, `bugzilla`, `distgit`, `mailman`,
  `last-seen`
- `last-seen` summary shows the most recent activity from each service,
  sorted by date
- Discourse: profile info, timezone, location, custom status with
  rendered emoji, last post/seen timestamps
- Bodhi: last update submitted and last comment/karma
- Bugzilla: last bug filed and last bug changed
- Dist-git: daily activity stats (last 7 days), last PR filed,
  actionable PRs awaiting review
- Mailing lists: recent posts across all lists via HyperKitty API
- All timestamps include relative time ("3 days ago", "in 2 hours")
- `--json` flag for machine-readable output on all subcommands
- Email discovery via FASJSON (Kerberos) with `--email` override and
  `--no-fas` to skip authentication

### New: sandogasa-discourse crate

- Discourse forum API client for user profile data
- Fetch timezone, location, custom status, last post/seen timestamps

### New: sandogasa-fasjson crate

- FASJSON (Fedora Account System) API client via `curl --negotiate`
- Kerberos ticket management: status check, renewal, interactive
  acquisition with retry on timeout
- Read Fedora UPN from `~/.fedora.upn`

### New: sandogasa-mailman crate

- HyperKitty (Mailman 3) archive API client
- Find sender by email across list archives
- Fetch recent posts by sender across all lists

### sandogasa-bodhi

- Add `updates_for_user()` and `comments_for_user()` for user activity
  queries
- Add `Comment` and `CommentsResponse` models

### sandogasa-distgit

- Add `user_activity_stats()` for daily action counts
- Add `user_pull_requests()` for PRs filed by a user
- Add `user_actionable_pull_requests()` with pagination-aware total
  count
- Add `PullRequest`, `PullRequestsResponse`, and `Pagination` models

## v0.5.0

### fedora-cve-triage

- Add `cross-ecosystem` command to detect CVEs misattributed across
  ecosystems (e.g. JavaScript CVE filed against a Rust package with a
  similar name)
- Ecosystem detection from Fedora package names (`rust-*`, `nodejs-*`,
  `python-*`) with spec file fallback for ambiguous names
- Validate Bugzilla API key in `config` command via `valid_login` endpoint

### sandogasa-bugzilla

- Add `valid_login()` method for API key validation

### sandogasa-distgit

- Add `Ecosystem` enum and ecosystem detection functions
  (`is_js_package`, `is_rust_package`, `is_python_package`,
  `detect_ecosystem`) with quick name-based and full spec-based modes

### sandogasa-nvd

- Add NVD reference URL parsing (`CveReference`, `github_repos()`)
- Add `has_npm_references()` for detecting JavaScript packages via
  npmjs.com URLs
- Add npmjs.com reference check as 4th strategy in `targets_js()`
- GitHub repo language detection fallback for cross-ecosystem command

## v0.4.0

### New: sandogasa-pkg-acl tool

- View and manage Fedora package ACLs via the Pagure dist-git API
- Subcommands: `show`, `set`, `remove`, `apply`, `give`, `config`
- Batch ACL application from TOML config files across multiple packages
- `--strict` flag to downgrade access when target already has higher level
- Access checks: require admin for modifications, owner for transfers
- Owner protection: cannot downgrade or remove a package owner
- Username caching to avoid repeated token verification
- `--json` flag for machine-readable output on all subcommands

### New: sandogasa-config crate

- Shared config file management (`ConfigFile`) and interactive prompting
  (`prompt_field`) extracted from fedora-cve-triage for reuse across tools
- Email address validation helper

### sandogasa-distgit

- ACL management: `set_acl`, `remove_acl`, `get_acls`, `get_contributors`
- Ownership transfer: `give_package` via Pagure PATCH API
- User validation: `user_exists`
- Access level model with ordering, display, serde, and `FromStr`
- Access checking with direct and group membership support
- Token verification via `/api/0/-/whoami`

### Workspace

- Centralize all dependencies in `[workspace.dependencies]`
- Add `--json` requirement for non-interactive subcommands (CLAUDE.md)

## v0.3.1

- Fix --edit-bodhi to preserve existing bug references when adding new ones
- Convert to Cargo workspace with sandogasa library crates (bodhi, bugzilla, nvd, distgit)
- Move binary crate to tools/fedora-cve-triage for multi-tool workspace layout

## v0.3.0

- Add unshipped-tools command to detect CVEs for tools not shipped in RPMs
- Add Bugzilla email to config and prompt to reassign bugs when closing them
- Support filtering bodhi-check bugs by assignee (opt-in per-user triage)
- Add global -v/--verbose flag for progress on rate-limited API queries
- Fix bodhi-check false positives from mismatched NVD products:
  - Only compare versions when NVD product matches Fedora component
  - Use fedrq RPM provides to resolve name mismatches (e.g. django → python-django3)
  - Expand [epel-all] bugs to check all active EPEL releases

## v0.2.2

- Batch Bugzilla updates to close multiple bugs in a single API request
- Update project guidelines (code style rules, revised coverage threshold)

## v0.2.1

- Fall through to description heuristics when CPE has wildcard target_sw
- Hide API key input in config command

## v0.2.0

- Add bodhi-check subcommand to detect CVE bugs already fixed in Bodhi
- Add lag-tolerant tracker blocking for late-filed CVE bugs
- Add unit tests and enforce minimum coverage threshold

## v0.1.1

- Fix license text to MPL-2.0

## v0.1.0

- Initial release
- CLI with Bugzilla product/component/assignee/status filters
- js-fps subcommand to detect JavaScript/NodeJS false positives
- Three-strategy JS detection: CPE target_sw, CNA source, description keywords
- config command for Bugzilla API key setup
- Paginated Bugzilla search results
