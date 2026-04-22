# sandogasa-pkg-health: TODO

Updated as work progresses. Items are deleted when done (git history
tracks when); in-progress items get an `(in progress)` marker.

## MVP (v0.1)

### Core framework
- [ ] Create crate skeleton (`Cargo.toml`, `src/main.rs`,
      `src/lib.rs` if needed)
- [ ] Add to workspace in root `Cargo.toml`
- [ ] `HealthCheck` trait with `id`, `description`, `cost_tier`, `run`
- [ ] `CostTier` enum: `Cheap`, `Medium`, `Expensive`
- [ ] `Context` struct bundling clients (fedrq, bugzilla, fasjson)
- [ ] Check registry (Vec<Box<dyn HealthCheck>>)
- [ ] `CheckResult` with timestamp + arbitrary serde value

### Report persistence
- [ ] TOML data model matching PLAN.md
- [ ] Load-or-create logic (read existing report, merge updates)
- [ ] Timestamp-based staleness check (`--max-age` flag)
- [ ] JSON Schema generation via `schemars` (matching inventory pattern)

### CLI
- [x] clap args: `-i inventory`, `-o report`, `--check <id>`,
      `--cheap`/`--medium`/`--expensive`/`--all`, `--max-age`,
      `--package`, `--json`, `--verbose`
- [x] Check selection logic (tier flags, --check, default=cheap)
- [x] Package selection (`--package`, repeatable; default=all)
- [ ] `--max-age` parsing and skip-when-fresh logic
- [ ] Per-package parallelism (rayon) for cheap checks
- [ ] Human-readable summary output (currently just a count)
- [x] JSON output (dumps full report)

### First checks
- [ ] `maintainer_count` (Cheap) — FASJSON lookup for direct users
      + group expansion
- [ ] `bug_count` (Medium) — Bugzilla search for component, classify
      by reuse of `sandogasa-report` classifiers

### Tests
- [ ] Unit tests for report merge/update logic
- [ ] Mock-based tests for each check
- [ ] Snapshot test for JSON Schema (matching inventory pattern)

### Docs
- [ ] `README.md` following project conventions (install, usage,
      fields)
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
- [ ] Where do bug classifiers live? (stay in sandogasa-report,
      extract to sandogasa-bugclass, or duplicate?)
- [ ] Should reports be per-inventory or per-package-set?
