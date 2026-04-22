# sandogasa-pkg-health: TODO

Updated as work progresses. Items are deleted when done (git history
tracks when); in-progress items get an `(in progress)` marker.

## MVP (v0.1)

### Core framework
- [x] Create crate skeleton (`Cargo.toml`, `src/main.rs`, `src/lib.rs`)
- [x] Add to workspace in root `Cargo.toml`
- [x] `HealthCheck` trait with `id`, `description`, `cost_tier`, `run`
- [x] `CostTier` enum: `Cheap`, `Medium`, `Expensive`
- [x] `Context` struct bundling clients (bugzilla, distgit, per-version
      trackers, tokio runtime handle)
- [x] Check registry (Vec<Box<dyn HealthCheck>>)
- [x] `CheckResult` with arbitrary serde value
- [x] Variant-aware checks via `HealthCheck::variants()` — each
      (package, check, variant) has its own stored entry and timestamp

### Report persistence
- [x] TOML data model matching PLAN.md
- [x] Load-or-create logic (read existing report, merge updates)
- [x] Timestamp-based staleness check (`--max-age` flag)
- [ ] JSON Schema generation via `schemars` (matching inventory pattern)

### CLI
- [x] clap args: `-i inventory`, `-o report`, `--check <id>`,
      `--cheap`/`--medium`/`--expensive`/`--all`, `--max-age`,
      `--package`, `--json`, `--verbose`
- [x] `--fedora-version` and `--epel-version` (CSV + repeatable,
      sorted and deduped with duplicate warnings)
- [x] Check selection logic (tier flags, --check, default=cheap)
- [x] Package selection (`--package`, repeatable; default=all)
- [x] `--max-age` parsing and skip-when-fresh logic
- [ ] Per-package parallelism (rayon) for cheap checks
- [ ] Human-readable summary output (currently just a count)
- [x] JSON output (dumps full report)

### First checks
- [x] `maintainer_count` (Cheap) — dist-git ACL lookup + Pagure group
      expansion for effective committer count
- [x] `bug_count` (Medium) — Bugzilla search per component, classify
      via `sandogasa-bugclass`; variant-aware per release

### Tests
- [x] Unit tests for report merge/update logic (7 tests)
- [x] Unit tests for duration parser (11 tests)
- [ ] Mock-based tests for each check (Bugzilla query, dist-git ACLs)
- [ ] Snapshot test for JSON Schema (matching inventory pattern)

### Docs
- [x] `README.md` following project conventions (install, usage)
- [ ] Root `README.md`: add tool entry (alphabetical)
- [ ] `CHANGELOG.md`: `Unreleased` entry

## Post-MVP

### Additional checks
- [ ] `cve_severity` (Medium) — NVD lookup for bug-associated CVEs
- [ ] `build_status` (Medium) — latest Koji build result, FTBFS flag
- [ ] `dependency_health` (Expensive) — rollup from BuildRequires
      closure
- [ ] `activity` (Cheap) — commits to dist-git in last N days

### Features
- [ ] `show <package>` subcommand for single-package detail
- [ ] Configurable thresholds → exit-code gating
- [ ] Config file support (per-inventory check selection)
- [ ] Comparison between two reports (diff: what got worse/better)

### Open questions to resolve
- [x] ~~Where do bug classifiers live?~~ — extracted to
      `sandogasa-bugclass` library crate
- [ ] Should reports be per-inventory or per-package-set?
