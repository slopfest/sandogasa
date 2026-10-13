# Deprecations

Functionality that still works but is scheduled for removal. Each
entry records the release that deprecated it, the planned removal
release, and the replacement. Deprecated functionality emits a
runtime warning on use whenever feasible; removals land in the
stated release and are listed in its CHANGELOG entry as breaking
changes.

No active deprecations.

## Completed removals

- `ebranch resolve --no-auto-exclude-install` (deprecated in v0.25.2)
  — removed in v0.26.0; there is no replacement and none is needed.
  It re-enabled library dependencies
  (`libc.so.6(GLIBC_2.38)(64bit)`) in installability checks, and
  those are no longer checked at all: they record what the source
  branch's build linked against, not what a rebuild on the target
  needs. Drop the flag.

- `poi-tracker sync-distgit --auto-prefix --pattern <start>`
  (deprecated in v0.12.1) — removed in v0.13.0; use
  `--start-pattern <prefix>` (optionally with
  `--end-pattern <prefix>`).
