# cpu-sig-tracker: TODO

Updated as work progresses. Items are deleted or checked off when
done; in-progress items get an `(in progress)` marker.

## MVP (v0.1)

### Dependencies / building blocks
- [ ] Add `untag_build(tag, nvr, profile)` to `sandogasa-koji`
- [x] Add MR detail fetch to `sandogasa-gitlab` (description,
      source/target branch, `parse_mr_url` helper)
- [x] New `sandogasa-jira` library crate:
      - [x] Minimal `JiraClient::new(base_url)` with optional
            token auth
      - [x] `issue(key) -> Option<Issue>` — status, resolution,
            summary; 404 → None
      - [x] Mock-based tests (wiremock pattern)

### Tool crate
- [x] Create `tools/cpu-sig-tracker/` skeleton (Cargo.toml,
      src/main.rs, LICENSE symlinks, README.md)
- [x] Add to workspace members in root `Cargo.toml`

### Subcommands
- [x] `dump-inventory --release c10s -o inv.toml` — list Koji-tagged
      packages, emit sandogasa-inventory TOML
- [x] `file-issue <mr-url> [--affected VER] [--expected-fix VER]`
      — standardized issue body, auto-extract JIRA link from MR
- [ ] `status -i inv.toml [--release c10s]` — per-package report
      (JIRA state, Stream NVR comparison, suggested action)
- [ ] `sync-issues -i inv.toml [--file-missing]` — ensure each
      inventory package has a tracking issue; create missing ones
- [ ] `untag <nvr> [--release c10s] [--yes]` — verify JIRA closed,
      prompt, untag

### Helpers
- [ ] Issue body formatter / parser (`issue_body.rs`)
- [ ] NVR bumping (release increment) — probably reuse
      `sandogasa-rpmvercmp`'s parsing; may need a small helper

### Output
- [ ] Human-readable `status` output (per-package, colored
      suggestion)
- [ ] `--json` flag for machine-readable output

### Config
- [ ] Reuse `sandogasa-config` for JIRA token storage (optional,
      anonymous works for public issues)

### Tests
- [ ] Unit tests for issue body format round-trip
- [ ] Mock tests for `sandogasa-jira`
- [ ] Integration: `dump-inventory` → `sync-issues --file-missing`
      → `status` on a canned inventory

### Docs
- [ ] Tool `README.md` (install, all five subcommands, workflow
      example)
- [ ] Root `README.md` entry (alphabetical)
- [ ] `CHANGELOG.md` Unreleased entry

## Post-MVP

### Features
- [ ] `rebase <pkg>` — drive the dist-git / MR workflow to rebuild
      against the latest Stream
- [ ] Multi-release scanning in one invocation
- [ ] Snapshot diff: compare two status outputs to show what
      changed
- [ ] CI-friendly `--strict` mode that fails on missing tracking
      issues

### Open questions to resolve
- [ ] JIRA token storage + auth flow details
- [ ] Per-release vs multi-release inventory format
- [ ] Rebase heuristic when Stream NVR is lower than
      proposed_updates NVR
