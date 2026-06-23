# Changelog

## Unreleased

### fedora-review-digest: new tool

Condense a `fedora-review` run of an auto-generated spec into a short
rust-sig-style Bugzilla review comment, dropping the template noise that
isn't decision-relevant for a generated package. Reads a finished
`fedora-review -b` result directory (or a bug id resolved to `<id>-*`)
and emits the three `===`-separated blocks: an optional reviewer note, a
checklist with a per-item ✅/🫤/❌ verdict and the MUST issues that need
attention, and the post-import rust-sig task boilerplate.

Marks are inferred and then confirmed interactively (`+1/0/-1` per item,
evidence shown inline, `-y` to accept). It computes what `fedora-review`
doesn't decide for a generated spec — crates.io latest version, the
spec↔`Cargo.toml` license cross-check — and applies rust2rpm-aware
handling: suppress the benign "license file listed twice", note a
manually-added license (`included manually[, fix submitted to
upstream]`), distinguish skipped vs disabled tests, and add a
static-linked-deps check for crates that ship a binary. rust2rpm only
for now; pyp2spec and running fedora-review itself are planned. Needs
`fedora-review` (to produce the dir) and `curl` (crates.io check;
`--no-net` to skip).

### dbranch: pre-flight PPA uploads against Launchpad

