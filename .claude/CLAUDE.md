# Project Guidelines

## Git
- Always use `git commit -s` (sign-off) when committing
- Always use `git tag -s` (GPG sign) when tagging
- Before tagging a release, update CHANGELOG.md and any README.md files affected by the changes (root, tool, or library crate). Use the tag message identical to the new CHANGELOG.md entry
- Before tagging, verify there are no uncommitted changes (`git status` must be clean)
- Before bumping versions, run `cargo semver-checks` on each library crate to determine the correct version bump (patch, minor, or major). If semver-checks reports breaking changes, the bump must be at least minor (or major if already ≥1.0). If it reports no breaking changes, a patch bump is sufficient unless new public API surface was added (which requires at least minor)
- Before tagging, publish all crates to crates.io with `cargo publish --workspace` (handles dependency ordering automatically and skips already-published versions). If any publish fails, fix the issue before tagging so the tag always corresponds to a successful publish
- After publishing and tagging, push with `git push --follow-tags`

## Code Style
- Always run `cargo fmt` before committing
- Commits must not introduce `cargo clippy --workspace` warnings or errors. Fix any clippy issues in code you touch
- Every source file must start with `// SPDX-License-Identifier: MPL-2.0`
- CLI help text (`-h` and `--help`) must not exceed 80 characters per line
- Keep the `Command` enum variants in `main.rs` sorted alphabetically (this determines the order in `--help` output)

## CLI behavior
- Non-interactive subcommands (e.g. `show`, `search`) must support a `--json` flag that outputs pretty-printed, machine-readable JSON instead of human-readable text
- Each tool must support `--version` and display its name, version, and short description (matching `Cargo.toml` `description`) in the `--help` header. In clap, use `#[command(version, about, long_about = None, before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")))]`

## Workspace layout
- Library crates go in `crates/`, binary crates go in `tools/`
- Each crate (library or tool) must have its own README.md
- Tool README.md files must include an Installation section with `cargo install <crate>` (and any required external tools like `fedrq` or `koji`). Do not include `cargo binstall` instructions since we do not provide binary downloads
- Symlink the root LICENSE file into each crate subdirectory so it is included when publishing to crates.io
- All dependencies (external and internal) are declared in `[workspace.dependencies]` in the root `Cargo.toml`, then referenced as `{ workspace = true }` in member crates

## Testing
- Always write corresponding tests when adding or modifying features
- Run `cargo cov` before committing to verify tests pass and coverage does not regress
- Coverage must stay at or above 75% line coverage
