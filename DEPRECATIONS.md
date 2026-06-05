# Deprecations

Functionality that still works but is scheduled for removal. Each
entry records the release that deprecated it, the planned removal
release, and the replacement. Deprecated functionality emits a
runtime warning on use whenever feasible; removals land in the
stated release and are listed in its CHANGELOG entry as breaking
changes.

## poi-tracker: `sync-distgit --auto-prefix --pattern <start>`

- **Deprecated in:** v0.12.1
- **Removal:** v0.13.0
- **Replacement:** `--start-pattern <prefix>` (optionally with
  `--end-pattern <prefix>`)

The pre-0.12.1 spelling for resuming a prefix scan reinterpreted
`--pattern` as the scan start point when `--auto-prefix` was also
given. `--pattern` now always means a single patterned query, and
`--start-pattern` / `--end-pattern` bound the prefix scan instead.
The old combination still works and prints a deprecation warning;
from v0.13.0 it will be rejected (`--pattern` will conflict with
`--auto-prefix`).