Before a PPA upload (`--ppa`/`ppa:` target), dbranch now checks via the
Launchpad API (`curl … getPublishedSources`) whether the package is
already published in that PPA. If it isn't — or the PPA can't be
verified (a typo'd name 404s) — it asks to confirm before uploading
(default **no**), catching an accidental wrong-PPA upload. A genuine
first upload hits this once and is confirmed, like trusting a new SSH
host. The prompt fires only on an interactive run; `--yes` or a non-tty
warns and proceeds, and `--dry-run`/`--explain` just narrate the `curl`.
A missing `curl` skips the check rather than blocking the upload. Only
PPA targets are checked — the Debian-archive default and explicit
`--upload-target` hosts have no equivalent pre-check.

### Workspace: update URL
The repo was renamed from `fedora-cve-triage` to `sandogasa`, with
`fedora-cve-triage` now only one of the many tools and library crates.

But the URL in the root `Cargo.toml` was never updated, so while users
clicking through from `crates.io` will get redirected to the correct
repo, this is both slightly confusing and inefficient.

## v0.15.0

### dbranch: bulk no longer hides the checked-out PPA branch

A bulk `rebuild` excluded the merge source from the target set. Since
the Debian branch isn't a Ubuntu codename it was already excluded by the
codename filter, so that exclusion only ever bit when you were checked
out **on a PPA branch** — silently dropping it from the rebuild set (and
treating it as the merge source). Bulk now selects every live Ubuntu PPA
branch regardless of what's checked out. A bulk **merge** still needs the
Debian branch as its source, so it now refuses early with a remedy if
the source is a PPA branch (check out the Debian branch or pass
`--source`); non-merge bulk stages (e.g. `--stage upload`) run from any
branch.

### dbranch: `update` upload to the archive requires a Debian host

`dbranch update`'s upload stage `dput`s to the Debian archive
(unstable), which only works on a Debian host — Ubuntu's `dput` doesn't
understand that target. It now hard-fails early on a non-Debian host
when uploading to the **default** target. `import`/`build`/`lint` are
unaffected (they run fine on Ubuntu), an explicit `--upload-target`
(e.g. `mentors`) is exempt, and a `--dry-run` is exempt.

### dbranch: fix a missing blank line in changelog conflict resolution

When resolving a `debian/changelog` merge conflict, the incoming Debian
stanza and the local rebuild stanza could run together (footer line
immediately followed by the next header) when git drew the hunk
boundary without a trailing blank on the incoming side. The resolver
now normalizes the junction to exactly one blank line. Observed on a
proposed-update merge; the same shared code path can in principle hit
it on a PPA rebuild merge, though that hasn't been seen in practice.

### dbranch: rebuild auto-detects Debian proposed-updates

`dbranch rebuild debian/<codename>` now recognises a Debian stable
branch (codename via `debian-distro-info`, e.g. `trixie` → 13) and
switches from the Ubuntu PPA scheme to a proposed-update:

- **version** `<base>~deb<N>u<M>` (tilde, so it sorts *older* than the
  plain build and never shadows testing/unstable on upgrade), `M`
  incrementing from the changelog like the PPA `+N` counter
- **changelog distribution** the codename (`trixie`)
- the changelog command shown/run is `gbp dch --stable` (not `--bpo`);
  the entry is still normalized to the `~` form + `* Rebuild for
  <codename>`
- **salsa-ci.yml** gets `RELEASE: "<codename>"` and **none** of the
  backports relaxations (it's a real stable build)
- **upload** goes to `dput`'s default target (the Debian archive) — no
  `--ppa`/`--upload-target` needed (an explicit `--upload-target` is
  still honored); the PPA "upload needs a target" rule applies only to
  PPA branches

Needs `debian-distro-info` (from `distro-info`) — consulted only for
`debian/`-namespaced branches, so plain PPA rebuilds are unaffected.

A proposed-update run **requires a Debian host** and hard-fails early
otherwise (`gbp dch --stable` needs a newer gbp, and the stable build
chroot and `dput`-to-stable are Debian-only). A `--dry-run` is exempt
(it executes nothing).

### dbranch: `--urgency` to override the changelog urgency

`dbranch rebuild` and `dbranch update` now take `--urgency <level>`
(default `medium`), passed to `gbp dch` as `-U`. Use `--urgency high`
(or `critical`) for a security upload. The value is passed through to
`dch`, which validates it.

### dbranch: `update` subcommand — new-upstream update of the Debian branch

`dbranch update [<branch>]` updates the Debian branch
(`master`/`main`/`debian/unstable`, default the current branch) to a
new upstream: `gbp import-orig --uscan --pristine-tar --no-interactive`
then `gbp dch -c -R -D unstable`, then the shared
`build → lint → push → upload → tag` tail. Differences from `rebuild`:

- The changelog is **not** normalized — `gbp dch`'s entry stands (a
  genuine new-upstream version, no `~codename+N` suffix), so commits
  since the last release show up as bullets. The distribution is pinned
  to `unstable` (`-D`) so dch's release heuristic can't substitute the
  host's own (e.g. an Ubuntu devel codename), which would fail Debian CI.
- The build suite is decoupled from the changelog distribution: builds
  against **testing** by default, `--build-suite unstable` to switch.
- Upload defaults to dput's own target (the Debian archive) with no
  flag; `--upload-target mentors` for a vetted upload (no `--ppa`).
- Self-heals a partial run: if a previous `update` imported the upstream
  but failed before writing the changelog, `gbp import-orig` refuses to
  re-import — the import stage now treats that one refusal as success and
  continues to `gbp dch`, so a plain re-run recovers. Other failures
  still propagate.

Stages: `import` (head) + `build`/`lint`/`push`/`upload`/`tag`; default
`import`, `all` = `import,build,lint,push`. Needs `devscripts` (uscan)
and `pristine-tar` for the import. Internally the
`build → … → tag` pipeline is now shared between `rebuild` and
`update`.

### dbranch: synthesize the rebuild changelog body (list adjusted files)

The rebuild changelog entry no longer flattens to just `* Rebuild for
<codename>` *or* dumps `gbp dch`'s body (which, after merging the
Debian branch, lists the entire merged Debian delta). `normalize_top_
stanza` now discards gbp's body and synthesizes a clean one: `* Rebuild
for <codename>` plus, when dbranch adjusted packaging files this run, a
single `* Adjust <files> for <codename>` line (e.g. `* Adjust gbp.conf
and salsa-ci.yml for questing`). So the first rebuild of a branch
records the gbp.conf/salsa-ci.yml setup; later rebuilds (nothing to
adjust) just say `* Rebuild for <codename>`. Discarding gbp's body also
drops any stray `UNRELEASED`.

### dbranch: rebuild self-heals an unadjusted existing PPA branch

Rebuilding an existing PPA branch whose `debian/gbp.conf` still pointed
`debian-branch` at the Debian branch — e.g. one branched by hand from
`main`/`debian/unstable` without dbranch — failed: `gbp dch` refused
("you are not on branch '<x>'") and the codename was wrongly taken from
gbp.conf (`-D main`). Now the codename is derived from the branch name,
and the merge stage applies the gbp.conf (`debian-branch` /
`debian-tag`) and salsa-ci.yml adjustments before `gbp dch` for
**existing** branches too (idempotent — a no-op on already-adjusted
branches), so such a branch is fixed up automatically on first rebuild.

### dbranch: safer bulk run — codename selection, EOL check, confirmation

A no-argument `dbranch rebuild` (bulk mode) is now both safer and more
deliberate:

- **Selection by Ubuntu codename.** It picks only branches whose
  codename is a real Ubuntu release (via `ubuntu-distro-info --all`) —
  `noble`, `ubuntu/questing`, etc. — so it no longer sweeps up the
  Debian branch, `master`/`main`, Debian suites (`debian/trixie`,
  `bookworm-backports`), or gbp plumbing. (Replaces the old
  "every local branch except a few" heuristic.) Bulk mode now requires
  `ubuntu-distro-info` (from the `distro-info` package).
- **EOL releases skipped by default.** A codename no longer in
  `--supported` is end-of-life; those are skipped (with a note).
  `--include-eol` rebuilds them too — but only locally: it can't be
  combined with the `upload` stage, since Launchpad rejects uploads to
  an EOL series' PPA.
- **Newest release first.** Bulk branches are processed in release
  order, newest first (oldest last), using `ubuntu-distro-info`'s
  ordering. A failed stage still aborts the whole run (unchanged), so
  the newest releases are attempted first.
- **Confirmation before work.** The resolved branch set is printed and
  confirmed (`[Y/n]`, default yes) before anything runs. `--yes`/`-y`
  skips the prompt for scripted runs; `--dry-run` just prints the set;
  a non-terminal stdin without `--yes` is refused with a remedy rather
  than run unconfirmed.

`rebuild --help` gains a **Bulk** section for `--yes`/`--include-eol`.

### dbranch: `--explain` shows a diff of each hand-edit

Under `--explain`, after dbranch edits a file itself — resolving the
`debian/changelog` conflict, normalizing the rebuild entry, or the
gbp.conf / salsa-ci.yml tweaks — it now runs `git diff` on that file
and pauses, so you see exactly what changed before it's committed
(`git diff` being a real command you could run yourself). No effect
outside `--explain` or under `--dry-run` (nothing is edited there).

### dbranch: refresh the pbuilder chroot before building; group `--help`

The build stage now refreshes the codename's pbuilder base chroot
(`pbuilder-dist <codename> update`) before building when it exists but
is older than a day, so builds aren't against stale packages. Control
it with the mutually-exclusive `--refresh-chroot` (force, regardless of
age) and `--no-refresh-chroot` (skip; build against the chroot as-is);
the default auto-refreshes only when stale. A brand-new chroot is still
created (`… create`) as before.

`dbranch rebuild --help` now groups flags into **Stages**, **Upload**,
and **Output** sections for readability.

### dbranch: `fixup` subcommand for existing branches

`dbranch fixup [<branch>...]` applies the PPA-branch packaging
adjustments — gbp.conf's `debian-branch` / `debian-tag` and the
salsa-ci.yml preset, the same ones the `merge` stage makes when
creating a branch — to **existing** branches, to repair ones set up
before (or outside) dbranch (e.g. branches missing `debian-tag`, which
made `gbp tag` use the wrong namespace). It checks each branch out,
adjusts, and commits what changed; idempotent, and defaults to the
current branch. Errors up front if there's no `debian/` directory (run
from the wrong repo).

### dbranch: `--source` to override the merge source branch

`dbranch rebuild --source <branch>` sets the branch merged into each
target, instead of always using the checked-out branch — so dbranch
can run without first checking out the Debian branch (e.g. from another
branch or a detached HEAD). The source is validated up front (clear
error if the ref doesn't exist) and feeds the `target == source` guard
and bulk exclusions.

### dbranch: `tag` stage (gbp tag)

A new `tag` stage tags the release: it first runs `dh clean` (`gbp tag`
refuses a dirty tree, and `debuild -S` leaves a generated
`debian/files`), then `gbp tag` — which derives the version from
`debian/changelog` and gbp's `debian-tag` format. Runs after `upload`
in the pipeline and is **opt-in** (not part of `all`). Adds `dh`
(debhelper) to the per-stage tool check, and requires `gbp` whenever
`tag` is selected.

### dbranch: `upload` stage (dput)

A new `upload` stage `dput`s the built source `.changes` (from
`debuild -S`) to its archive. The target is given by `--ppa
<user/name>` (sugar that becomes a `ppa:<user/name>` dput target; a
leading `ppa:` is tolerated) or `--upload-target <host>` for any dput
host (e.g. `mentors`, `ftp-master`); the two are mutually exclusive,
and the stage errors up front if neither is given. It runs after
`push` in the pipeline (so CI can pass before publishing) and is
**opt-in** — `--stage all` stays `merge + build + lint + push` and does
not upload. Adds `dput` to the per-stage tool check.

### dbranch: per-job CI watch progress

While watching a pipeline (push stage / `watch-ci`), dbranch now also
polls the pipeline's jobs (`glab api projects/:id/pipelines/<id>/jobs`)
and prints each job as it finishes — `✓ <name> (<stage>)` on success,
`✗ … — <status>` on failure — instead of only the pipeline-level
state. Best-effort: a failed jobs query is ignored (the pipeline poll
still drives pass/fail).

### dbranch: `--quiet` mode

`dbranch rebuild --quiet` (`-q`) suppresses the shelled-out tools'
output (git, gbp, debuild, pbuilder-dist, lintian), leaving just
dbranch's own step narration. Each command's output is captured and
replayed only if it fails, so problems stay diagnosable. Mutually
exclusive with `--explain` (the opposite, step-through verbose mode).

### dbranch: adjust gbp.conf and salsa-ci.yml when creating a new PPA branch

Creating a brand-new PPA branch now performs the two one-time
packaging tweaks the workflow needs, each as its own signed commit:

- `debian/gbp.conf`: point `debian-branch` at the new branch itself
  (so its codename resolves correctly and gbp treats it as its own
  Debian branch), and set `debian-tag` to the `ubuntu/%(version)s`
  format so `gbp tag` tags under `ubuntu/` (matching the branch's
  namespace) rather than gbp's default `debian/`.
- `debian/salsa-ci.yml`: inject the PPA-rebuild `variables` preset —
  `RELEASE: "unstable"` (salsa-ci builds against Debian unstable) plus
  the backports-style relaxations `SALSA_CI_LINTIAN_SUPPRESS_TAGS`,
  `SALSA_CI_DISABLE_VERSION_BUMP`, `SALSA_CI_DISABLE_PIUPARTS` —
  preserving the file's existing entries and comments.

Both edits are idempotent and skipped when the file is absent or
already adjusted; they only run on the new-branch (create) path, not
when merging into an existing branch.

### dbranch: track remote-only target branches instead of recreating them

A target branch that exists on `origin` but was never checked out
locally was misclassified as new and recreated from the Debian branch
(`git checkout -b <branch> <debian-branch>`), discarding the real PPA
branch's history. dbranch now classifies a target as local,
remote-only, or new: a remote-only branch is checked out as a tracking
branch from `origin/<branch>` (`git checkout -b <branch>
origin/<branch>`) and then merged/built as usual; its codename is read
from `origin/<branch>`'s `debian/gbp.conf`. Only a branch that exists
nowhere is created from the Debian branch. (Bulk, no-argument runs
still only consider local branches.)

dbranch also skips a redundant `git checkout <branch>` when already on
the target branch (the build/lint/push path) — it would otherwise just
print `Already on '<branch>'` and add a pointless `--explain` pause on
a no-op.

### sandogasa-cli: unified tool-availability check (breaking)

One batch function now covers both existence and probe checks:
`require_tools(&[(exe, install_hint, Option<probe>)])`. Each tuple's
optional probe means *run* `<exe> <arg>` and require a zero exit
(e.g. `Some("--version")`, `Some("version")` for koji, `Some("--help")`
for pbuilder-dist); `None` checks only `$PATH` existence. It checks
every entry and returns one error listing all missing tools with
their install hints. `tool_exists(name)` remains as the bare PATH
check.

Removed `require_tool` and `require_tool_with_arg`. Migrate:
`require_tool(n, h)` → `require_tools(&[(n, h, Some("--version"))])`;
`require_tool_with_arg(n, arg, h)` → `require_tools(&[(n, h, Some(arg))])`.
Callers updated: ebranch (fedrq/koji/bodhi), poi-tracker (koji),
koji-diff (koji), and dbranch (git/gbp/debuild/lintian probe
`--version`, pbuilder-dist probes `--help`).

### dbranch: new tool

A helper for propagating a Debian package across its Ubuntu/PPA
branches. Run from the Debian branch (whatever is checked out —
`master`, `debian/unstable`, …), `dbranch rebuild <branch>...` brings
that branch into each named PPA branch. A target that doesn't exist
yet is created from the Debian branch; with no targets it does all
existing PPA branches (every local branch except the current one and
gbp's `upstream` / pristine-tar). The codename comes from an existing
branch's `debian/gbp.conf` (`debian-branch` basename) or the branch
name's basename.

Work runs in `rpmbuild`-style stages via `--stage` (default `merge`):

- `merge` — switch to / create the target, merge the Debian branch,
  resolve the `debian/changelog` conflict deterministically (incoming
  Debian entry above the existing rebuild entry — the
  `dpkg-mergechangelogs` result), then `gbp dch --bpo -R -D <codename>`
  and normalize the stanza to `<debver>~<codename>+<N>` /
  `* Rebuild for <codename>`. The Debian base version is detected even
  from a PPA branch (a `~<codename>+<N>` suffix is stripped).
- `build` — `debuild -S -sa -d` + `pbuilder-dist` (opt-in for now),
  creating the codename's pbuilder chroot first
  (`pbuilder-dist <codename> create`) when
  `~/pbuilder/<codename>-base.tgz` is absent.
- `lint` — `lintian -I` on the built `.deb`s in
  `~/pbuilder/<codename>_result/` (`-I` includes info-level tags;
  linting binaries directly avoids re-unpacking the source, which
  `debuild -S` already lints). lintian is quiet when clean, so its
  output is echoed and a tag-count summary printed.
- `push` — push the branch (`git push -u origin <branch>` the first
  time, to set the upstream the remote ref didn't have yet; a plain
  `git push` once it tracks `origin/<branch>`), then (unless
  `--nowait`) watch
  that commit's GitLab CI pipeline to completion via the `glab` CLI,
  which auto-detects the salsa host / project from the git remote.
  dbranch polls `glab ci list --sha <commit> -F json` — targeting the
  exact pushed commit, not the branch, so it never latches onto the
  *previous* commit's pipeline during the post-push window before the
  new one is created. (It deliberately avoids `glab ci status`, whose
  `--live` needs a TTY to wait and whose action menu otherwise blocks;
  glab is still run with stdin on `/dev/null` as a backstop.) It waits
  until the pipeline reaches a terminal state: `failed`/`canceled`
  propagates a non-zero exit; `success`/`skipped`/`manual` pass; if no
  pipeline appears within ~3 min it's treated as benign (nothing to
  watch). The instance's glab auth is verified first
  (`glab auth status --hostname <host>`, host derived from the
  `origin` remote) — glab keeps a token per host, so this fails early
  with the `glab auth login --hostname <host>` command rather than a
  downstream API error (glab's own output is captured and only shown
  on failure, since older glab misreports a working token as invalid).
  `--nowait` pushes without waiting; attach later with
  `dbranch watch-ci [<branch>]` (defaults to the current branch, and
  likewise watches the branch-tip commit's pipeline) — e.g. after a
  `--nowait` push or a dropped connection. Adds a `serde_json`
  dependency (to parse glab's pipeline JSON).
- `all` — all of the above.

A failing stage command propagates its **real exit code** (lintian
uses its default — non-zero on error-level tags; a failed push or CI
pipeline propagates `git`'s / `glab`'s code), so `dbranch` exits with
the same status rather than a generic `1`.

dbranch is also a learning tool. `--dry-run` prints every command
without running anything; `--explain` runs the workflow but narrates
each command and pauses for Enter before running it (a step-through,
Ctrl-C aborts) for following along or sanity-checking; the two
compose. Narration is color-coded via `anstream`/`anstyle`,
auto-disabled when piped or under `NO_COLOR`. Also adds `anstream`
and `anstyle` to the workspace.

### hs-relmon: skip archived / issues-disabled GitLab projects when filing

`check-manifest` and `check-latest --file-issue` now check a
project's status before filing and skip — with a one-line note rather
than a counted error — when the GitLab project is archived
(read-only) or has the Issues feature disabled. Both states return
`403 Forbidden` on issue creation; previously each such package was
reported as a failure (and `check-manifest` exited non-zero). Seen on
e.g. `socat` (archived) and `mesa` / `centos-release-hyperscale`
(issues disabled). On a status-lookup failure the tool assumes filing
is allowed so a transient error never silently suppresses an issue.
New `sandogasa_gitlab::ProjectStatus` and
`sandogasa_gitlab::Client::project_status`.

### hs-relmon: new `file-conflicts` subcommand

Finds files shipped by more than one source package across the
repositories enabled together on a Hyperscale host. Where
`dupe-subpkgs` catches two sources shipping the same binary RPM
*name* in one tag, this catches the sharper case: a **file** conflict
between differently-named RPMs in *different* repos — e.g. the
`kernel` source ships `/usr/bin/ynl` and the `pyynl` tree inside
`python3-kernel-tools` (kernel repo) while a standalone `python3-ynl`
(main repo) ships the same paths, so dnf hits a conflict that name-
and-tag matching never sees.

Scans per EL version over the enabled repo set (default `main` +
`kernel` on EL10/10s; `main` only on EL9/9s, which has no kernel
repo; override with `--repositories`), pulling each binary RPM's file
list from Koji via `listRPMFiles` batched through `system.multicall`
(a whole tag is a handful of HTTP requests, not one per RPM), then
flags any path owned by two or more distinct sources. Directories,
`%ghost` entries, and debug payloads under `/usr/lib/debug` /
`/usr/src/debug` are excluded. `--release` limits which Hyperscale
releases are scanned (CSV of `9`, `9s`, `10`, `10s`) and `--package`
reports only conflicts involving named source packages. Read-only,
`--json` output, exits non-zero on any conflict. New
`cbs::Client::list_rpm_files_multi`, `cbs::RpmFile`,
`cbs::RpmFileList`, and `cbs::TaggedBinary.rpm_id`.

### hs-relmon: new `dupe-subpkgs` subcommand

Finds binary RPMs shipped by more than one source package within a
single Hyperscale tag. Hyperscale overrides stock CentOS packages
and occasionally moves where a binary RPM is built from (e.g.
splitting `perf` out of `kernel-tools`); mid-move, two source
packages can ship the same binary in the same tag, leaving the
depsolver to pick one. The redundant source should be retired.

Scans each repository's `-release`/`-testing` tags (EL9/EL10 and the
Stream variants) via Koji `listTaggedRPMS` (latest build per source,
`inherit=false` so only Hyperscale-tagged content counts), maps
binary-RPM name → distinct sources, and flags any with two or more.
Detection is per-tag (a collision only matters when both providers
land in the same enabled repository); `-debuginfo`/`-debugsource`
RPMs are excluded. Read-only by default (no Koji auth), `--json`
output, `--repositories` to scan beyond `main`, `--release` to limit
to specific Hyperscale releases (CSV of `9`, `9s`, `10`, `10s`), and
`--package` to report only collisions involving named sources;
exits non-zero when any collision is found.

`--fix` adds an interactive resolution pass: for each cluster of
sources sharing a binary it recommends untagging the oldest build
(the likely stale leftover) and — crucially — lists the binaries
that only each candidate provides, so you can see what would vanish
from the tag. Untagging `kernel-tools` to resolve a `perf` collision
would also drop `cpupower`, `rtla`, `rv`, … so the choice stays with
a human (one prompt per cluster, default skip; requires CBS auth). In
`--json` mode or without a terminal the plan is printed and nothing
is untagged. New `cbs::Client::list_tagged_binaries`,
`cbs::TaggedBinary`.

### fedora-cve-triage: new `interpreter-fps` subcommand

Detects CVEs that live in a language interpreter/runtime but were
filed against an application merely written in that language — e.g.
CVE-2025-13836 (a DoS in CPython's `http.client`, NVD product
`python:python`, fixed in cpython) misfiled against `asahi-installer`,
a Python app that ships no interpreter. The fix arrives via the
`python3.x` update, so the application's bug is a false positive.

A bug is flagged only when every product the CVE marks affected is
the interpreter itself (so a CVE that also names a real product is
never swept up) and the component is not an interpreter package
(`python3`, `python3.NN`, `pypy`, …). Scans by `components` or by `assignees`
(sweep a maintainer's CVE bugs); closes detected FPs as NOTABUG +
tracker block with `--close-bugs`, mirroring `js-fps`. Python today;
the interpreter table extends to other runtimes. New
`sandogasa_nvd::CveResponse::affected_products`.

## v0.14.0

### poi-tracker: export drops unshipped packages from the hs-relmon manifest

`export` (hs-relmon manifest) now excludes packages marked
`unshipped` in the inventory — they're gone (no CBS builds), so
hs-relmon has nothing to track or prune for them. Existing
manifest entries for unshipped packages are removed
unconditionally (not gated by `--prune`, since `unshipped` is an
explicit marker), and new ones are never added; the count is
reported. The inventory keeps the tombstone, so a revived package
returns to the manifest on the next export. Fixes unshipped
packages (e.g. one whose builds `prune-archived` cleaned up)
lingering in the manifest as normal tracked entries. New
`MergeResult.unshipped_removed`.

### hs-relmon: check CBS auth before pruning

`prune-tags`, `prune-manifest`, and `prune-archived` now verify an
authenticated CBS koji session up front (via `koji moshimoshi`)
before any read-side planning, failing fast with an actionable
hint (`run centos-cert`) instead of erroring at the first
untag after a long scan. Dry runs skip the check (read-only). New
`sandogasa_koji::check_auth`.

### hs-relmon: new `prune-archived` subcommand

Cleans up CBS builds for packages whose upstream repo is archived
(manifest `archived = true`, from
`poi-tracker sync-gitlab --mark-unshipped`). For each archived
package it compares every build in its `-release`/`-testing` tags
against the stock distro version for that tag's channel — CentOS
Stream N for Stream tags (`hyperscaleNs`), AlmaLinux N for RHEL
tags (`hyperscaleN`) — and untags builds at or behind stock
(redundant now). Builds newer than stock, or with no stock entry,
are never untagged automatically (the archived repo may be their
only source): they are prompted per build, and `--yes` warns and
skips them. New `repology::{centos_stream_release, almalinux_release}`
and an `archived` field on hs-relmon's manifest `PackageEntry`.

### poi-tracker: export carries the archived-builds marker to hs-relmon

`poi-tracker export` (hs-relmon manifest) now writes `archived =
true` on packages whose inventory `archived_builds` marker is set,
so hs-relmon knows which archived-upstream packages have CBS
builds to prune. Reconciled bidirectionally on every export — a
reactivated package loses the flag. hs-relmon's manifest gains a
matching `archived` field on `PackageEntry`/`ResolvedPackage`
(its prune logic is still the pending follow-up).

### poi-tracker: `sync-gitlab --mark-unshipped` (CBS release check)

Cross-checks each GitLab-synced project against CBS (CentOS koji)
and records archival state. An archived GitLab repo with no
released CBS build is marked `unshipped` (a tombstone, skipped
like a retired package); an archived repo that *still* has
release builds is marked `archived_builds` — it still ships, so
it is not skipped, but its lingering builds are a cleanup
candidate and the command suggests running hs-relmon to prune
them. "Released" respects each SIG's lifecycle: Hyperscale ships
for both RHEL `N` and CentOS Stream `Ns` (`hyperscaleN-*-release`
or `hyperscaleNs-*-release`), Proposed Updates is Stream-only;
`--centos-release` sets the valid majors (default `9,10`).
Requires `koji` with the `cbs` profile.

New surface: `Package.archived_builds` + `has_archived_builds()`
in sandogasa-inventory (schema regenerated; merged field-aware),
`sandogasa_koji::{list_tags, list_tagged_package_names}`, and
`sandogasa_gitlab::list_archived_project_names`. The marker
apply logic is now field-generic (`prune_retired::apply_marker`),
shared by `unshipped` and `archived_builds`.

### Dependencies: reqwest 0.13, toml 1, toml_edit 0.25, quick-xml 0.40 (breaking)

Bumped the four deferred major dependency upgrades, all now in
Fedora/EPEL (reqwest 0.13.3, toml 1.1, toml_edit 0.25.12,
quick-xml 0.40.1; lockfile pinned to the Fedora-shipped point
releases).

Breaking (API): `reqwest::Error` appears in some public
signatures (e.g. `sandogasa-bodhi`'s query methods), so the
reqwest major bump changes that type's identity for library
consumers. No sandogasa API was intentionally changed.

TLS posture change: reqwest 0.13's default `rustls` feature pulls
in `aws-lc-rs`, which is not packaged in Fedora, so we build with
`rustls-no-provider` and keep the ring crypto provider (as
before, statically linked, build-dep only — no runtime RPM). The
provider is no longer compiled in as a default, so it is
registered at startup via the new `sandogasa_cli::init()` (called
from every tool's `main`) and defensively in the library client
builders. Trust roots now come from the system store
(`rustls-platform-verifier` → Fedora's `ca-certificates`) instead
of a copy of Mozilla's CA list baked into the binary, so CA
updates flow through `dnf` rather than a rebuild.

New `sandogasa-cli` surface: `init()` (standard per-`main`
startup hook — extend it for future cross-cutting setup) and
`install_crypto_provider()`.

quick-xml 0.40 migration: `BytesText::unescape` is gone and
`read_text` now yields a raw `BytesText`; the koji-diff and
hs-relmon XML-RPC parsers decode + unescape explicitly via a
local helper.

### koji-diff: fix the koji availability check

`koji-diff` checked for koji with `koji --version`, which exits 2
(koji uses the `version` subcommand), so it always aborted with
"is it installed correctly?" even on a working koji. Switched to
`require_tool_with_arg("koji", "version", ...)`, matching ebranch.

### sandogasa-distgit: group syncs no longer import non-rpms projects (breaking)

A Pagure group's project listing includes everything the group
can access — `container/`, `tests/`, and `modules/` projects and
forks, all reported under their bare names — and the group
endpoint honors neither `namespace=` nor `fork=false` (found
live: `container/python-classroom` imported into a
python-packagers-sig inventory as package "python-classroom",
`modules/askalono-cli` into a rust-sig inventory as
"askalono-cli", plus 117 forks). `group_projects` now records each project's
`fullname` and keeps only the `rpms/` namespace; skipped
projects are counted in the sync output. A re-sync with
`--prune` clears previously imported strays.

Breaking (API): `ProjectInfo` gained a public `fullname` field —
code constructing it with a struct literal must add it.

### poi-tracker: `prune-retired` flags nonexistent projects as invalid entries

A dist-git 404 means the inventory entry itself is wrong — a
non-RPM repo (module, container image, tests) imported under its
bare name by an older group sync, or a binary subpackage name
recorded instead of the source package. The first full scan
marked such an entry `unshipped`, which was misleading. 404s are
now reported as a separate "invalid entries — fix or remove"
list, never marked; a stale marker on such an entry is cleared
by the next run.

### poi-tracker: `sync-distgit --mark-unshipped`

Run the prune-retired check on the packages a sync adds (bounded
by `-j`/`--jobs`, default 8), so a fresh inventory starts with
its `unshipped` markers in place instead of needing a follow-up
`prune-retired` run. Best-effort: a failing check warns and the
sync still saves. New library surface:
`prune_retired::scan_packages` and `active_branches_from_bodhi`
extracted from the prune-retired flow.

### sandogasa-inventory: field-aware multi-inventory merge (breaking)

Merging inventories (poi-tracker's `-i`/`-I` with multiple files)
previously replaced a colliding package entry wholesale with the
later file's version, silently dropping fields the later file
didn't set — including `priority`, `retired_on`, and `unshipped`
markers. Merges are now field-aware: the later file's set fields
win, its unset fields keep the earlier values, `retired_on` is
unioned, and `unshipped` survives a bare later entry. Genuine
conflicts (both files set different values) are reported on
stderr, later file winning.

Breaking (API): `Inventory::merge` now returns `Vec<String>`
(conflict notes) instead of `()`; new `Package::merge_from`.
Callers that ignored the old unit return just discard the new
return value. `Package` also gained public fields this release
(`unshipped`, `archived_builds`) — code constructing one with a
struct literal must add them.

### poi-tracker: parallel `prune-retired` scan

The scan now checks packages concurrently, bounded by `-j`/`--jobs`
in-flight dist-git requests (default 8) — roughly an 8x speedup,
turning a 4500-package inventory from an hour into minutes. The
report order and the abort-on-persistent-failure behavior are
unchanged.

### sandogasa-bodhi: retry transient failures on the auth path

Token refresh, OIDC metadata/userinfo fetches, and Bodhi's
login/csrf requests now retry transport errors and 5xx responses
with backoff (new `auth::send_with_retry`). These requests run
right when `--give-karma` is about to post — after minutes of
analysis — so a single connection blip previously wasted the
whole run. The comment POST itself is deliberately not
auto-retried, since repeating it after an ambiguous failure could
double-post.

### ebranch: reviewer notes and provenance in posted reports

The comment `--give-karma` posts is always the full Markdown
check report, now with a provenance footer recording the ebranch
version and the command invocation that produced the analysis.
`--comment <TEXT>` adds reviewer notes as a section near the top
of the report (it no longer replaces the report); when the flag
is omitted you are prompted for notes interactively, and `--yes`
skips the prompt.

### ebranch: own-update detection no longer aborts the vote

A transient network failure looking up the session username (for
own-update karma skipping) aborted `--give-karma` after the whole
analysis had already run. The lookup now retries with backoff and,
if it still fails, warns and proceeds assuming a foreign update —
Bodhi enforces the own-update karma rule server-side regardless.

### ebranch: fix false "removed Provides" for compat packages (@testing path)

`check-update` compared provides per source package when querying
via `@testing`, so an update that bumps a crate and adds a compat
package shipping the old version (e.g. rust-const-oid 0.10 +
rust-const-oid0.9) falsely reported the old provides as removed —
and flagged reverse deps as broken. Provides are now unioned
across all packages in the update on both sides before comparing,
matching what the side-tag (koji) path already did.

### poi-tracker: new `prune-retired` subcommand

Finds inventory packages no longer carried on any active branch:
the dist-git project is gone (404), it has no branch on an active
release, or it is retired (`dead.package`) on every active branch
it has. The active branch set is queried from Bodhi's active
releases (plus rawhide), or set explicitly with `--branch`.

By default matches are marked with an `unshipped` reason in the
inventory rather than deleted — retired packages keep their ACLs,
so deleted entries would come straight back on the next
`sync-distgit`. The marker drives the rest of the tooling:
`triage-updates` and `semver-audit` skip unshipped packages,
`triage-retired` still processes them so remaining bugs get
closed, and `sync-distgit`/`sync-gitlab` `--prune` preserve them.
Markers are refreshed in both directions (a revived package is
unmarked). `--remove` deletes entries outright; `--dry-run`
previews; the usual `--pattern`/`--start-from`/`--end-with`
filters apply.

New library surface: `Package.unshipped` + `is_unshipped()` in
sandogasa-inventory (and the JSON schema), and
`DistGitClient::project_branches` in sandogasa-distgit
(`list_branches` that reports a missing project as `Ok(None)`
instead of an error).

### ebranch: `check-update --give-karma` casts karma with per-bug feedback

`check-update` can now vote on the Bodhi update it just checked:
`--give-karma` posts a comment with overall karma plus per-bug
feedback, like the web UI. The check result suggests the overall
karma — `+1` when no issues are found, `-1` when reverse deps
break or the updated packages have unsatisfied deps, `0` when
the analysis was incomplete — and the user is prompted with that
suggestion as the default. Update-request bugs
(`<pkg>-<version> is available`) are auto-voted `+1` when the
update delivers at least the requested version (by rpm version
comparison) and `-1` otherwise; other bugs are put to the user.
The full plan is shown for confirmation before posting; `--yes`
skips prompts (non-update bugs get `0`) and `--comment <TEXT>`
overrides the comment text, which defaults to the full Markdown
check report. On the user's own updates the overall karma is
skipped (Bodhi ignores submitter karma; the plan says so) while
per-bug feedback is still posted. Before any manual bug prompt
the update's notes are printed for context, and server-side
caveats from Bodhi are echoed after posting. Authentication reuses the bodhi CLI's cached OIDC
session (`~/.config/bodhi/client.json`), refreshing expired
tokens against the ID provider and writing them back. The
session is validated before the analysis runs; with no session,
an interactive bodhi CLI login is started up front.

New library surface: `sandogasa_bodhi::auth` (bodhi CLI session
reuse: `cli_session_token`, `load_tokens`/`save_tokens`,
`refresh_tokens`), `BodhiClient::with_token` (guarded by
`ensure_secure_url`), `BodhiClient::comment` with `bug_feedback`,
a `title` field on `BodhiBug`, and
`sandogasa_bugclass::bugzilla::extract_new_version` (moved from
poi-tracker's `semver_audit`, which now re-uses it).

### ebranch: `check-update` offers to regenerate stale side-tag repos

When the side-tag repodata lags koji (the V-R cross-check fails),
`check-update` no longer just warns: it now offers to run
`koji regen-repo --wait <side-tag>` on the user's behalf (default
yes), clears the fedrq metadata cache, and re-checks freshness
before running the provides comparison — so the analysis uses the
regenerated data instead of silently dropping reverse deps. If the
regen is declined, a second prompt asks whether to continue with
stale data (default no — the check aborts). Prompts only appear in
interactive runs; `--json` mode and non-terminal stdin keep the old
warn-and-continue behavior. New `sandogasa_koji::regen_repo()`
backs this.

## v0.13.0

### poi-tracker: `sync-distgit --fast` via the owner-alias dump

User syncs can now skip the prefix scan entirely: `--fast` fetches
Pagure's `extras/pagure_owner_alias.json` (one ~3 MB request) and
takes the user's directly-maintained packages from it — seconds
instead of minutes, with none of the group-derived download the
scan can't avoid. Trade-off (now also documented in
`crates/sandogasa-distgit/DEVELOPMENT.md` alongside the other
per-user query semantics): the dump records only direct
owner/admin/commit maintainers, so collaborator- and ticket-level
grants are missed — and `--prune --fast` would remove them.
`--fast` implies `--no-groups`; `--pattern`/`--exclude` apply
client-side. New `DistGitClient::user_packages_fast` backs it.

### poi-tracker: `sync-distgit` retries transport errors and resumes from partials

`DistGitClient`'s project queries now retry transient transport
failures (connection reset, timeout) with the same backoff already
used for 5xx responses, so a network blip no longer aborts a long
prefix scan. When a fetch still fails, `sync-distgit` saves the
failed pattern to `<output>.partial.state` next to the existing
`<output>.partial`; re-running the same command resumes from that
pattern (loading the partial as the base inventory), and a
completed run replaces `<output>` and removes both files.

### sandogasa-distgit: exclude forks from per-user project queries

`DistGitClient::user_projects` now passes `fork=false`: without
it, Pagure's listing includes the user's forks, and a fork is
reported under its bare package name with the user as `owner` —
indistinguishable from really owning `rpms/<pkg>`. Fork-only
packages therefore leaked into `sync-distgit --user` inventories
as direct/owner entries (even under `--no-groups`). Re-syncing an
affected inventory with `--prune` removes them.

### poi-tracker: remove deprecated `--auto-prefix --pattern` spelling (breaking CLI)

The pre-0.12.1 scan-resume spelling `sync-distgit --auto-prefix
--pattern <start>` — deprecated with a warning since v0.12.1 — is
now rejected: `--pattern` conflicts with `--auto-prefix`,
`--start-pattern`, and `--end-pattern`. Migration: use
`--start-pattern <prefix>` (optionally with `--end-pattern`).
This completes the removal scheduled in DEPRECATIONS.md.

### poi-tracker: consistent filters across walking commands (breaking CLI)

`semver-audit`, `triage-retired`, and `triage-updates` now share
the same package filters — `--pattern <glob>` (a bare name
matches exactly), `--start-from <name>`, and `--end-with <name>`
— and all three compose. `--batch [EMAIL]` is now available on
`triage-retired` too, replacing its per-retired-package-per-branch
Bugzilla searches with one email-scoped query matched locally;
with `--all-reporters` the batch query drops the reporter filter
as well.

Breaking CLI: `triage-retired --package <name>` is removed — use
`--pattern <name>` (an exact match when no glob characters are
used). `--pattern` no longer conflicts with the range flags.

### poi-tracker: record retirement in the inventory (breaking)

`triage-retired --mark` (single `-i` file only) now records its
findings in each package's new `retired_on` field — the list of
dist-git branches carrying a `dead.package` marker. The update is
bidirectional: a branch found live again is removed, so re-running
`triage-retired --mark` keeps the markers fresh. `semver-audit`
and `triage-updates` skip packages marked retired on rawhide
(their checks couldn't succeed anyway), which also saves their
per-package network traffic.

Breaking: `sandogasa_inventory::Package` gained the public field
`retired_on: Option<Vec<String>>` (and an `is_retired_on()`
helper); code constructing `Package` via a struct literal must add
`retired_on: None`. Inventories without the field parse unchanged.

### poi-tracker: `--batch` mode for `semver-audit` and `triage-updates`

Both subcommands previously issued one Bugzilla search per
inventory package, which dominates the runtime on a large
inventory. The new `--batch [EMAIL]` flag replaces them with a
single query for every open release-monitoring bug assigned to or
CC'ing EMAIL (default: the email configured via `poi-tracker
config`), matched against the inventory locally. Caveat: bugs
where that email is neither assignee nor CC'd are not seen, so
batch mode fits inventories of packages you (co-)maintain or
watch.

### Security: `--` separators before external-tool positional arguments

Shell-outs to `fedrq`, `koji`, `bodhi`, and `curl` now pass `--`
before positional arguments (package, tag, NVR, update alias,
URL), so a value beginning with `-` can never be parsed as a
flag by the external tool. Each tool's handling of `--` was
verified against the real CLI before the change; two call sites
in fedora-cve-triage also had their options reordered to come
before the positionals. `kinit` is intentionally unchanged
(verification was inconclusive, and its principal comes from
local user config). Defense-in-depth — Fedora package names
can't start with `-`.

### poi-tracker: `triage-updates` closes already-addressed bugs via Bodhi

`triage-updates` now checks every open release-monitoring bug
against Bodhi (and, for builds that predate the active releases,
against the branch's dist-git spec): when builds with the
advertised version or newer already exist, the latest addressing
build per release is written to the bug's Fixed In Version field,
and the bug is closed as `ERRATA` when stable in every active
release the package has a branch for, moved to `MODIFIED` while
any addressing update is still in testing, or — when only some
releases carry the fix (commonly just rawhide) — offered for
closing interactively. New flags: `--close-stale` closes the
partial cases without asking, `--skip-stale` disables the check
(restoring the previous priority-only behavior and cost), and
`--pattern <glob>` scopes the run to matching packages.
`semver-audit` now points at `triage-updates` from its "up to
date (stale bug)" group, mirroring its `triage-retired` hint.

The check short-circuits on rawhide: Fedora updates land in
rawhide first (a stable release may never carry a newer version
than rawhide), so a bug whose version isn't in rawhide — neither
in Bodhi nor committed to the dist-git spec — skips the
stable-release queries entirely. EPEL branches update
independently of each other, so EPEL bugs are always checked in
full.

### poi-tracker: new `semver-audit` subcommand

`semver-audit` classifies the pending upstream update for each
maintained package by semver impact, so a maintainer can see which
updates are safe to push. For every package (optionally filtered
by `--pattern <glob>`, e.g. `rust-*`) it reads the open
`upstream-release-monitoring@` "X is available" bug for the new
version, compares it against the rawhide dist-git spec's current
version, and reports **non-breaking** / **breaking** / **up to
date (stale bug)** / **retired (update request invalid)** /
**needs review**, grouped (or as `--json`). `--non-breaking` shows
only the safe updates. ("Up to date" means the packaged version
already matches the available version — the bug is stale.)

Classification follows Cargo's compatibility rule: a change at or
before the leftmost non-zero version component is breaking (so
`1.4 → 1.5` is safe but `0.4 → 0.5` is not). Semver build
metadata (a `+suffix`, e.g. `1.7.0+v1.7.0`) is ignored for the
comparison, per the semver spec. Non-numeric versions
(pre-releases, dates, snapshots) are reported as needs-review
rather than guessed. A package retired on rawhide (a `dead.package`
marker, the signal `triage-retired` keys on) is reported as
retired, consistent with that flow.

### sandogasa-distgit: validate identifiers before URL interpolation

`DistGitClient` now rejects package, branch, user, and group
names that aren't bare dist-git tokens (`[A-Za-z0-9._+-]`, and not
`.`/`..`) before building a request URL, returning an error
instead. This stops a value containing a path separator or a
parent-directory segment — which URL normalization could redirect
to a different resource — from reaching the wire. It matters
because some of these names arrive from API responses (e.g. a
Bugzilla component fed to `fix-version`), not just local config.
Valid Fedora names are unaffected; this is defense-in-depth.

### Security: refuse to send credentials over plaintext HTTP (breaking)

API clients now fail closed when a token would be sent to a
plaintext `http://` URL on a non-loopback host, so a misconfigured
base URL can no longer leak a Bugzilla API key or GitLab/GitHub
token in cleartext. Loopback hosts (`localhost`, `127.0.0.0/8`,
`::1`) stay allowed for mock servers and local development, and the
`SANDOGASA_ALLOW_INSECURE_URL=1` environment variable overrides the
guard for testing or a trusted internal proxy (see
`crates/sandogasa-cli/DEVELOPMENT.md`).

