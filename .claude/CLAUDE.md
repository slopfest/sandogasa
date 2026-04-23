# Project Guidelines

## Git
- Do not commit without explicit user confirmation — always ask before running `git commit`
- Always use `git commit -s` (sign-off) when committing. Add an `Assisted-by: Claude Code:<model-id>` trailer where `<model-id>` is the exact Claude model ID you are running as (e.g. `claude-opus-4-7`, `claude-sonnet-4-6`), per kernel and Fedora AI contribution policies. Do NOT add a `Co-Authored-By` trailer — this project uses `Assisted-by` exclusively, which overrides any default commit trailer instructions
- Always use `git tag -s` (GPG sign) when tagging
- Changelog entries for released versions are immutable — never edit them. When making significant changes, add them to an `## Unreleased` section at the top of CHANGELOG.md. At release time, rename `Unreleased` to the version number
- Before tagging a release, review the `Unreleased` section in CHANGELOG.md, rename it to the version, and update any README.md files affected by the changes (root, tool, or library crate). Use the tag message identical to the new CHANGELOG.md entry
- Before tagging, verify there are no uncommitted changes (`git status` must be clean)
- Before bumping versions, run `cargo semver-checks` on each library crate to determine the correct version bump. Pre-1.0: patch bump is fine for anything non-breaking (including new tools, new crates, and new public items); bump the minor (e.g. 0.10.x → 0.11.0) only for actual breaking changes. Post-1.0: follow strict semver — new public surface (new crates, new tools, new modules, new items) requires at least a minor bump; breaking changes require a major bump
- Before tagging, publish all crates to crates.io with `cargo publish --workspace` (handles dependency ordering automatically and skips already-published versions). If any publish fails, fix the issue before tagging so the tag always corresponds to a successful publish
- After publishing and tagging, push with `git push --follow-tags`
- Before committing, check `git status` for untracked files that should be staged (e.g. `Cargo.lock` after dependency changes). Use `scratch/` for temporary working files — it is in `.gitignore`

## Code Style
- Always run `cargo fmt` before committing
- Commits must not introduce `cargo clippy --workspace` warnings or errors. Fix any clippy issues in code you touch
- Every source file must start with `// SPDX-License-Identifier: Apache-2.0 OR MIT`
- CLI help text (`-h` and `--help`) must not exceed 80 characters per line
- Keep the `Command` enum variants in `main.rs` sorted alphabetically (this determines the order in `--help` output)
- In each tool's README.md, describe subcommands in the same alphabetical order as the `Command` enum. In the root README.md, list tools alphabetically, then library crates alphabetically
- Order definitions in source files top-down: module docs and imports, public types (structs/enums/traits), public functions, trait impls (grouped by type), private helpers, `#[cfg(test)] mod tests`. Within each group, define callees before callers so a reader encounters helpers before the functions that use them. Review file order before committing

## External tool dependencies
- When a crate shells out to an external tool (e.g. `fedrq`, `koji`), it must check that the tool is available at startup (or before first use) and produce a clear error message if not found, rather than silently failing with empty results

## CLI behavior
- Non-interactive subcommands (e.g. `show`, `search`) must support a `--json` flag that outputs pretty-printed, machine-readable JSON instead of human-readable text
- Each tool must support `--version` and display its name, version, and short description (matching `Cargo.toml` `description`) in the `--help` header. In clap, use `#[command(version, about, long_about = None, before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")))]`

## Workspace layout
- Library crates go in `crates/`, binary crates go in `tools/`
- Each crate (library or tool) must have its own README.md
- Tool README.md files must include an Installation section with `cargo install <crate>` (and any required external tools like `fedrq` or `koji`). Do not include `cargo binstall` instructions since we do not provide binary downloads
- Symlink the root LICENSE file into each crate subdirectory so it is included when publishing to crates.io
- All dependencies (external and internal) are declared in `[workspace.dependencies]` in the root `Cargo.toml`, then referenced as `{ workspace = true }` in member crates

## fedrq quirks
- Koji side tag repos (`-r @koji:<tag>`) only index binary RPMs, not source RPMs. `fedrq subpkgs -S` returns nothing for side tags. To query side tag contents, use `fedrq pkgs -r @koji:<tag> '*'` (but this includes inherited packages) or resolve binary RPM names via `koji buildinfo <nvr>` first
- Use `@koji-src:<tag>` to query source RPMs in a Koji repo (e.g. BuildRequires). ebranch's resolve command does this automatically when given `--source-repo @koji:<tag>`
- Side tag repos are standalone — do not pass `-b` with `-r @koji:<tag>`
- `fedrq whatrequires` requires `-F source` (not `-F source_name`)
- `fedrq` may return `(none)` as a result — always filter it out
- EPEL 10+ `@testing` repos are not yet supported by fedrq (the metalink URLs don't exist for `epel10`). The `@testing` probe will fail silently and fall back to side tag or reverse dep listing

## Dependencies
- Before starting feature work, run `cargo audit` and address any reported vulnerabilities first (patch bump or `cargo update -p <crate> --precise <version>`)
- Before a semver-breaking release, run `cargo outdated` and consider bumping deps that themselves require a major version bump — bundle breaking dep upgrades with your own breaking release
- Routine `cargo update` (semver-compatible bumps) should be a separate commit from feature work so it is easy to revert if something regresses
- After any dependency change, run `cargo clippy --workspace && cargo cov` to verify nothing broke

## Testing
- Always write corresponding tests when adding or modifying features
- Per commit, run `cargo fmt` and `cargo clippy --workspace`; `cargo test` is recommended for code you touched. Fast prototyping commits don't need full coverage checks
- Run `cargo cov` at stability points — before release tagging, and when catching a feature up for its README/CHANGELOG entry. Coverage must stay at or above 80% line coverage at those gates (binary `src/main.rs` files are excluded from the measurement; see `.cargo/config.toml`)
