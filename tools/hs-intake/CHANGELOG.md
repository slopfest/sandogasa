# Changelog

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