The shared check is the new
`sandogasa_cli::ensure_secure_url` (plus the
`sandogasa_cli::ALLOW_INSECURE_URL_ENV` constant), wired into the
Bugzilla, GitLab, and GitHub client constructors.

Breaking: `sandogasa_bugzilla::BzClient::with_api_key` now returns
`Result<Self, Box<dyn std::error::Error>>` instead of `Self` (it
can now reject an insecure URL). Migration: append `?` (or handle
the `Result`) at call sites — e.g.
`BzClient::new(url).with_api_key(key)?`. The GitLab/GitHub
`new()`/`validate_token()` signatures are unchanged (already
`Result`); they just gained the check. Jira and Discourse clients
are not yet guarded.

### Errata: v0.12.1 `sync-distgit --user` rationale

The v0.12.1 note for `poi-tracker sync-distgit` said "there is no
cheaper API that covers all ACL types." That is accurate but
misleads — it reads as "Pagure has no per-user endpoint." It does:
`/api/0/user/<name>` exists, but it returns HTTP 500 for prolific
users (it can't build the full response), and it wouldn't cover
non-owner ACLs (commit/collaborator/ticket) even if it worked. The
real situation is that *every* user-scoped path fails at Fedora
scale — `/api/0/user/<name>` 500s and `/api/0/projects?username=`
504s — which is why prefix-scanning is the default for `--user`.
Group syncs use `/api/0/group/<name>?projects=true`, a bounded,
indexed lookup, so they need no workaround.

### fedora-cve-triage: URL-encode Bugzilla query values

`build_multi_query` now percent-encodes product/component/status/
assignee values (matching `triage-retired`'s query builder),
instead of interpolating them raw. A value containing a space,
`&`, or `=` previously malformed the query string or injected an
extra parameter. These values come from trusted config, so this
is hardening rather than a fix for an exploitable issue.

### fedora-cve-triage: new `fix-version` subcommand

`fix-version` corrects CVE bugs filed against a dist-git branch
the package never shipped on (e.g. an `[epel-all]` bug landing on
`epel10` for a package that only has `epel8`/`epel9`). It
reassigns each such bug to the package's latest still-standing
branch in the same product family and marks it blocking the
configured tracker; if every branch in that family is retired,
the bug is reassigned to the latest one and closed as `CANTFIX`.
Bugs already filed against a real branch are left untouched.
Defaults to a preview; `--apply` writes the changes (with a
confirm prompt, and an offer to reassign the bugs to your
configured Bugzilla email), and `--component` narrows the run.
Branch existence and retirement come from dist-git (Pagure) —
no Koji or git-history lookups.

### sandogasa-distgit: `list_branches`

New `DistGitClient::list_branches` returns a package's dist-git
branch names via the Pagure `git/branches` API.

### poi-tracker: `triage-retired --branch` accepts multiple branches

`--branch` is now repeatable (and comma-separated), so one run
can check retirement across several dist-git branches (e.g.
`--branch epel8,epel9`). Each branch scopes its own Bugzilla
search and closure comment; a package retired on some branches
but live on others only has its bugs closed for the dead
branches. The default is still `rawhide`. Per-bug output and the
final tally now name the branch each closure is for.

### poi-tracker: `triage-retired --all-reporters`

New `--all-reporters` flag drops the release-monitoring reporter
filter so `triage-retired` closes **every** open bug on a retired
branch (CVEs, FTBFS, and other human-filed bugs included), not
just Anitya / the-new-hotness new-version bugs. The default
remains release-monitoring-only, which is safe to run routinely
across a whole inventory.

### sandogasa-report: group reports by domain in `--domain` order (breaking JSON)

A multi-domain report is now organized by domain rather than by
service. Each domain is a top-level `## <domain>` section,
emitted in the order the domains were passed on the command
line, with its Bodhi/Koji/GitLab/GitHub activity nested beneath
it as `###` subsections. Previously the report was grouped by
service (a fixed Bugzilla → Bodhi → Koji → GitLab → GitHub
sequence), so the first `--domain` had no bearing on output
order. Bugzilla remains a single aggregated section, now placed
immediately after the last domain that references it.

Breaking JSON: the top-level `koji`, `gitlab`, and `github`
objects (previously maps keyed by domain name) and the
top-level `bodhi` object are removed. They are replaced by a
`domains` array, each element `{ "name", bodhi?, koji?,
gitlab?, github? }`, in CLI order. `bugzilla` remains a
top-level sibling. Consumers that read e.g. `.koji["hyperscale"]`
must now find the matching entry in `.domains` and read
`.koji`.

Markdown also re-nests headings: services moved from `##` to
`###`, and their internal subsections from `###` to `####`.
`indexmap` is not used; domain order is carried by the
`domains` array/`Vec` directly.

### Deprecation tracking: DEPRECATIONS.md

New root-level `DEPRECATIONS.md` records deprecated
functionality with its deprecation release, planned removal
release, and replacement. The first entry pins the removal of
poi-tracker's deprecated `sync-distgit --auto-prefix --pattern
<start>` spelling to v0.13.0, and its runtime warning now
names that version.

## v0.12.1

### fedora-cve-triage: `--component` filter for `bodhi-check`

`bodhi-check` accepts a new `-c` / `--component` flag (CSV or
repeated) that limits the run to the given components,
overriding the config file's `components` list. This allows
scoping an assignee-based config to specific packages for one
run without editing the config.

### fedora-cve-triage: `bodhi-check` resolves rawhide bugs

Bugs filed against version `rawhide` previously produced
"cannot determine release" warnings and were skipped, because
the release name cannot be derived from the version field
alone. `bodhi-check` now resolves them to whichever release
Bodhi currently calls rawhide (`branch == "rawhide"`, e.g.
`F45`), where rawhide builds receive automatic updates. The
fedrq provides fallback for NVD product matching queries the
`rawhide` branch for that release (its lowercased name has no
repos until Fedora branches). An explicit `[fedora-NN]`
summary tag still takes precedence.

### poi-tracker: prefix mode is the default for `sync-distgit --user`

Pagure's unfiltered per-user projects query scans every
project's ACLs server-side and routinely exceeds the gateway
timeout (HTTP 504) — and there is no cheaper API that covers
all ACL types — so `sync-distgit --user` without `--pattern`
now scans one name prefix at a time (`a*`–`z*`, `0*`–`9*`).
Group syncs still issue a single query by default.

- `--pattern` now always means a single patterned query. The
  new `--start-pattern <prefix>` (with the existing
  `--end-pattern`, which no longer requires `--auto-prefix`)
  bounds the prefix scan and implies prefix mode, also for
  group syncs.
- The old scan-resume spelling `--auto-prefix --pattern
  <start>` is deprecated but still accepted with a warning,
  to be removed in the next breaking release. Use
  `--start-pattern <prefix>` instead.
- `--auto-prefix` remains as the explicit opt-in to a full
  scan (the way a group sync enables prefix mode).
- The new `--no-auto-prefix` flag forces a single unfiltered
  query (the old `--user` default); if both `--auto-prefix`
  and `--no-auto-prefix` are given, the last one wins.
- When an unfiltered query does hit a 504, the error output
  now suggests retrying with `--auto-prefix`.

### sandogasa-distgit: non-blocking retry backoff

`get_with_retry` now uses `tokio::time::sleep` instead of
`std::thread::sleep`, so the retry backoff yields to the tokio
runtime instead of blocking the worker thread. `tokio` is now a
regular dependency of the crate (it was already pulled in
transitively via `reqwest`).

## v0.12.0

### sandogasa-fasjson: `timezone` field on `FasUser` (breaking)

`sandogasa_fasjson::models::FasUser` gained a public field
`timezone: Option<String>` so callers can read the user's FAS
profile timezone (used by sandogasa-hattrack's `last-seen`,
below). Adding a `pub` field to a struct with no
`#[non_exhaustive]` marker is a semver-breaking change for
code that constructs `FasUser` via a struct literal; consumers
that only read fields are unaffected.

Migration: if you construct `FasUser` directly (rare; mostly
seen in tests), add `timezone: None` to the field list.

### sandogasa-hattrack: narrow `last-seen`'s service set

Two new flags on `last-seen` let callers skip the expensive
HyperKitty mailing-list scan (or any other service) when a
target user clearly doesn't use it:

- `--skip <list>` — comma-separated services to skip.
- `--only <list>` — comma-separated services to ONLY query
  (mutually exclusive with `--skip`).

Values: `bodhi`, `bugzilla`, `discourse`, `distgit`,
`mailman`. Skipped services don't appear in the human output
or JSON `services` array at all (rather than appearing with
"no activity").

### sandogasa-hattrack: public-holiday signal in `discourse` and `last-seen`

The `discourse` and `last-seen` subcommands now flag any
nationwide public holiday falling on the user's local date,
rendered as a `Holiday:` line under `Country:` (and as a
`holidays` array on each `LocalTimeEntry` / `LocalTimeReport`
in JSON output). Data comes from the Nager.Date public API
(<https://date.nager.at>) and is cached per country-per-year
at `$XDG_CACHE_HOME/sandogasa-hattrack/holidays/{CC}-{YEAR}.
json` (typically `~/.cache/...`), so repeat lookups never go
to the network. Only nationwide holidays (`global: true`) are
surfaced — we only know the country, not the subdivision.

When FAS and Discourse advertise different timezones, each row
gets its own holiday check, so a holiday in either location
shows up next to that location's `Local time:` line.

New global flags:
- `--no-holidays` skips the lookup entirely (useful offline).
- `--refresh-holidays` force-refetches the year's data even
  when a cached copy is present.
- `--now <YYYY-MM-DD | RFC 3339>` overrides "now" for the
  local-time / holiday computation. Intended for testing and
  demos — relative timestamps on other services are
  unaffected.

### sandogasa-hattrack: surface local time in `last-seen`

`last-seen` now prints the same `Local time:` / `Country:`
block already rendered by `discourse`, with the same colour
treatment (`--color`, `--working-hours`). Both FAS (via
FASJSON's previously-unused `timezone` field) and Discourse
are queried independently; when both advertise a timezone and
they agree, the block is rendered once, and when they
disagree (e.g. a traveller who updated Discourse but not FAS),
both are rendered side-by-side with a `[FAS]` / `[Discourse]`
suffix so the divergence is visible. JSON output gains a
`local_times` array on the top-level summary, one entry per
distinct timezone with its source(s) attached. The FAS-side
timezone read uses the new `FasUser.timezone` field (see the
`sandogasa-fasjson` entry above).

### sandogasa-hattrack: colour the local-time / weekday output

Adds ANSI styling to the `discourse` subcommand's `Local time:`
line: the weekday tag is green for a weekday or yellow for a
weekend, and the timestamp itself is dimmed when the local hour
sits outside working hours. JSON output is unaffected.

New global flags: `--color <auto|always|never>` (default
`auto`, follows the grep/ls convention — TTY + `NO_COLOR`
honoured) and `--working-hours <START-END>` (default `9-18`,
24-hour clock, start inclusive / end exclusive).

### sandogasa-hattrack: local time and weekend signal in `discourse`

The `discourse` subcommand now derives the user's local time
from their Discourse-set IANA timezone, names the country (via
tzdb's `zone1970.tab`), and flags whether it's currently the
weekend there. Weekends default to Sat+Sun with overrides for
the MENA Fri+Sat block, Iran (Fri), Nepal (Sat), and a few
others. JSON output gains a `local_time` object alongside the
existing `timezone`/`location` fields.

A bundled copy of `zone1970.tab` ships with the crate so the
lookup works on systems without tzdata installed. By default
the system file at `/usr/share/zoneinfo/zone1970.tab` wins as
long as it's at least as new as the bundled copy; otherwise the
bundled one is used and a one-line `info:` is logged. The new
global flag `--tz-source <auto|system|bundled>` forces a
choice.

### poi-tracker: `triage-retired` subcommand

Close open release-monitoring bugs for any inventoried package
that's retired on a dist-git branch. For each package the
command checks Pagure for a `dead.package` marker on
`--branch` (default `rawhide`); when present, every open bug
that `triage-updates` would touch is closed as
`CLOSED/CANTFIX` with a short comment naming the package and
branch.

The branch also scopes the Bugzilla search — `--branch
rawhide` closes `Fedora`/rawhide bugs, `--branch epel10`
closes `Fedora EPEL`/epel10 bugs — so EPEL retirements clear
the right tracking bug. `--package <name>` scopes the run to a
single package (handy for testing); `--start-from <name>` and
`--end-with <name>` bound an inclusive sub-range of the
inventory (e.g. `--start-from rust-nu-cli --end-with
rust-nu-utils` to walk every `rust-nu-*` package).
Network reads (dist-git probes, Bugzilla searches) retry up to
3 times with exponential backoff so a transient connection
blip doesn't abort the whole inventory. Findings print
per-package as each retirement is confirmed (rather than
batched at the end), followed by a one-line-per-package tally
listing the `rhbz#<id>`s about to be closed. Interactive runs
offer to claim ownership (set `assigned_to` to the configured
Bugzilla email) before applying — `--claim` skips that prompt
and is also the only way to claim under `-y`. `poi-tracker
config` now prompts for an optional Bugzilla email used for
claiming. `--dry-run` previews, `--yes` skips the confirmation
prompt.

`sandogasa-distgit` gained `DistGitClient::is_retired(package,
branch)`, a presence probe that returns `true` when the
`dead.package` marker exists on that branch and `false` on
404.

## v0.11.4

### ebranch: branch-request filing and escalation

Ports the EPEL branch-request workflow from the old Python
ebranch (issue #9). Three new Bugzilla-backed subcommands:

- `file-request <pkg> <branch>` — file one "Please branch and
  build" bug against `Fedora EPEL`/`<branch>`, falling back to
  `Fedora`/`rawhide` when the component isn't in EPEL. `--fas`
  and `--sig` add a co-maintainer offer; `--blocked`/
  `--dependson` set links (default: block the
  `EPELPackagersSIG` tracker); `--toml` records the bug ID in a
  check-crate report.
- `file-requests <report.toml> <branch>` — file requests for
  every package in a `resolve --report` closure and link them
  along the dependency graph (a package's request `depends_on`
  its dependencies' requests). IDs are written back under
  `[branch_requests]`.
- `escalate <report.toml> <branch>` — add a `needinfo?` ping to
  requests that have been NEW for ≥7 days and not yet pinged,
  marking them so they aren't pinged twice.

`resolve` gained `--report <file>`, which writes the closure
(package list + dependency edges) as a TOML the branch-request
commands consume. API key resolves from `--api-key` →
`BUGZILLA_API_KEY` → `ebranch config`. All three support
`--dry-run`.

`sandogasa-bugzilla` gained `BzClient::create` (POST
`/rest/bug`) returning a `CreateBugResponse` that surfaces
Bugzilla-level rejections (e.g. an invalid component) without
erroring, so callers can fall back to another product.

### hs-relmon: prune-tags untags testing builds not newer than release

`prune-tags` / `prune-manifest` previously untagged a
`-testing` build only when the exact same NVR was present in
the sibling `-release` tag. It now untags any testing build
whose version is *not newer* than the latest release build —
covering older leftovers in testing, not just the promoted
build. Strictly-newer testing builds are still subject only to
the keep-N retention rule.

### hs-relmon: `review` subcommand

Interactively review builds in Hyperscale `-testing` tags,
modeled on `fedora-easy-karma`. For each build it shows the
build metadata and the currently-released NVR for comparison,
then prompts:

- `+1` promotes — tags into the sibling `-release` tag and
  untags from `-testing`.
- `-1` rejects — untags from `-testing`.
- `0` / `s` / Enter skips; `q` / Ctrl-D stops.

Changelog display is scoped to what changed: for a build whose
package is already in release, only the changelog entries newer
than the released build are shown; for a brand-new package
(nothing in release yet) the changelog is capped at
`--changelog-lines` (default 20). If a testing build is not
newer than the released build (same version already released,
or a downgrade), review prints a warning rather than acting —
pruning the stale testing tag is `prune-tags`' job.

`hs-relmon review` with no argument walks every build in
testing; a package name reviews its latest build per testing
tag; an NVR reviews that specific build. `--repositories`
selects which repos to scan (default `main`); `--skip`
(repeatable or CSV) excludes packages with their own release
pipeline (e.g. systemd) and wins over an explicit target;
`--dry-run` lists the builds and exits.

`sandogasa-koji` gained `tag_build` (sibling of `untag_build`)
and `build_info_with_changelog`. `tag_build` passes `--wait`
explicitly — koji defaults to `--nowait` off a TTY, which would
let promote untag a build from testing before the release tag
landed, briefly leaving it in neither tag.

## v0.11.3

### ebranch: check-update no longer trusts a stale @testing snapshot

`check-update` previously fell back to `@testing` for "new"
provides as soon as that repo returned *any* subpackage for the
source — even when the Bodhi update was still `pending` and
`@testing` actually carried the previous V-R. The diff against
stable was then empty, hiding removed-subpackage cases like a
default-feature rename (e.g. `rust-libmimalloc-sys` flipping
its default from v2 to v3, where `+v3-devel` is replaced by
`+v2-devel`).

Two gates now guard the `@testing` path:

- For Bodhi-alias input, the update's status must be
  `testing`. Anything else (typically `pending`) skips
  `@testing` and uses the build side tag instead.
- `@testing` must report at least one subpackage whose
  `(version, release)` matches one of the input NVRs.

When either gate fails, `check-update` falls through to the
side-tag comparison as before, so reports for pending updates
correctly surface removed provides.

`sandogasa-fedrq` gained `Fedrq::subpkgs_nvrs(srpm)` returning
`Vec<(name, version, release)>`, used by the new gate.

### ebranch: check-update flags stale side-tag repodata

When the side-tag comparison path runs, `check-update` now
cross-checks each koji NVR against the V-R that the side tag's
repodata actually serves. A mismatch means
`compute_changed_provides_via_koji` would diff stable's provides
against an *old* V-R inherited from the parent tag, silently
dropping affected reverse deps from the report. Concrete case:
FEDORA-2026-7db4114930 listed `rust-mimalloc-0.1.50-1.fc44`,
but the side-tag repodata still returned `0.1.48-2.fc44`, so
`crate(mimalloc) = 0.1.48` never landed in `changed_provides`
and `rust-nu` (a real reverse dep) was missed.

The previous `check_side_tag_staleness` only verified that
*some* provides existed for the binary RPM names — it didn't
notice when those provides came from the previous V-R.

New report field `stale_side_tag: Vec<StaleSideTag>`
(`{ package, expected_nvr, actual_vr? }`) surfaces each
mismatch in both the JSON and human output. When non-empty the
report prints a prominent banner asking the user to run
`koji regen-repo` on the side tag and rerun with `--refresh`
(the latter clears fedrq's smartcache, which would otherwise
keep serving the old metadata).

`sandogasa-fedrq` gained `Fedrq::pkg_nvrs(name)` returning
`Vec<(name, version, release)>` for the per-binary lookup.

### hs-relmon: prune-tags untags promoted builds from -testing

`prune-tags` (and `prune-manifest`) now queue any build that
appears in *both* a `-testing` tag and the sibling `-release`
tag for untagging from `-testing`, in addition to the existing
keep-N-newest retention rule. Once a build is promoted to
release, leaving its `-testing` copy in place only adds noise
to `list-tagged` output. Sibling matching is on the literal
tag-name prefix, so `main-testing` pairs with `main-release`
and `facebook-testing` pairs with `facebook-release` — there's
no cross-repository attribution.

## v0.11.2

### sandogasa-report: history-based Koji activity reporting

Koji CBS reporting now walks `koji list-history` events
across the reporting window instead of diffing two snapshots
at the window's boundaries. Snapshot-diff missed any package
that was tagged and untagged entirely within the window;
history-walking captures every "tagged into" event so that
activity surfaces even when the net effect is invisible at
the start/end.

`sandogasa-koji` gained `tag_history(tag, profile, after,
before)` returning `Vec<TagAddEvent>` plus a public
`parse_tag_history` helper for the line-by-line parser.

No JSON shape changes; same `KojiReport` / `PackageEntry` /
`ChangeKind` surface.

### sandogasa-inventory: `Priority` enum + per-package and per-workload fields

New `Priority` enum (`unspecified` / `low` / `medium` / `high`
/ `urgent`, ordered so `max(…)` picks the most important). New
optional fields:

- `Package.priority: Option<Priority>` — explicit override.
- `WorkloadMeta.default_priority: Option<Priority>` —
  workload-level default.

New method `Inventory::priority_for(name)` resolves the value
for a package: per-package field wins outright (including
`unspecified` as an explicit opt-out); else the max
`default_priority` across every workload listing the package.
Both fields serialize to lowercase TOML strings.

### poi-tracker: `triage-updates` and `config` subcommands

`triage-updates` raises the Bugzilla priority on
release-monitoring bugs for inventoried packages whose
resolved priority is set. For each such package, queries OPEN
bugs reported by `upstream-release-monitoring@fedoraproject.org`
against `Fedora` and `Fedora EPEL` and updates any whose
current priority is `unspecified` — leaving already-triaged
bugs alone. `--dry-run` previews; otherwise prompts unless
`--yes`.

`config` walks an interactive Bugzilla API-key setup mirroring
`ebranch config`. Storage at `~/.config/poi-tracker/config.toml`
with restricted perms; lookup order at runtime is `--api-key`
→ `BUGZILLA_API_KEY` env → config file.

### hs-relmon: `prune-tags` / `prune-manifest` subcommands

Untag old hyperscale builds, keeping the N newest in each
`-release` / `-testing` tag. Enumerates the candidate managed
tags (cross product of EL version × repository × stage), calls
`listTagged` once per candidate for the package, and emits
`koji untag-build` calls for everything past the retention
threshold. Per-tag progress is printed with `--verbose`.

Defaults: 2 builds kept per `-release` tag, 1 per `-testing`.
`--repositories main` is the default repository filter;
`--repositories main,facebook` opts into additional channels.
Output is a per-tag breakdown listing both the builds that
will stay tagged and the ones to be untagged, so the user can
sanity-check before confirming. `--dry-run` previews without
acting; without it, prompts per package unless `--yes`.
`prune-manifest <path>` walks every package in the manifest
with the same options, and accepts `--skip <list>` to exclude
packages that manage their own tag cleanup (e.g. systemd).

`-candidate` and tags whose repository isn't in
`--repositories` are not touched.

## v0.11.1

### sandogasa-report: tags and releases on both forges

`GithubReport` and `GitlabReport` gained `tags_pushed` and
`releases_published` fields, with matching summary lines and
detailed `### Tags pushed` / `### Releases published`
sections.

GitHub tag detection walks each touched repo's tag refs via
the Git Refs API and resolves annotated tag objects to check
the tagger date and identity. The user-events stream alone
can't carry tag info: `git push --follow-tags` folds the tag
creation into the PushEvent (which only lists the branch
ref), so a release-tag push doesn't surface as `CreateEvent`.
Match heuristic: tagger.date in the window AND tagger.name or
tagger.email matching the user's GitHub profile name/email
(case-insensitive). Lightweight tags are skipped — they carry
no tagger metadata. GitHub Releases stay on events
(`ReleaseEvent` with `action == "published"`).

GitLab tag detection is two-stage. The events stream tells us
which projects had any user tag push, but the events
themselves can omit per-tag names (a `git push --tags` of N
tags fires one event with `ref_count: N` and `ref: null`) and
GitLab's `tag.created_at` follows the tagger date for
annotated tags rather than the push time, so a batch of tags
created locally across several days but pushed at once
doesn't cluster around the event timestamp. So for every
project where the user pushed any tag, we list the project's
tags and include all entries with `created_at` in the window.
GitLab Releases come from a per-project query against
`/projects/:id/releases`, filtered to releases authored by
the user and released inside the window.

`sandogasa-github` gained `GitTagRef`, `GitObject`,
`AnnotatedTag`, `Tagger` types plus `Client::list_tag_refs`
and `Client::get_annotated_tag`. `User` gained `name` and
`email` fields (both optional). `sandogasa-gitlab` gained
`Tag`, `Release`, `ReleaseAuthor`, `ReleaseLinks` types plus
`list_tags` and `project_releases`.

## v0.11.0

### New: sandogasa-github library crate

Minimal blocking GitHub REST client scoped to what sandogasa
tools need for activity reports: token validation, user
identity lookup, paginated user events, the Search Issues API
for pull requests, and per-repo authored-commit counts. Mirrors
`sandogasa-gitlab` in shape so downstream tools can treat the
two forges structurally the same.

Surface:

- `Client::new(base_url, token)` with `Accept:
  application/vnd.github+json`, `X-GitHub-Api-Version:
  2022-11-28`, and a 120s request timeout.
- `validate_token` — three-state return
  (Ok(true)/Ok(false)/Err) distinguishes rejected creds from
  transport errors.
- `user_by_username` — Ok(None) on 404 so callers can recover.
- `search_pull_requests(query)` — paginated over Search Issues
  up to GitHub's 1000-item cap.
- `user_events(username)` — paginated up to GitHub's
  300-event/3-page cap.
- `count_authored_commits` — treats 404/409 as "no commits" so
  an empty/gone repo doesn't abort the run.

DEVELOPMENT.md captures the design choices that aren't obvious
from the code.

### sandogasa-report: GitHub activity reporting

New data source mirroring GitLab. Each domain can declare
`[domains.<name>.github]` with an `instance` URL (defaults to
`https://api.github.com`) and an optional `org` prefix; the
tool queries the user's PRs (opened / merged / reviewed /
commented on) via the Search Issues API, then walks user
events to find touched repos and counts authored commits per
repo. Rendered as `## GitHub (<domain>)` sections alongside
GitLab.

Profile schema gained `[users.<key>.github]` for per-instance
GitHub usernames, and the overlay gained `[github_tokens]` for
persisted PATs. `--no-github` skips the queries.

Authentication: `GITHUB_TOKEN_<HOSTNAME>` env var (e.g.
`GITHUB_TOKEN_API_GITHUB_COM`) → generic `GITHUB_TOKEN` (the
same name the `gh` CLI uses) → overlay `[github_tokens]`.

`sandogasa-report config` now walks GitHub identities and
tokens in addition to GitLab. The token prompt uses the new
`sandogasa-github::validate_token`'s three-state return so a
saved-but-unreachable token isn't mistaken for an invalid one
and re-prompted needlessly.

GitHub ships with authored-commit counting only for v1;
mirror-pusher detection (analogous to GitLab's `commits_pushed`
vs `commits_authored` split) is deferred — see
`tools/sandogasa-report/TODO.md` for the rationale.

### sandogasa-report: authored-commit count alongside pushed (breaking JSON)

GitLab's push events credit every commit in a push to the
pusher, so a single `git push --mirror` of someone else's repo
can wildly inflate the numbers. Sync now cross-checks with
`/projects/:id/repository/commits?author=<user>` and reports
both:

    - **Commits pushed:** 193 across 6 project(s)
    - **Commits authored:** 14

In detailed mode, the per-project breakdown shows both side by
side so a mirror is obvious at a glance:

    - `CentOS/Hyperscale/rpms/kernel`: 0 authored / 187 pushed
    - `CentOS/Hyperscale/rpms/perf`:   12 authored / 14 pushed

Cost: one additional API call per unique project the user
pushed to.

JSON shape change: `GitlabReport.commits_by_project` is renamed
to `commits_pushed`; a sibling `commits_authored` map is added.

`sandogasa-gitlab` gained `count_authored_commits` as a reusable
primitive.

### sandogasa-report: user profiles (breaking)

Replaces the old `[users] <fas> = "<email>"` map and the
`[domains.X.gitlab].user` override with first-class user
profiles. One profile represents a single person and ties
together their per-service identities — FAS login, Bugzilla
email, and GitLab usernames per instance:

```toml
[users.michel]
fas = "salimma"
bugzilla_email = "michel@example.com"

[users.michel.gitlab]
"gitlab.com" = "michel-slm"
"salsa.debian.org" = "michel"
```

`sandogasa-report report --user michel` resolves the profile
once and each backend picks the right username:

- Bugzilla / Bodhi / Koji: `profile.fas` (or the profile key if
  unset)
- GitLab on `<host>`: `profile.gitlab[<host>]` → `profile.fas` →
  raw `--user`

Unknown `--user` values still work — they're treated as a raw
FAS login for back-compat with scripts that don't use profiles.

`sandogasa-report config` now walks through: profile key
(showing existing profiles), FAS username, Bugzilla email,
per-instance GitLab usernames, per-instance tokens. Every value
has a default (the current one) so re-running with Enter
presses keeps everything in place.

Breaking changes:

- `[users] <fas> = "<email>"` → `[users.<profile>]
  bugzilla_email = "<email>"`
- `[domains.X.gitlab].user` is dropped — move to
  `[users.<profile>.gitlab].<host>`

### sandogasa-report: persisted GitLab tokens

`sandogasa-report config` now prompts for a GitLab API token per
unique instance after the username round and saves them to the
overlay under `[gitlab_tokens]` keyed by hostname (e.g.
`"gitlab.com" = "glpat-…"`). Existing tokens are validated on
re-run and kept if still working. The overlay file is written
with 0600 permissions.

Token lookup order: `GITLAB_TOKEN_<HOSTNAME>` env var →
`GITLAB_TOKEN` env var → `gitlab_tokens.<host>` from the
overlay. Env vars win over config so a one-shot shell override
still works with a persisted token.

### sandogasa-report: `report` and `config` subcommands (breaking)

CLI restructured to a subcommand shape, matching ebranch,
cpu-sig-tracker, and other sibling tools. Existing invocations
of the form `sandogasa-report -c … -d …` now need a leading
`report`: `sandogasa-report report -c … -d …`. New subcommand
`sandogasa-report config` walks each GitLab-enabled domain from
the main config and prompts for the per-user username override,
writing the result to the overlay at
`~/.config/sandogasa-report/config.toml` while preserving any
other keys the user added manually.

### sandogasa-report: per-user config overlay

Configuration is now layered. The `-c` main config holds the
shared structure (domains, groups, koji tags, GitLab instance
URLs) and can be checked in; a per-user overlay at
`~/.config/sandogasa-report/config.toml` is auto-loaded when
present and deep-merged on top, so personal settings (GitLab
usernames, Bugzilla emails, any override) stay out of the
sharable file. Tables merge recursively; scalar and array values
are replaced wholesale by the overlay.

### sandogasa-report: GitLab activity reporting

New data source. Each domain can declare
`[domains.<name>.gitlab]` with an `instance` URL and an optional
`group` prefix; the tool fetches the user's activity events on
that instance, filters by group, and renders a `## GitLab
(<domain>)` section (bare `## GitLab` for single-domain runs).

Reported activity:

- MRs opened, merged, approved, commented on (dedup per MR)
- Commits pushed, summed per project

`--no-gitlab` flag to skip. Authentication: instance-specific env
var `GITLAB_TOKEN_<HOSTNAME>` (e.g. `GITLAB_TOKEN_GITLAB_COM`,
`GITLAB_TOKEN_SALSA_DEBIAN_ORG`) with fallback to generic
`GITLAB_TOKEN`. Lets a single run cover multiple GitLab instances
(gitlab.com + salsa.debian.org, etc.).

Each `[domains.<name>.gitlab]` block may set a `user` override
for cases where the GitLab username differs from the CLI/FAS
username (e.g. FAS `salimma` vs gitlab.com `michel-slm` vs salsa
`michel`). If unset, the CLI `--user` value is used.

`sandogasa-gitlab` gained the supporting primitives:
`user_by_username`, `user_events` (paginated), `project_summary`,
plus `User`, `Event`, `EventNote`, `EventPushData`, and
`ProjectSummary` types.

### hs-meetings: year headings at `###` level

The tool-managed meetings list is included underneath the docs'
`## Meeting minutes` parent heading, so year sections now render
as `### YYYY` instead of `## YYYY`. Fixes the sidebar indent in
mkdocs-material, where `## YYYY` sections sat at the same level
as `## Meeting minutes` and visually detached from it.

### sandogasa-bodhi: paginate `updates_for_user`, date filter, timeout (breaking)

`updates_for_user` used to fetch the full result set in a
single `rows_per_page=500` call, which Bodhi routinely needed
45s to serve and would sometimes hang entirely with no
client-side timeout. Reworked:

- Paginate at `rows_per_page=100` and invoke a caller-supplied
  `on_page` closure `(page, total_pages, running_count)` per
  response, so tools can stream progress to the user instead
  of waiting in silence.
- Accept optional `submitted_since` and `submitted_before`
  `NaiveDate` bounds that map to Bodhi's server-side filter.
  Activity reports no longer walk past the window just to
  discard everything client-side.
- `BodhiClient::new()` / `with_base_url` now build the reqwest
  client with a 120s per-request timeout so a truly hung
  connection fails loudly instead of blocking forever.

Also added `display_name` and `notes` to the `Update` model.
`title` on the API is the space-joined NVR list; the
human-readable heading users see in the Bodhi UI comes from
`display_name` (when set) or the first line of `notes`.

Breaking: `updates_for_user` signature gained
`submitted_since`, `submitted_before`, and `on_page` params.

### sandogasa-report: two-level `--detailed` Bodhi, progress, date window

`--detailed` is now a count flag — passing it twice
(`--detailed --detailed`) opts into a second detail level. All
formatters take a `detail: u8`; only Bodhi uses level 2 today,
the rest treat `>=1` uniformly.

Bodhi rendering at level 1:

    - [alias](url) (status, date)
      Latest `selinux` crates (8 builds)

The summary comes from `display_name` when set, else all
bullet-list lines of `notes` (preserving the full CVE list
when present), else the single build NVR when the update only
has one. Bullet-prefix markers (`- `, `* `, `+ `) are stripped
from each line. Level 2 additionally emits every build NVR as
an indented sub-bullet. Single-build updates also get the
sub-bullet at level 1.

Tool-side Bodhi fetch updates:

- Hands `(since - 30 days, until + 1 day)` to
  `updates_for_user` so Bodhi narrows server-side; 30-day
  buffer catches submissions that pushed inside the window.
- Wires the `on_page` callback to eprintln! when `--verbose`,
  so a long fetch streams progress per page.

Also adds DEVELOPMENT.md design notes covering the
commits-pushed/authored reasoning, event-endpoint half-open
date windows, overlay editing strategy, and future-work
section.

### sandogasa-report: trailing blank on Koji non-detailed output

Koji's summary mode (no `--detailed`) only emitted a single
trailing newline, so a following `## GitLab (…)` heading
rendered rammed up against it. Now matches the
detailed/empty paths by ending with `\n\n`.

### sandogasa-report: GitHub reviewed/commented from events, not search

The Search Issues qualifiers `reviewed-by:` and `commenter:`
match any PR the user has ever reviewed or commented on,
filtered by the PR's own timestamps — so a PR last updated by
someone else inside the window would surface even when the
user's only interaction with it was years ago. Switched to
walking the user-events endpoint (PullRequestReviewEvent,
IssueCommentEvent, PullRequestReviewCommentEvent) and
filtering on the event timestamp itself, so each entry is a
review or comment actually authored by the user in the
reporting window. See `tools/sandogasa-report/TODO.md` for the
300-event ceiling this introduces.

### ebranch: fix bogus installability issues for caps with parens

`extract_capability_names` trimmed trailing `)` from every dep,
even when the `)` was part of the capability name (e.g.
`libc.so.6(GLIBC_2.34)(64bit)` → `libc.so.6(GLIBC_2.34)(64bit`,
missing final paren). The corrupted cap then failed fedrq
lookup, surfacing as a "missing" provide for nearly every
system library. Wrapping parens are now stripped only when the
entire dep is itself a rich/boolean expression.

### sandogasa-report: per-domain Koji sections

Multi-domain runs (e.g. `--domain hyperscale --domain proposed_updates`)
now render one `## Koji CBS (<domain>)` section per domain instead of
merging all Koji activity into a single `## Koji CBS` block. Single-
domain runs keep the bare `## Koji CBS` heading. Bodhi and Bugzilla
sections are unchanged — Bugzilla still runs once across the unioned
Fedora versions, and Bodhi still merges since its release keys are
orthogonal across domains.

The JSON shape changes: `report.koji` is now an object keyed by
domain name (`{"hyperscale": {...}, "proposed_updates": {...}}`)
instead of a single `KojiReport`. The key is omitted when no domain
reports Koji activity.

## v0.10.2

### New: hs-meetings tool + sandogasa-meetbot library

CentOS Hyperscale SIG meeting archive helper. `hs-meetings
list` queries meetbot.fedoraproject.org for meetings whose
topic matches `centos-hyperscale-sig` (overridable) and prints
them as a table (date + stacked summary/logs URLs) or `--json`.
Supports calendar filters via `--period 2026Q1` (or `YYYY`,
`YYYYH1`) and explicit `--since` / `--until`.

`hs-meetings sync --file PATH` fetches from meetbot, deduplicates
against entries already in the target file (matching by date),
and inserts missing entries into the correct `## YYYY` section in
reverse-chronological order. New year sections are created
newest-first. Meetings from 2023 and earlier are dropped before
insertion — those predate meetbot and often carry hand-curated
`[agenda](...)` links, so legacy sections stay untouched. New
entries are rendered without an `agenda,` prefix (no SIG meeting
has had an external agenda link since January 2023). `--dry-run`
previews the change without writing. The target file is intended
to be a tool-managed partial pulled into `meetings.md` via
`pymdownx.snippets`.

Meetbot sometimes records multiple `!startmeeting` fragments on
a single day (same channel when the first attempt wasn't closed
cleanly, or across two rooms if the session was moved). sync
collapses all same-day entries by fetching the log HEAD for each
candidate and keeping the longest one, printing a warning with
the kept and dropped URLs. The SIG only ever runs one meeting
per day, so the longest log is taken as canonical.

`sandogasa-meetbot` gained `Meetbot::content_length` (HEAD-based
byte count) and `dedup_by_longest_log` (the grouping utility
used by sync) as reusable primitives.

Backed by a new `sandogasa-meetbot` library crate that wraps
meetbot's `/fragedpt/` search endpoint behind a typed blocking
client.

### sandogasa-cli: shared date-range helpers

`sandogasa-cli::date::{parse_period, resolve_date_range}`
extracted from sandogasa-report so hs-meetings can share the
same `--since/--until/--period` grammar. sandogasa-report
switched to the shared implementation; the grammar is
unchanged (`YYYY`, `YYYYQ1..Q4`, `YYYYH1..H2`).

### New: cpu-sig-tracker tool

Track CentOS Proposed Updates SIG package state across Koji,
GitLab, and JIRA. Manages the full lifecycle of each tracking
issue — filed when an MR against CentOS Stream exists, watched
until JIRA closes or Stream catches up, then retired and
untagged.

Subcommands:

- `config` — interactive GitLab + JIRA token setup
- `dump-inventory` — enumerate `proposed_updates<N>s-packages-main-release`
  contents into a sandogasa-inventory TOML; `--prune` drops
  packages no longer tagged in either `-release` or `-testing`
- `file-issue` — file a standardized tracking issue for an MR;
  auto-extracts package / release / JIRA key from the MR,
  applies labels, transitions work-item status to In progress,
  stamps start_date from Koji build creation time
- `retire` — close a tracking issue after verifying JIRA
  resolved + build untagged; mirrors JIRA resolution to
  GitLab (Done vs Won't do), stamps due_date, leaves an
  audit-trail comment
- `status` — per-package report with JIRA state + Koji/Stream
  NVR compare + suggested action; `--refresh` reconciles body
  format, work-item status, and start/due dates against live
  data; `--include-closed` extends the refresh scan to
  historical issues; `--package` and `--release` narrow the
  scan
- `sync-issues` — gap analysis per (release, package):
  active / proposed / missing classification
- `untag` — remove a proposed_updates build from both
  `-release` and `-testing` after verifying JIRA resolved;
  accepts either a package name or a specific NVR

Issue bodies follow a canonical markdown format so the read
side can parse back what the write side wrote; work-item
status, `start_date`, and `due_date` go via GraphQL since the
REST `PUT /issues` endpoint ignores them for work items.

### New: sandogasa-jira library crate

Minimal Red Hat JIRA REST client — issue lookup with
status / resolution / resolution date. Used by cpu-sig-tracker
to drive the retire and status flows.

### cov

- Raised the workspace line-coverage gate from 75% to 80%.
- Excluded `src/main.rs` files from the measurement — they're
  structurally 0% (the harness doesn't invoke main()) and the
  logic they delegate to is exercised by module tests.

### New: sandogasa-pkg-health tool

Audit package health across a sandogasa inventory via pluggable
checks classified by cost tier (cheap / medium / expensive).
Reports persist to TOML with selective per-(package, check,
variant) update — re-running one check preserves every other
stored entry's timestamp.

- `HealthCheck` trait (id, description, cost_tier, variants, run,
  format_result)
- Cost tiers: Cheap / Medium / Expensive
- Variant-aware checks (e.g. `bug_count:f45` vs `bug_count:epel10`)
  with independent per-variant staleness
- CLI: `run`, `show`, `checks` subcommands
- `--fedora-version` and `--epel-version` (CSV + repeatable, sorted
  and deduped with duplicate warnings)
- `--max-age` for age-based selective re-run
- `--package` and check selection flags for scoped updates
- Per-package parallelism via rayon (~3.4x speedup on 44 packages)
- JSON Schema for the report format (checked in, snapshot-tested)
- MVP checks: `maintainer_count` (Cheap), `bug_count` (Medium)
- `show` subcommand: display an existing report without re-running

### New: sandogasa-bugclass library crate

Bug classifier extracted from `sandogasa-report` into a shared
library so `sandogasa-pkg-health` can reuse it. The `BugKind` enum
is the tracker-agnostic vocabulary (Security, Ftbfs, Fti, Update,
Branch, Review, Other); per-tracker submodules hold the
classification logic. Currently only Bugzilla is supported.

## v0.10.1

### ebranch

- `check-update`: add installability check for updated packages —
  catches missing dependencies (e.g. `comfy-table`) that would make
  subpackages uninstallable
- `check-update`: output Markdown for direct Bodhi copy-paste
- `check-update`: show repo class in report (e.g. "c10s (@epel)")
- `check-update`: fix stale side tag warning false positives
- `resolve`: verify requested packages exist on source before
  resolving (catches `--source-repo rawhide` misuse)
- Fix root README: Haskell → Hyperscale for hs-intake/hs-relmon

## v0.10.0

### ebranch

- `check-crate`: allow `-r` without `-b` for side tag repos
- `check-crate`: include dev deps in build-order edges (fixes
  incorrect phasing for packages with dev-only dependencies like
  arrow-row → arrow-cast)
- `check-crate`: add `--koji` and `--copr` output modes
- `check-crate`: include root crate as the final build phase
- `check-crate`: add `--refresh` flag
- `check-update`: add `--refresh` flag
- `resolve`: remove `--phases` flag (phases are always computed)
- `resolve`: auto-use `@koji-src:` for source RPM queries when
  `--source-repo @koji:<tag>` is given
- `resolve`: validate all configured repos on startup (catches
  nonexistent Koji repos early)
- `resolve`: reject bare `@koji:` repos as source with a clear
  error message

### poi-tracker

- **New: `sync-distgit` subcommand** — create or update an inventory
  from packages a user or group has access to on Fedora dist-git
  (Pagure). Merges new packages without overwriting existing entries.
  `--user` or `--group` mode with group-access filtering via
  `--no-groups`, `--include-group`, and `--exclude-group`
- Rename `domains` to `workloads` (matching content-resolver
  terminology)
- Workload membership is now declared at the workload level
  (`[inventory.workloads.<key>]` with a `packages` list) rather
  than inline on each package
- Per-workload metadata overrides (name, description, maintainer,
  labels) for content-resolver export
- Multi-workload export: omit `--workload` to produce one YAML
  per workload
- Rename `--domain` to `--workload` across all subcommands

### sandogasa-inventory

- Add `WorkloadMeta` struct with per-workload metadata and package
  list
- Replace `domains` with `workloads` (`BTreeMap<String, WorkloadMeta>`)
- Add `workloads_for_package()`, `add_to_workload()`,
  `workload_names()` methods
- Add JSON Schema generation via `schemars` (`json_schema()`)
- Check in schema at `data/inventory.schema.json` with snapshot test

### sandogasa-distgit

- Add `user_projects()` and `group_projects()` for listing RPM packages
  by user or group from the Pagure API
- Add `AccessGroups::contains_group()` helper

### sandogasa-pkg-acl

- Validate user/group existence before setting ACLs, replacing
  a generic 404 error with a clear message

### Workspace

- Relicense from MPL-2.0 to Apache-2.0 OR MIT

## v0.9.1

### New: sandogasa-inventory library crate

- TOML-based package-of-interest inventory data model
- Content-resolver YAML export (feedback-pipeline-workload format)
- hs-relmon manifest TOML export
- Import from legacy poi-tracker JSON format
- Domain-level defaults, private field stripping, multi-inventory merge

### New: poi-tracker tool

- Package-of-interest tracker for Fedora, EPEL, and CentOS SIGs
- Commands: add, remove, show, validate, export, import
- Multi-inventory merge for exports
- Content-resolver export defaults to {name}.yaml filename

## v0.9.0

### New: sandogasa-koji library crate

- Shared Koji CLI wrappers: `list_tagged`, `list_tagged_nvrs`,
  `build_rpms`, `parse_nvr`, `parse_nvr_name`

### New: sandogasa-report tool

- Activity reporting for Fedora, EPEL, and CentOS SIG packaging work
- **Bugzilla**: review requests submitted/completed, reviews done for
  others, CVE/security, update requests, branch requests, FTBFS/FTI
  (classified via tracker bug aliases)
- **Bodhi**: updates submitted, pushed to testing, pushed to stable,
  with per-release breakdown sorted newest first
- **Koji CBS**: new packages and version updates detected by comparing
  tag snapshots at period start/end. Per-distro version merging,
  quarterly report style output
- Multi-domain support (`-d fedora -d hyperscale`)
- `--period` flag for years (2026), halves (2026H1), and quarters
  (2026Q1), plus `--since`/`--until` for arbitrary date ranges
- `--config` for project-level config (domains, groups, users)
- `--no-bugzilla`, `--no-bodhi`, `--no-koji` to skip data sources
- Brace expansion for Koji tag patterns
- Package groups with optional descriptions for categorical reporting
- User email resolution via FASJSON (rhbzemail) or config mapping

### ebranch

- **Breaking**: remove `build-order` subcommand; merged into
  `resolve --phases`
- `--exclude` flag for resolve: treat packages as already available
  on the target
- Rename `--no-auto-exclude` to `--no-auto-exclude-install`
- Fix side tag detection: use Bodhi's `from_tag` field (was
  incorrectly reading non-existent `from_side_tag`)

### sandogasa-bodhi (**breaking**)

- Rename `from_side_tag` to `from_tag` on `Update` struct (matching
  the actual Bodhi API field name)
- Add `date_testing` and `date_stable` fields to `Update`

### sandogasa-config

- Only enforce 600/700 permissions for user config files
  (`for_tool`), not project-level configs (`from_path`)

### sandogasa-cli

- New `require_tool_with_arg` for tools that use subcommands instead
  of `--version` (e.g. `koji version`)

## v0.8.1

### ebranch

- **New: `check-pkg-reviews` subcommand** — find and link Bugzilla
  package review requests based on the dependency graph from
  `check-crate --toml`. Caches bug IDs in the TOML file, batch-fetches
  bugs for speed, and prompts before applying changes
- **New: `config` subcommand** — interactive Bugzilla API key setup,
  stored securely at `~/.config/ebranch/config.toml`
- **New: `--toml` flag for `check-crate`** — save the full analysis
  (dependencies, edges, build phases) to a TOML file for reuse by
  `check-pkg-reviews` and other tools
- **New: `--dot` flag for `check-crate`** — output the dependency graph
  in Graphviz DOT format with version labels and build-phase grouping
- check-crate now resolves default Cargo features to find optional deps
  activated by default (e.g. `lexical-write-integer` via `lexical-core`)
- check-crate dev deps included by default (`--exclude-dev` to skip),
  matching Fedora's `%check`-enabled builds
- check-crate checks all RPM provider versions, finding compat packages
  (e.g. `rust-rand0.9`). Deps satisfied by compat packages are flagged
- check-crate resolves transitive dep versions matching the parent's
  semver requirement instead of always fetching the latest
- Rename `TooOld` to `Unmet` with full available-versions list
- Rename `--include-too-old` to `--include-unmet`
- Transitive deps now carry a `status` field (`missing` vs `unmet`)
  and a `package` field (RPM source package name)

### sandogasa-config

- Config files are now saved with 600 permissions and directories
  with 700, protecting API keys similar to SSH key files
- `load()` automatically fixes permissions on existing config files

### sandogasa-bugzilla

- New `bugs()` method for batch-fetching multiple bugs in one request

### hs-relmon

- Migrate config storage to `sandogasa-config`, gaining automatic
  secure file permissions for the GitLab access token

### Workspace

- Alphabetize subcommand sections in all tool READMEs to match
  `--help` output order

## v0.8.0

### New: sandogasa-cli library crate

- Shared `require_tool()` function for checking external tool
  availability at startup with clear install hints

### ebranch

- **New: `check-crate` command** — analyze a crates.io crate's
  dependencies against a target RPM repo
  - Shows missing, too-old, and satisfied dependencies with semver
    version matching
  - `--transitive` / `-t` expands missing deps recursively with
    phased build order (topological sort)
  - `--include-dev`, `--include-optional`, `--include-too-old` to
    widen transitive expansion
  - `--exclude CRATE,...` to skip crates (e.g. criterion) from
    transitive expansion
  - Partial version resolution: `57` resolves to highest `57.x.y`,
    `57.3` to highest `57.3.y`
  - Deduped crate counts when the same crate appears with different
    dependency kinds
- **`check-update` improvements**:
  - Prefer `@testing` repo (authoritative metadata) over side tag
  - Auto-detect testing branch from EPEL side tag names and Bodhi
    release metadata
  - Warn on stale side tag repos
  - Document EPEL 10 `@testing` limitation
- Parallelize fedrq queries with rayon (~4x speedup on 4 cores)
- Check for `fedrq` and `koji` availability at startup with clear
  error messages

### hs-relmon

- Reopen closed GitLab issues with matching title instead of creating
  duplicates

### sandogasa-bodhi (**breaking**)

- Add `from_side_tag` field to `Update` struct
- Add `branch` field to `Release` struct
- Add `update_by_alias()` for single-update API lookup

### Workspace

- External tool dependency checks: tools that shell out to fedrq or
  koji now verify availability at startup
- Move tool configs to top-level `configs/` directory
- Add source file ordering convention to CLAUDE.md
- Add dependency management guidelines to CLAUDE.md

## v0.7.0

### New: sandogasa-depfilter library crate

- Shared RPM dependency filtering for cross-branch analysis
- Classifies solib symbol version deps, soname deps, and RPM-internal
  deps (rpmlib, auto, config)

### ebranch

- Auto-exclude solib symbol version deps (e.g.
  `libc.so.6(GLIBC_2.38)(64bit)`) from installability checks — removes
  the need to manually `--exclude-install glibc` in most cases
- `--no-auto-exclude` flag to disable auto-exclusion
- Use shared dep filtering from sandogasa-depfilter

### koji-diff

- Fall back to build storage HTTP download when task logs have been
  garbage collected (requires build reference, not task reference)
- Retry with exponential backoff on transient server errors (502/503/504)
- **Breaking**: `BuildInfo` struct has new public fields (`name`,
  `version`, `release`)

### hs-intake

- Use shared solib detection from sandogasa-depfilter

### Workspace

- Fix all clippy warnings across workspace
- Add clippy cleanliness rule to CLAUDE.md

## v0.6.3

### New: koji-diff tool

- Compare buildroot and build logs between two Koji builds
- Accepts Koji build URLs, task URLs, or `build:<ID>`/`task:<ID>` refs
- Resolves builds to buildArch tasks via Koji XML-RPC API
- Downloads logs using `koji download-logs` with profile support
  (koji.fedoraproject.org, cbs.centos.org, kojihub.stream.centos.org)
- Parses installed packages from the DNF transaction table in root.log
  (supports both DNF4 and DNF5)
- Color-coded version change output using Rust semver rules:
  green (same version), yellow (compatible), orange (0.x minor break),
  red (major break)
- Shows mock_output.log for dependency resolution failures, build.log
  for rpmbuild failures
- `--json` flag for machine-readable output
- `--arch` to select architecture (default: x86_64)

### New: ebranch tool

- Build dependency resolver for cross-branch package porting
  (Rust rewrite of the Python ebranch tool)
- Compute build order for porting packages between branches
- `--koji` flag for chain build command output
- `--copr` flag for batch build script generation
- `--check-install` for subpackage installability verification

### New library crates

- **sandogasa-fedrq**: wrapper for the fedrq CLI tool (RPM repo queries)
- **sandogasa-rpmvercmp**: pure Rust implementation of RPM's rpmvercmp
  algorithm with epoch-version-release comparison
- **sandogasa-gitlab**: GitLab REST and GraphQL API client
- **sandogasa-repology**: Repology package version tracking API client

### Workspace

- Unify all tool versions to use `version.workspace = true`
- Integrate hs-intake and hs-relmon into the workspace, refactored to
  use shared library crates (sandogasa-fedrq, sandogasa-rpmvercmp,
  sandogasa-gitlab, sandogasa-repology)

## v0.6.2

### sandogasa-hattrack

- Display Discourse custom status (emoji + description) and expiration
  in the `last-seen` summary

## v0.6.1

### sandogasa-mailman

- Fix sender search to check all candidate email addresses per page
  instead of exhaustively scanning all pages for one address at a time

### sandogasa-hattrack

- Fix slow mailing list lookups for users who post from a non-primary
  email address

## v0.6.0

### New: sandogasa-hattrack tool

- Look up a Fedora contributor's activity across multiple services
- Subcommands: `discourse`, `bodhi`, `bugzilla`, `distgit`, `mailman`,
  `last-seen`
- `last-seen` summary shows the most recent activity from each service,
  sorted by date
- Discourse: profile info, timezone, location, custom status with
  rendered emoji, last post/seen timestamps
- Bodhi: last update submitted and last comment/karma
- Bugzilla: last bug filed and last bug changed
- Dist-git: daily activity stats (last 7 days), last PR filed,
  actionable PRs awaiting review
- Mailing lists: recent posts across all lists via HyperKitty API
- All timestamps include relative time ("3 days ago", "in 2 hours")
- `--json` flag for machine-readable output on all subcommands
- Email discovery via FASJSON (Kerberos) with `--email` override and
  `--no-fas` to skip authentication

### New: sandogasa-discourse crate

- Discourse forum API client for user profile data
- Fetch timezone, location, custom status, last post/seen timestamps

### New: sandogasa-fasjson crate

- FASJSON (Fedora Account System) API client via `curl --negotiate`
- Kerberos ticket management: status check, renewal, interactive
  acquisition with retry on timeout
- Read Fedora UPN from `~/.fedora.upn`

### New: sandogasa-mailman crate

- HyperKitty (Mailman 3) archive API client
- Find sender by email across list archives
- Fetch recent posts by sender across all lists

### sandogasa-bodhi

- Add `updates_for_user()` and `comments_for_user()` for user activity
  queries
- Add `Comment` and `CommentsResponse` models

### sandogasa-distgit

- Add `user_activity_stats()` for daily action counts
- Add `user_pull_requests()` for PRs filed by a user
- Add `user_actionable_pull_requests()` with pagination-aware total
  count
- Add `PullRequest`, `PullRequestsResponse`, and `Pagination` models

## v0.5.0

### fedora-cve-triage

- Add `cross-ecosystem` command to detect CVEs misattributed across
  ecosystems (e.g. JavaScript CVE filed against a Rust package with a
  similar name)
- Ecosystem detection from Fedora package names (`rust-*`, `nodejs-*`,
  `python-*`) with spec file fallback for ambiguous names
- Validate Bugzilla API key in `config` command via `valid_login` endpoint

### sandogasa-bugzilla

- Add `valid_login()` method for API key validation

### sandogasa-distgit

- Add `Ecosystem` enum and ecosystem detection functions
  (`is_js_package`, `is_rust_package`, `is_python_package`,
  `detect_ecosystem`) with quick name-based and full spec-based modes

### sandogasa-nvd

- Add NVD reference URL parsing (`CveReference`, `github_repos()`)
- Add `has_npm_references()` for detecting JavaScript packages via
  npmjs.com URLs
- Add npmjs.com reference check as 4th strategy in `targets_js()`
- GitHub repo language detection fallback for cross-ecosystem command

## v0.4.0

### New: sandogasa-pkg-acl tool

- View and manage Fedora package ACLs via the Pagure dist-git API
- Subcommands: `show`, `set`, `remove`, `apply`, `give`, `config`
- Batch ACL application from TOML config files across multiple packages
- `--strict` flag to downgrade access when target already has higher level
- Access checks: require admin for modifications, owner for transfers
- Owner protection: cannot downgrade or remove a package owner
- Username caching to avoid repeated token verification
- `--json` flag for machine-readable output on all subcommands

### New: sandogasa-config crate

- Shared config file management (`ConfigFile`) and interactive prompting
  (`prompt_field`) extracted from fedora-cve-triage for reuse across tools
- Email address validation helper

### sandogasa-distgit

- ACL management: `set_acl`, `remove_acl`, `get_acls`, `get_contributors`
- Ownership transfer: `give_package` via Pagure PATCH API
- User validation: `user_exists`
- Access level model with ordering, display, serde, and `FromStr`
- Access checking with direct and group membership support
- Token verification via `/api/0/-/whoami`

### Workspace

- Centralize all dependencies in `[workspace.dependencies]`
- Add `--json` requirement for non-interactive subcommands (CLAUDE.md)

## v0.3.1

- Fix --edit-bodhi to preserve existing bug references when adding new ones
- Convert to Cargo workspace with sandogasa library crates (bodhi, bugzilla, nvd, distgit)
- Move binary crate to tools/fedora-cve-triage for multi-tool workspace layout

## v0.3.0

- Add unshipped-tools command to detect CVEs for tools not shipped in RPMs
- Add Bugzilla email to config and prompt to reassign bugs when closing them
- Support filtering bodhi-check bugs by assignee (opt-in per-user triage)
- Add global -v/--verbose flag for progress on rate-limited API queries
- Fix bodhi-check false positives from mismatched NVD products:
  - Only compare versions when NVD product matches Fedora component
  - Use fedrq RPM provides to resolve name mismatches (e.g. django → python-django3)
  - Expand [epel-all] bugs to check all active EPEL releases

## v0.2.2

- Batch Bugzilla updates to close multiple bugs in a single API request
- Update project guidelines (code style rules, revised coverage threshold)

## v0.2.1

- Fall through to description heuristics when CPE has wildcard target_sw
- Hide API key input in config command

## v0.2.0

- Add bodhi-check subcommand to detect CVE bugs already fixed in Bodhi
- Add lag-tolerant tracker blocking for late-filed CVE bugs
- Add unit tests and enforce minimum coverage threshold

## v0.1.1

- Fix license text to MPL-2.0

## v0.1.0

- Initial release
- CLI with Bugzilla product/component/assignee/status filters
- js-fps subcommand to detect JavaScript/NodeJS false positives
- Three-strategy JS detection: CPE target_sw, CNA source, description keywords
- config command for Bugzilla API key setup
- Paginated Bugzilla search results
