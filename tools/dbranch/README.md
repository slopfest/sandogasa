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

## Usage

```
dbranch rebuild [<branch>...] [--stage <list>] [-C <dir>]
    [--dry-run] [--explain]
```

Run it from the package's git working tree **with the Debian branch
checked out** (e.g. `master` or `debian/unstable`) — that branch is
the merge source. Name the PPA branch(es) to rebuild; a branch that
doesn't exist yet is created from the Debian branch. With no branches
given it rebuilds every local branch except the current one and gbp's
`upstream` / pristine-tar branches.

```
$ dbranch rebuild noble ubuntu/questing
$ dbranch rebuild noble,oracular        # repeatable or comma-separated
$ dbranch rebuild                        # all existing PPA branches
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
- **`build`** — `debuild -S -sa -d` then
  `pbuilder-dist <codename> ../<dsc>`.
- **`lint`** — `lintian -I` on the built **`.deb`s** in
  `~/pbuilder/<codename>_result/` (`-I` surfaces info-level tags too;
  linting the binaries directly, rather than the `.changes`, avoids
  lintian re-unpacking the source, which `debuild -S` already lints).
  It warns but does **not** fail the run — rebuild lint tags are
  mostly inherited from Debian.
- **`all`** — all of the above.

```
$ dbranch rebuild noble                  # merge stage only (default)
$ dbranch rebuild noble --stage all      # merge, build, lint
$ dbranch rebuild noble --stage build,lint   # build an already-merged branch, then lint
```

`<N>` is `1` for a new Debian version, bumped if you rebuild the same
version again. The Debian base version is detected even when run from
a PPA branch (a `~<codename>+<N>` suffix is stripped first).

### Learning / sanity-checking

`--explain` and `--dry-run` are separate and composable:

- `--dry-run` prints every command **without running anything** — a
  tutorial.
- `--explain` **runs** the workflow but narrates each command and
  pauses for Enter before running it (Ctrl-C aborts), so you can step
  through, learn it, or sanity-check a real run.
- `--explain --dry-run` together is a pure walkthrough.

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
<codename> create` before building.

Commands are color-coded on a terminal; color is dropped automatically
when output is piped or `NO_COLOR` is set.

Use `-C <dir>` to run against a package tree other than the current
directory.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
