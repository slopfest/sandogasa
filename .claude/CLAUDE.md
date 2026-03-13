# Project Guidelines

## Git
- Always use `git commit -s` (sign-off) when committing
- Always use `git tag -s` (GPG sign) when tagging
- Before tagging a release, update CHANGELOG.md and any README.md files affected by the changes (root, tool, or library crate). Use the tag message identical to the new CHANGELOG.md entry
- Before tagging, verify there are no uncommitted changes (`git status` must be clean)
- Before tagging, publish all crates to crates.io in dependency order — library crates in `crates/` first, then binary crates in `tools/`. Use `cargo publish -p <crate>` for each. If any publish fails, fix the issue before tagging so the tag always corresponds to a successful publish
- After publishing and tagging, push with `git push --follow-tags`

## Code Style
- Always run `cargo fmt` before committing
- Every source file must start with `// SPDX-License-Identifier: MPL-2.0`
- CLI help text (`-h` and `--help`) must not exceed 80 characters per line
- Keep the `Command` enum variants in `main.rs` sorted alphabetically (this determines the order in `--help` output)

## Workspace layout
- Library crates go in `crates/`, binary crates go in `tools/`
- Each crate (library or tool) must have its own README.md
- Symlink the root LICENSE file into each crate subdirectory so it is included when publishing to crates.io

## Testing
- Always write corresponding tests when adding or modifying features
- Run `cargo cov` before committing to verify tests pass and coverage does not regress
- Coverage must stay at or above 72% line coverage
