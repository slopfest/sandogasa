# ebranch — development notes

Design decisions and rules future work must follow. The README
describes the tool as it is; this file says why some of it is that way.

## check-update's local-repo fallback stands alone, like a side-tag repo

A pending Bodhi update — or one pushed to testing that the mirrors do
not carry yet — used to get the reduced report ("Provides can't be
compared"), because neither `@testing` nor a side tag nor a COPR had
its builds. Its builds are in koji, though, so `check-update` downloads
them and indexes a local repository as the "new" side. Rules that fall
out of how the pieces behave, each checked on a live update
(FEDORA-2026-a67d0ddb5f, uxplay, 2026-09-18):

- **`-r @baseurl:file://<dir>` replaces the branch's repositories**, it
  does not add to them: `fedrq pkgs -b f45 -r @baseurl:… -F nevr uxplay`
  returned only the downloaded 1.73.7, while the same query without `-r`
  returned the stable 1.73.3. So the local repo is queried with no
  branch, exactly like `@koji:<side-tag>`, and the Provides diff cannot
  be polluted by the old version. Had it merged, every old Provide would
  still have been "present" and no update would ever have been detected.
- **Download with `bodhi updates download --updateid <alias>` and no
  `--arch`.** bodhi's client then passes `--arch=noarch --arch=<host
  machine>` to `koji download-build`; an explicit `--arch x86_64` passes
  only that and silently drops noarch subpackages. It also keys the
  download by the alias `check-update` already holds, so there is no
  per-NVR loop to get wrong.
- **Reuse the side-tag arm's comparison, skip its staleness check.**
  `compute_changed_provides_via_koji` takes binary names from `koji
  buildinfo` and Provides from whatever repo it is handed; a repo built
  from koji's own RPMs cannot lag koji, so the regen-repo machinery does
  not apply.
- **Cache under `$XDG_CACHE_HOME/ebranch/update-repos/<alias>/`** (the
  `dirs` crate, per the workspace rule), with the sorted NVR list beside
  the repodata: same list, reuse; different list, remove and rebuild so
  no RPM from a superseded build lingers. The alias is validated to the
  characters a Bodhi alias has before it becomes a path component.
- **Both tools are preconditions**, checked with `require_tools` before
  anything is downloaded; a missing tool or a failed download prints why
  and falls through to the reverse-deps-only report rather than aborting
  the run.

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
