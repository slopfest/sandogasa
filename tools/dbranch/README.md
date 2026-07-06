<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# dbranch

Propagate a Debian package across its downstream branches — Ubuntu PPAs,
Debian unstable, Debian stable proposed-updates, and Debian backports.

A common Debian/Ubuntu packaging layout (managed with
[git-buildpackage](https://honk.sigxcpu.org/piki/projects/git-buildpackage/))
keeps the Debian packaging on one branch (e.g. `master` or
`debian/unstable`) and further branches per downstream target. dbranch
automates the repetitive loops across them:

- **`rebuild`** — bring an **Ubuntu PPA** branch (`noble`, `oracular`,
  `ubuntu/questing`, …) up to date with the Debian branch: merge it, fix
  the (always identically-shaped) `debian/changelog` merge conflict, add
  a `~<codename>+<N>` rebuild entry, and scratch-build. The same command
  also handles a **Debian stable proposed-update** when the target is a
  `debian/<codename>` branch (e.g. `debian/trixie`) — a
  `~deb<N>u<M>` entry via `gbp dch --stable` — and a **Debian
  backport** when it is `debian/<codename>-backports` — a `~bpo<N>+<M>`
  entry via `gbp dch --bpo`; both built on a Debian host.
- **`update`** — update the Debian branch itself to a **new upstream**
  release (`gbp import-orig --uscan`), then build/lint/push/upload/tag.

It is also a **learning tool**: `--explain` runs the workflow while
narrating each step and the exact command it uses, so you can follow
along or learn to do it by hand.

## Installation

```
cargo install dbranch
```

`dbranch` shells out to the standard Debian tooling; install what the
stages you run need:

- `git` — always
- `gbp` (`git-buildpackage`) — `merge` stage
- `debuild` (`devscripts`) and `pbuilder-dist` (`ubuntu-dev-tools`) —
  `build` stage
- `lintian` (`lintian`) — `lint` stage
- `glab` (the [GitLab CLI](https://gitlab.com/gitlab-org/cli)) —
  `push` stage CI watch and `watch-ci` (skip with `--nowait`).
  Authenticate to the instance the repo lives on first:
  `glab auth login --hostname salsa.debian.org` (glab keeps a separate
  token per host, so a gitlab.com login alone won't do)
- `debusine-client` — only for `--debusine` uploads; it provides the
  `debusine.debian.net` dput profile and the `debusine setup`
  authentication

## Usage

```
dbranch fixup [<branch>...] [-C <dir>] [--dry-run] [--explain] [--quiet]
dbranch rebuild [<branch>...] [--stage <list>] [-C <dir>]
    [--source <branch>] [--nowait]
    [--refresh-chroot | --no-refresh-chroot] [--urgency <level>]
    [--ppa <name> | --upload-target <host> | --debusine <name>]
    [--yes] [--include-eol]
    [--dry-run] [--explain] [--quiet]
dbranch update [<branch>] [--stage <list>] [-C <dir>]
    [--build-suite <suite>] [--nowait]
    [--upload-target <host> | --debusine <name>]
    [--refresh-chroot | --no-refresh-chroot] [--urgency <level>]
    [--dry-run] [--explain] [--quiet]
dbranch watch-ci [<branch>] [-C <dir>] [--dry-run] [--explain]
```

`rebuild` is the main command (below). `fixup [<branch>...]` applies
the PPA-branch packaging adjustments — gbp.conf's `debian-branch` /
`debian-tag` and the salsa-ci.yml preset, the same ones the `merge`
stage makes for a new branch — to **existing** branches, to repair
ones set up before (or outside) dbranch. It checks each branch out,
adjusts, and commits what changed (idempotent; defaults to the current
branch).

`update [<branch>]` updates the **Debian** branch (`master`/`main`/
`debian/unstable`, default the current branch) to a new upstream:
`gbp import-orig --uscan --pristine-tar` then `gbp dch -c -R -D
unstable`, then the same `build → lint → push → upload → tag` tail as
`rebuild`. Unlike a rebuild the changelog is left as gbp writes it (a
real new-upstream entry — your other commits since the last release
show up as bullets, nothing is normalized away); the distribution is
pinned to `unstable` so dch's release heuristic can't substitute the
host's own (e.g. an Ubuntu devel codename). It builds against **testing** by default
(`--build-suite unstable` to switch — sometimes deps removed from
testing force it); upload goes to dput's default target (the Debian
archive) with no flag, `--upload-target mentors` for a vetted upload,
or `--debusine <name>` for a Debusine personal repository (the suite
is `sid` — the Debian branch targets unstable). `watch-ci` is
described under the `push` stage.

Both `rebuild` and `update` write the changelog entry at `medium`
urgency; pass `--urgency <level>` (e.g. `--urgency high`) to override —
useful for a security upload.

Run it from the package's git working tree **with the Debian branch
checked out** (e.g. `master` or `debian/unstable`) — that branch is
the merge source. (Use `--source <branch>` to merge from a specific
branch instead, so you needn't check it out first.) Name the PPA
branch(es) to rebuild; a branch that doesn't exist yet is created from
the Debian branch.

With **no branches given** (bulk mode), it rebuilds every local branch
whose codename is a real Ubuntu release — `noble`, `ubuntu/questing`,
etc. (looked up via `ubuntu-distro-info`) — so the Debian branch,
`master`/`main`, Debian suites (`debian/trixie`, `bookworm-backports`),
and gbp plumbing are left out. End-of-life releases are **skipped** by
default (use `--include-eol` to rebuild them locally — it can't be
combined with `upload`, since EOL PPAs reject uploads). Before doing
anything it prints the resolved set and asks for confirmation
(`[Y/n]`); `--yes`/`-y` skips the prompt, and a non-interactive run
without `--yes` is refused rather than run blind. (Bulk mode needs the
`distro-info` package.)

Bulk considers only **local** branches — a local branch is the opt-in.
To include a release in bulk runs, check it out once; to drop it,
delete the local branch (it stays on `origin`). Name it explicitly to
rebuild it without checking it out.

```
$ dbranch rebuild noble ubuntu/questing
$ dbranch rebuild noble,oracular        # repeatable or comma-separated
$ dbranch rebuild                        # all live Ubuntu PPA branches
$ dbranch rebuild --include-eol --stage build  # local rebuild incl. EOL
```

The codename is taken from an existing branch's `debian/gbp.conf`
(`debian-branch` basename), or from the branch name's basename for a
new branch (`ubuntu/<rel>` → `<rel>`).

### Stages

Like `rpmbuild`'s build stages, `--stage` selects what to run
(repeatable or comma-separated; default `merge`):

- **`merge`** — switch to (or create) the target branch, merge the
  Debian branch, resolve the `debian/changelog` conflict
  deterministically (incoming Debian entry above the existing rebuild
  entry — the `dpkg-mergechangelogs` result, committed), then
  `gbp dch --bpo -R -D <codename>` and **normalize** the new stanza to
  `<debver>~<codename>+<N>` / `* Rebuild for <codename>` and commit.
  When the target branch is **brand new**, it is created from the
  Debian branch and two one-time packaging tweaks are committed first:
  `debian/gbp.conf`'s `debian-branch` is pointed at the new branch,
  and `debian/salsa-ci.yml` gets the PPA-rebuild `variables` preset
  (`RELEASE: "unstable"` plus the backports-style relaxations). A
  branch that already exists locally or only on `origin` is checked
  out and merged into instead (no recreation). The packaging tweaks are
  re-checked on **every** merge, not just at creation — they're
  idempotent, so an already-correct branch is left untouched, but an
  unadjusted or externally-created one is self-healed (and the files it
  changed are listed in the entry).

  **Debian proposed-updates:** when the target is a `debian/<codename>`
  branch whose codename is a real Debian release (e.g. `debian/trixie`,
  via `debian-distro-info`), the merge stage instead produces a
  proposed-update: version `<debver>~deb<N>u<M>` (the `~` makes it sort
  *older* than the plain build, so it never shadows testing/unstable),
  the changelog distribution is the codename, and the command run is
  `gbp dch --stable` (still normalized to the `~` form + `* Rebuild for
  <codename>`). The one-time `salsa-ci.yml` tweak sets
  `RELEASE: "<codename>"` with **no** backports relaxations (it's a real
  stable build). This needs `debian-distro-info` (from `distro-info`),
  consulted only for `debian/`-namespaced branches. The `upload` stage
  goes to `dput`'s default target (the Debian archive) — no
  `--ppa`/`--upload-target` needed (only PPA branches require one) —
  or to a Debusine personal repository with `--debusine <name>`. A
  proposed-update must be run on a **Debian host** (`gbp dch --stable`
  needs a newer gbp, and the stable chroot / archive upload are
  Debian-only); dbranch hard-fails early otherwise, except under
  `--dry-run`.

  **Debian backports:** when the target is a
  `debian/<codename>-backports` branch (e.g. `debian/trixie-backports`),
  the merge stage produces a backport: version `<debver>~bpo<N>+<M>`
  (the official backports scheme, e.g. `2.3.0-1~bpo13+1` for trixie),
  the changelog distribution is `<codename>-backports`, and the command
  run is `gbp dch --bpo` (normalized afterward, which also drops the
  trailing period gbp puts on its `Rebuild for …` line). `gbp.conf`
  gets **only** `debian-branch` — the branch lives in the `debian/`
  namespace, so gbp's default `debian/%(version)s` tag is already right
  — and any existing settings are preserved. The one-time
  `salsa-ci.yml` tweak sets `RELEASE: "<codename>-backports"` — an
  officially supported salsa-ci release whose image also enables the
  backports apt repo — with **no** relaxations (without the pin
  salsa-ci builds against sid). The `build` stage
  scratch-builds in the **base release's** chroot (`pbuilder-dist
  trixie`, not `trixie-backports` — the suffix is a changelog
  distribution, not a pbuilder dist). Like a proposed-update, it
  uploads to `dput`'s default target — or with `--debusine <name>` to
  a Debusine personal repository, publishing to the **base** release's
  suite (`publish-to-trixie-<srcpkg>`, the official backports pattern) —
  and requires a **Debian host** (`--dry-run` exempt).
- **`build`** — `debuild -S -sa -d` then
  `pbuilder-dist <codename> ../<dsc>`.
- **`lint`** — `lintian -I` on the built **`.deb`s** in
  `~/pbuilder/<codename>_result/` (`-I` surfaces info-level tags too;
  linting the binaries directly, rather than the `.changes`, avoids
  lintian re-unpacking the source, which `debuild -S` already lints).
  lintian is quiet when clean, so its output is echoed with a
  tag-count summary. It uses lintian's default exit convention
  (non-zero on error-level tags) and propagates that status.
- **`push`** — push the branch (`git push -u origin <branch>` the
  first time, to set the upstream tracking the new remote ref didn't
  have yet; a plain `git push` once it tracks `origin/<branch>`), then
  (unless `--nowait`)
  watch the pushed commit's GitLab CI pipeline to completion. dbranch
  polls `glab ci list --sha <commit> -F json`, targeting the **exact
  commit** rather than the branch — so it can't accidentally report
  the *previous* commit's pipeline in the window after `git push`
  before GitLab has created the new one. `glab` reads the git remote
  to find the host / project (e.g. `salsa.debian.org`) itself. It
  waits until the pipeline finishes: a `failed`/`canceled` result
  makes dbranch exit non-zero; `success`/`skipped`/`manual` pass; if
  no pipeline shows up within ~3 minutes it's treated as benign
  (nothing to watch). It also polls the pipeline's jobs and prints each
  one as it finishes (`✓ build source (build)`, `✗ … — failed`).
  Before watching, dbranch checks `glab auth
  status --hostname <host>` for that instance (glab stores a token per
  host) and fails early with the `glab auth login` command to run if
  you're not logged in. `--nowait` pushes without waiting; attach to a
  running pipeline later — after a `--nowait` push or a dropped
  connection — with `dbranch watch-ci [<branch>]` (defaults to the
  current branch; it watches the branch-tip commit's pipeline).
- **`upload`** — `dput` the built source `.changes` (from
  `debuild -S`) to its archive. Give the target with `--ppa
  <user/name>` (sugar for a `ppa:<user/name>` dput target; a leading
  `ppa:` is accepted) or `--upload-target <host>` for any dput host
  (e.g. `mentors`, `ftp-master`); the two are mutually exclusive and
  one is required. Runs after `push` so CI can pass before publishing.
  **Opt-in** — not part of `all`.

  For Debian targets, `--debusine <name>` uploads to a [Debusine
  personal repository](https://wiki.debian.org/DebusineDebianNet#Repositories)
  instead: `dput -O debusine_workspace=r-<name>-<srcpkg>
  -O debusine_workflow=publish-to-<suite>-<srcpkg> debusine.debian.net
  …`, where `<suite>` is the target's **base** release — a trixie
  backport publishes to `trixie`, `update` publishes to `sid`. Needs
  `debusine-client` (the dput profile) and a `debusine setup` token;
  both are pre-flighted before any work. Ubuntu PPA targets can't use
  it (Debusine hosts Debian suites only).

  For a **PPA** target, dbranch first checks via the Launchpad API
  (`curl … getPublishedSources`) whether the package is already in that
  PPA. If not — or the PPA name can't be verified — it asks to confirm
  before uploading (default **no**), to catch a wrong/typo'd `--ppa`. A
  genuine first upload is confirmed once (like trusting a new SSH host).
  `--yes` or a non-interactive run warns and proceeds instead of
  prompting; a missing `curl` skips the check.

  > **dput over sftp:** with a `"method": "sftp"` dput profile,
  > dput-ng uploads via paramiko, which prompts to trust the host's SSH
  > key. It **reads** `~/.ssh/known_hosts` but does **not** save keys
  > you accept at the prompt, so you get re-prompted on *every* run
  > (and it ignores `~/.ssh/config`'s `StrictHostKeyChecking`, so
  > `accept-new` there won't help). Under `--quiet` the prompt can't be
  > answered — the captured `dput` has no stdin — so the stage fails.
  > Fix it once by seeding the host key into `~/.ssh/known_hosts`
  > yourself; paramiko then finds it and never prompts:
  >
  > ```
  > ssh-keyscan ppa.launchpad.net >> ~/.ssh/known_hosts
  > ```
  >
  > If the prompt names several hosts, `ssh-keyscan` each. Don't
  > disable host-key checking — it removes the MITM protection on the
  > upload.
- **`tag`** — tag the release: `dh clean` (so `gbp tag` sees a clean
  tree — `debuild -S` leaves a `debian/files`) then `gbp tag`, which
  derives the version from `debian/changelog` and gbp's `debian-tag`
  format. Runs after `upload`. **Opt-in** — not part of `all`.
- **`all`** — `merge`, `build`, `lint`, `push` (not `upload`/`tag`,
  which are deliberate publish/release steps).

```
$ dbranch rebuild noble                  # merge stage only (default)
$ dbranch rebuild noble --stage all      # merge, build, lint, push
$ dbranch rebuild noble --stage build,lint   # build an already-merged branch, then lint
$ dbranch rebuild noble --stage push --nowait   # push, don't wait for CI
$ dbranch rebuild ubuntu/questing --stage upload --ppa me/sugarjar  # dput to a PPA
$ dbranch watch-ci noble                 # attach to noble's CI pipeline
```

When a stage command fails, `dbranch` exits with that command's own
exit code (not a generic `1`), so CI sees the real status.

`<N>` is `1` for a new Debian version, bumped if you rebuild the same
version again. The Debian base version is detected even when run from
a PPA branch (a `~<codename>+<N>` suffix is stripped first).

### Learning / sanity-checking

`--explain` and `--dry-run` are separate and composable:

- `--dry-run` prints every command **without running anything** — a
  tutorial.
- `--explain` **runs** the workflow but narrates each command and
  pauses for Enter before running it (Ctrl-C aborts), so you can step
  through, learn it, or sanity-check a real run. After a step dbranch
  edits a file itself (the changelog conflict/normalization, the
  gbp.conf / salsa-ci.yml tweaks) it shows `git diff` of the change
  and pauses, so you see what it did before it's committed.
- `--explain --dry-run` together is a pure walkthrough.
- `--quiet` (`-q`) is the opposite end: it suppresses the tools'
  output, leaving only dbranch's step headings, and replays a
  command's output only if it fails. Mutually exclusive with
  `--explain`. **Caveat for `--stage build --quiet`:** `pbuilder-dist`
  runs under `sudo`, and `--quiet` captures the command's I/O — so a
  `sudo` password prompt can't be answered and the build hangs/fails.
  Set up passwordless `sudo` for `pbuilder-dist` (or pre-authenticate
  `sudo` in the same session) before a quiet build.

```
$ dbranch rebuild noble --dry-run        # on debian/unstable, damo 3.2.8-1

» noble (codename: noble)
    $ git checkout noble
    $ git merge --signoff --no-edit debian/unstable

» Resolve the debian/changelog conflict
    $ git add debian/changelog
    $ git commit -s --no-edit

» Generate the rebuild changelog entry
    $ gbp dch --bpo -R -D noble

» Normalize the entry to 3.2.8-1~noble+1 / "Rebuild for noble"
    $ git commit -s -m 'Update changelog for 3.2.8-1~noble+1 release' debian/changelog
```

The build stage (`--stage build` / `all`) creates the codename's
pbuilder chroot automatically the first time (when
`~/pbuilder/<codename>-base.tgz` is absent) with `pbuilder-dist
<codename> create` before building. When the chroot already exists but
is older than a day it is refreshed (`pbuilder-dist <codename> update`)
so the build isn't against stale packages; `--refresh-chroot` forces a
refresh regardless of age and `--no-refresh-chroot` skips it.

Commands are color-coded on a terminal; color is dropped automatically
when output is piped or `NO_COLOR` is set.

Use `-C <dir>` to run against a package tree other than the current
directory.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
