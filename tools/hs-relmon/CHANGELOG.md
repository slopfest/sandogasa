<!-- SPDX-License-Identifier: MPL-2.0 -->

# Changelog

## 0.3.0 - 2026-03-09

- Add `list-issues` command to list all GitLab issues labeled `rfe::new-version`
  under a group, with `--issue-status` and `--issue-assignee` filters
- Add `--manifest` flag to `list-issues` to show which packages with update
  issues are missing from the manifest
- Add `--add-missing` flag to `list-issues` to insert missing packages into the
  manifest file, keeping entries sorted
- Support `--issue-assignee "none"` to filter for unassigned issues in both
  `check-manifest` and `list-issues`
- Preserve comments and formatting when modifying manifest files via
  `--add-missing` (using `toml_edit`)
- Add GitLab CI with coverage tests (80% threshold) and secret detection

## 0.2.1 - 2026-03-09

- Add `check-manifest` command to check all packages listed in a TOML manifest
  file, with `[defaults]` and per-package overrides for distros, track,
  repology name, and issue filing
- Add issue status and assignee fields to JSON output, resolved via GitLab
  GraphQL work-item STATUS widget
- Add `--issue-status` and `--issue-assignee` filters to `check-manifest` to
  narrow the report by issue status (e.g. "To do") or assignee username
- Replace version comparison with RPM `rpmvercmp` algorithm, correctly handling
  `~` (pre-release) and `^` (post-release snapshot) markers

## 0.2.0 - 2026-03-09

- Add `--file-issue` flag to `check-latest` to automatically create or update
  a GitLab issue (labeled `rfe::new-version`) when a package is outdated
- Add `config` command to set up GitLab authentication with token validation
  and secure input (token stored in `~/.config/hs-relmon/config.toml`)
- GitLab client falls back to config file token when `GITLAB_TOKEN` is unset

## 0.1.1 - 2026-03-09

- Add `--track` option to compare Hyperscale builds against a reference
  distribution (defaults to upstream)
- Add `--repology-name` option to override the Repology project name when
  it differs from the package name (e.g. `linux` for `perf`)
- Fix Repology entry selection for projects with multiple source packages,
  using status priority and numeric version comparison
- Fix table column alignment and help text formatting
- Fix version comparison to use numeric ordering instead of string equality

## 0.1.0 - 2026-03-09

Initial release.

- Add `check-latest` command to query the latest version of a package
  across upstream (Repology), Fedora (Rawhide and stable), CentOS Stream,
  and Hyperscale (EL9 and EL10)
- Hyperscale builds report release and testing status separately via CBS
  Koji tag lookup
- Support filtering which distributions to check with `--distros`
- Table output by default, machine-readable JSON with `--json`
