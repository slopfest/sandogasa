<!-- SPDX-License-Identifier: Apache-2.0 OR MIT -->

# dbranch

Propagate a Debian package across its Ubuntu/PPA branches.

A common Debian/Ubuntu packaging layout (managed with
[git-buildpackage](https://honk.sigxcpu.org/piki/projects/git-buildpackage/))
keeps the Debian packaging on one branch (e.g. `master`) and a branch
per Ubuntu release for PPA uploads (`noble`, `oracular`,
`ubuntu/questing`, …). When the Debian branch gets a new version, each
PPA branch has to be brought up to date: merge the Debian branch, fix
the (always identically-shaped) `debian/changelog` merge conflict, add
a `~<codename>+<N>` rebuild entry, and do a local scratch build.
`dbranch rebuild` automates that loop.

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

## Usage

```
dbranch fixup [<branch>...] [-C <dir>] [--dry-run] [--explain] [--quiet]
dbranch rebuild [<branch>...] [--stage <list>] [-C <dir>]
    [--source <branch>] [--nowait]
    [--refresh-chroot | --no-refresh-chroot]
    [--ppa <name> | --upload-target <host>]
    [--yes] [--include-eol]
    [--dry-run] [--explain] [--quiet]
dbranch update [<branch>] [--stage <list>] [-C <dir>]
    [--build-suite <suite>] [--nowait] [--upload-target <host>]
    [--refresh-chroot | --no-refresh-chroot]
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
archive) with no flag, or `--upload-target mentors` for a vetted
upload. `watch-ci` is described under the `push` stage.

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
  out and merged into instead — no recreation, no packaging tweaks.
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
  `--explain`.

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
