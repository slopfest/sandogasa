# Changelog

## v0.1.3 — 2026-03-06

### Features

- Add `format_result` functions to `compare` and `safe_to_backport` for programmatic report generation without printing to stdout.

### Housekeeping

- Use SPDX license headers (`// SPDX-License-Identifier: MPL-2.0`) in all source files.

## v0.1.2 — 2026-03-06

### Features

- Add library target so `hs-intake` can be used as a dependency from other crates.

## v0.1.1 — 2026-03-06

### Features

- **safe-to-backport**: Add `--also-check` flag to check reverse dependencies on additional branches (comma-separated).

### Bug Fixes

- Restrict solib version extraction to actual `.so` entries. Parenthesized entries like `pkgconfig(dracut)` are no longer incorrectly split into a name and version.

## v0.1.0 — 2026-03-06

Initial release.

### Features

- **compare-provides**: Compare the Provides of a source package between two branches, with upgrade/downgrade detection using RPM version comparison.
- **compare-requires**: Compare the Requires of a source package between two branches, with support for `=`, `>=`, and solib-style version matching.
- **compare-build-requires**: Compare the BuildRequires of a source package between two branches.
- **safe-to-backport**: Evaluate whether a package is safe to backport, combining all three comparisons with reverse dependency analysis. Exits non-zero when concerns are found.
- `--json` flag on all commands for machine-readable output.
- `--show-unchanged` flag on compare commands to include unchanged entries in the output.
- Self-dependency filtering: Requires on the source package's own subpackages are automatically excluded from comparisons.
- ASCII table output for upgraded/downgraded entries.
- Detailed reverse dependency analysis showing which Requires are affected and what each branch provides.
