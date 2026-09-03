# ebranch — development notes

Design decisions and rules future work must follow. The README
describes the tool as it is; this file says why some of it is that way.

## check-crate follows every real dev dependency; benchmarks are excluded, not capped

`check-crate --transitive` expands the dependencies of every crate that
would have to be packaged, including their dev dependencies, and their
dev dependencies' dependencies in turn. That is intentional: a dev
dependency is what the crate's tests need, Fedora runs those tests in
`%check`, so a real dev dependency is something to package.

It also means one benchmarking harness can dominate a report. The
uutils-coreutils check once listed 483 transitive-missing crates, 473
of them behind `codspeed-criterion-compat` — parse_datetime's dev
dependency — through `smol`, `surf`, `plotters` and their own dev
dependencies. The fix for that is the `[check-crate] exclude` list in
the config file (criterion, `codspeed-*`, divan, iai,
count_instructions, …): Fedora drops benchmark machinery from the
build, so check-crate should not count it at any level.

Do not "fix" this with a depth or kind rule — dev dependencies only for
the root, or only one level down. When a report balloons, find the
entry point (the `transitive_edges` in the saved TOML give the paths)
and suggest an exclude entry for it.

The common harnesses are built in (`config::DEFAULT_EXCLUDES`) so a
fresh install gets a sane report. A configured list *replaces* the
built-in one rather than merging with it — the same "user wins per
key" rule the /etc-beneath-~/.config layering follows — because the
one person who wants to package criterion must be able to un-exclude
it, and a merge would leave them no way to. Adding to the set is done
inside the same list, with the `"@default"` entry standing for it —
TOML has no `+=`. So: keep the built-in list to crates Fedora never
packages as dependencies, and never add a second, merged list on top
of it.
