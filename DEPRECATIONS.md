# Deprecations

Functionality that still works but is scheduled for removal. Each
entry records the release that deprecated it, the planned removal
release, and the replacement. Deprecated functionality emits a
runtime warning on use whenever feasible; removals land in the
stated release and are listed in its CHANGELOG entry as breaking
changes.

No active deprecations.

## Completed removals

- `poi-tracker sync-distgit --auto-prefix --pattern <start>`
  (deprecated in v0.12.1) — removed in v0.13.0; use
  `--start-pattern <prefix>` (optionally with
  `--end-pattern <prefix>`).
