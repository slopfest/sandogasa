# Changelog

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
