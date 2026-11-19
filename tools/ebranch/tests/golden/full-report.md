# Checking update: epel10.4-build-side-152994

**Branch:** c10s (@epel)

## Packages

**2** package(s): 1 updated, 1 new

- `1.2-3.el10` → `2.0-1.el10` (libfoo)

**New (1):** widget

> **Warning:** side tag repodata is stale for 1 source package(s); the reverse-dep analysis below may be incomplete. Run `koji regen-repo` on the side tag, then rerun this check with `--refresh` to drop the cached metadata.
> - **widget**: expected `widget-1.0-1.el10`, found `0.9-1.el10` in repodata

## Analysis

- **Changed Provides:** 2 (1 updated, 1 removed)
- **Installability issues:** 1
- **Reverse dependencies:** 1 checked, 1 would break (1 FTI, 0 FTBFS)

## Removed Provides (1)

- `libfoo-legacy.so.0`

## Installability issues (1)

- **plasma-settings**: `libfoo.so.1`

## Reverse dependencies that would break (1)

- **kpat**
  - [FTI] `needs` (changed: `libfoo.so.1`)

