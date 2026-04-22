# sandogasa-pkg-health: Project Plan

## Purpose

Audit the health of packages in a sandogasa inventory. Each package
gets a report of health metrics — open bugs by category, maintainer
coverage, build status, CVE exposure, etc. — and eventually an
aggregated score that bubbles up from a package's dependencies.

The tool is designed around two constraints:

1. **Checks have very different costs.** Counting open bugs is one
   Bugzilla query per package. Walking the transitive dependency
   graph for health rollup is potentially thousands of queries.
2. **Reports should be incrementally updatable.** Cheap checks run
   often (daily); expensive ones run rarely (weekly/monthly).
   Re-running one check preserves existing results for the others.

## Architecture

### Pluggable checks

Each health check implements a trait:

```rust
trait HealthCheck {
    /// Stable identifier used in CLI flags and stored results.
    fn id(&self) -> &'static str;
    /// Human-readable description.
    fn description(&self) -> &'static str;
    /// Cost tier — controls whether --cheap/--medium/--expensive
    /// runs this check.
    fn cost_tier(&self) -> CostTier;
    /// Run the check for a single package.
    async fn run(&self, package: &str, ctx: &Context) -> Result<CheckResult>;
}

enum CostTier {
    Cheap,     // local data or one cheap query
    Medium,    // one API call per package
    Expensive, // transitive / multi-query
}
```

Checks are registered in a central registry. CLI flags select which
ones to run.

### Report data model

Reports persist to TOML (via `sandogasa-inventory`-style format,
since we already have JSON Schema tooling there).

```toml
[report]
inventory = "hyperscale-packages"
generated = "2026-04-22T10:00:00Z"

[package.rust-arrow]
# Per-check results keyed by check id.
[package.rust-arrow.bug_count]
timestamp = "2026-04-22T10:00:00Z"
data = { open = 5, cve = 2, ftbfs = 1, update = 2 }

[package.rust-arrow.maintainers]
timestamp = "2026-04-20T09:00:00Z"
data = { direct = ["alice"], groups = ["rust-sig"], effective_count = 3 }
```

The timestamp on each check lets us age-out and selectively re-run.

### Selective updates

CLI:

```
sandogasa-pkg-health run -i inventory.toml -o health.toml \
    [--check bug_count] [--check maintainers] \
    [--cheap | --medium | --expensive | --all] \
    [--max-age 7d] \
    [--package rust-arrow]
```

Semantics:

- No `-o` file or file doesn't exist → fresh report.
- `-o` file exists → load it, run selected checks, update those
  entries, preserve all others.
- `--check <id>` (repeatable) → run only those checks.
- `--cheap`/`--medium`/`--expensive` → run all checks in that tier.
- `--all` → run all checks regardless of tier.
- `--max-age 7d` → rerun any check whose timestamp is older than 7
  days ago (in addition to explicitly selected checks).
- `--package <name>` (repeatable) → limit to specific packages.

### Output

- TOML report file (persistent, incrementally updatable).
- Human-readable summary to stdout: package, issues found, age.
- `--json` for machine-readable.
- Exit non-zero if any package fails a threshold (configurable
  later — out of MVP scope).

### Aggregation / bubble-up

Health scores bubble up from dependencies. A package with zero
direct issues but all its deps are unhealthy gets flagged as
"at risk".

- Needs a graph model: package → BuildRequires → dependencies.
- Can reuse `ebranch` resolution machinery (`sandogasa-fedrq`,
  `resolve_closure_with_options`).
- Computed as a separate check (expensive tier) that reads
  per-package results and computes a rollup.

## MVP scope (v0.1 of the tool)

1. Core framework: check trait, registry, Context (with fedrq +
   bugzilla clients), report TOML model.
2. Cheap checks:
   - `maintainer_count` (count of direct/group maintainers via FASJSON).
3. Medium checks:
   - `bug_count` (open bugs by category via Bugzilla; reuse
     `sandogasa-report` classifiers).
4. CLI with `run` subcommand and selective update semantics.
5. TOML report persistence.
6. Human-readable summary output; `--json` flag.

## Post-MVP

- CVE severity aggregation (medium tier — NVD per CVE).
- FTBFS/FTI detection (medium tier — Koji build log scraping).
- Dependency health bubble-up (expensive tier).
- Configurable health thresholds → exit-code gating.
- `show` subcommand for inspecting single-package reports.

## Files to modify

- New crate: `tools/sandogasa-pkg-health/`
  - `Cargo.toml`, `src/main.rs`, `src/check.rs`,
    `src/report.rs`, `src/checks/` (one file per check).
- Root `Cargo.toml`: add to workspace members.
- Root `README.md`: add tool entry.
- `CHANGELOG.md`: `Unreleased` entry.
- Reuse: `sandogasa-inventory` (inventory loading),
  `sandogasa-bugzilla`, `sandogasa-fasjson`, `sandogasa-report`
  (bug classifiers — may need to extract shared types).

## Open questions

1. Should we reuse the JSON Schema pattern from sandogasa-inventory
   for the report format? Would help external consumers validate.
2. Do we want a config file for per-inventory check selections /
   thresholds, or is CLI-only fine for MVP?
3. Bug classification lives in `sandogasa-report` currently — should
   it move to a shared crate (e.g. `sandogasa-bugclass`) or should
   `pkg-health` depend directly on `sandogasa-report`? Depending
   on a binary crate from a library is unusual.
