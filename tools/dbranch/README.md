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
  release (`gbp import-orig --uscan`), then
  source/build/lint/push/upload/tag.

It is also a **learning tool**: `--explain` runs the workflow while
narrating each step and the exact command it uses, so you can follow
along or learn to do it by hand.

## Installation

```
cargo install dbranch
```

Or from the prebuilt binaries attached to each release, which install
without a compile, using
[cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```
cargo binstall dbranch
```

That still needs cargo, since `binstall` is a cargo subcommand. With no
Rust installed at all, download the archive for your architecture from
the [releases page](https://github.com/slopfest/sandogasa/releases) and
put `dbranch` on your `PATH`.

The binaries are statically linked against musl, for x86_64 and
aarch64 Linux, so they run on any distribution.

`dbranch` shells out to the standard Debian tooling; install what the
stages you run need:

- `git` — always
- `gbp` (`git-buildpackage`) — `merge` stage
- `debuild` (`devscripts`) — `source` stage
- `pbuilder-dist` (`ubuntu-dev-tools`) — `build` stage
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
    [--debusine-project <project>] [--yes] [--include-eol]
    [--dry-run] [--explain] [--quiet]
dbranch update [<branch>] [--stage <list>] [-C <dir>]
    [--build-suite <suite>] [--nowait]
    [--upload-target <host> | --debusine <name>]
    [--debusine-project <project>]
    [--refresh-chroot | --no-refresh-chroot] [--urgency <level>]
    [--dry-run] [--explain] [--quiet]
dbranch watch-ci [<branch>] [-C <dir>] [--dry-run] [--explain]
```

`rebuild` is the main command (below). `clone <url> [<dir>]` starts a
package from upstream's git rather than from tarballs (gbp's "upstream
uses git" flow): it clones with upstream as remote `upstream`, starts
the Debian branch (`debian/latest`; `--debian-branch` to change) at the
newest release tag (`--upstream-version` to pick another), and commits a
`debian/gbp.conf` naming the tag style — `upstream-tag = v%(version)s`
or `%(version)s`, detected from the tags — with pristine-tar and
`pristine-tar-commit` on. When upstream itself carries a `debian/*`
branch, `clone` says who wrote it and which version its changelog is
at, and offers to start from it instead (default yes;
`--from-upstream-packaging` says yes unasked, `--fresh` ignores it, a
non-interactive run starts fresh with a warning): the branch begins at
upstream's packaging branch without tracking it, the release tag is
merged in, and their gbp.conf is completed — `debian-branch` set to
yours, `upstream-tag` and the pristine-tar keys added when absent.
Their packaging commits stay in the history, so diverging from them is
ordinary commits and their later changes can still be merged. A tag
whose tree carries a `debian/` directory gets a warning (dpkg-source
drops the orig's copy, so the two disagree). Writing the rest of
`debian/` is up to you; with no packaging anywhere the next-step
message prints the `dh_make -p <name>_<version> --createorig` to run.
Two optional steps finish the setup: `--salsa <namespace>` creates the
packaging project `<namespace>/<name>` on salsa.debian.org (public, CI
config path preset to `debian/salsa-ci.yml`) and adds it as `origin` —
nothing is pushed yet: a fresh branch holds only a gbp.conf and an
adopted upstream packaging wants a look first, so the first push is
`dbranch update --stage push` (below), which also adds the CI file;
`--mr` registers the clone with myrepos
(`mr -c ~/.mrconfig config <dir> checkout=… update=…`; `--mrconfig` for
another file) — with a salsa project as `gbp clone --all <url> && …
git remote add upstream … && git fetch --tags upstream` updated by
`gbp pull && git fetch --tags upstream`, without one as the `dbranch
clone` invocation that reproduces the setup, updated by the tag fetch.
From then on `update` merges new releases in (below). `fixup [<branch>...]` applies
the PPA-branch packaging adjustments — gbp.conf's `debian-branch` /
`debian-tag` and the salsa-ci.yml preset, the same ones the `merge`
stage makes for a new branch — to **existing** branches, to repair
ones set up before (or outside) dbranch. It checks each branch out,
adjusts, and commits what changed (idempotent; defaults to the current
branch).

`update [<branch>]` updates the **Debian** branch (`master`/`main`/
`debian/unstable`, default the current branch) to a new upstream:
`gbp import-orig --uscan --pristine-tar` then `gbp dch -c -R -D
unstable`, then the same `source → build → lint → push → upload → tag`
tail as `rebuild`. In a repository packaged from upstream's git (an
`upstream` remote — `--upstream-remote` to name another — plus
`upstream-tag` in gbp.conf, the layout `clone` leaves) there is no
tarball to import: the import stage runs `git fetch --tags upstream`,
merges the newest release tag (`--upstream-version` for another) into
the Debian branch and writes the entry with `gbp dch -N <version>-1`,
and the source stage first runs `gbp export-orig --pristine-tar
--pristine-tar-commit` to generate the orig tarball from that tag, so
the tarball is reproducible from git and never downloaded. The
upstream remote is never a push candidate. Unlike a rebuild the changelog is left as gbp writes it (a
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
delete the local branch (it stays on its remote). Name it explicitly to
rebuild it without checking it out.

**Which remote.** Each target branch is pushed to, and has its CI
configured on, its own remote — not necessarily `origin`, which in a
team project the rebuilder may not control while the rebuild branch
lives on a fork. dbranch resolves it per branch: `--remote <name>` if
given; else the remote the branch pushes to or tracks; else the one
remote already holding `<remote>/<branch>`; else the only configured
remote. When several remotes could hold a new branch it asks which (a
non-interactive run errors out and asks for `--remote`). A branch it
creates records the choice as `branch.<name>.pushRemote`, so later
stages and runs do not ask again. glab is pointed at that remote's
project explicitly, so the CI watch and the CI settings follow the
branch too.

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
  (`RELEASE: "unstable"` plus the backports-style relaxations). Either
  file missing from the source branch is created on the new branch
  instead — gbp.conf with just those keys, salsa-ci.yml from the
  upstream template (an `include:` of `recipes/debian.yml`) plus the
  preset — so the Debian branch stays untouched. A
  branch that already exists locally or only on its remote is checked
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
- **`source`** — `debuild -S -d` the source package into the parent
  directory: the `.dsc` the `build` stage scratch-builds and the
  `.changes` the `upload` stage dputs.

  Whether the orig tarball is offered for upload depends on where the
  package is going. Uploads to the Debian archive — `update` to
  unstable, proposed-updates and backports, which share one pool — use
  dpkg's own rule (`-si`): the orig ships with a new upstream version
  and is left out for the revisions after it, which the archive
  resolves from what it already has. Everywhere else it is included
  (`-sa`): a PPA, a dput host such as `mentors`, a Debusine personal
  repository. None of those can fall back on that pool, and the rebuild
  versions dbranch generates reuse the upstream version, so dpkg would
  otherwise leave the tarball out. The flag is always in the narrated
  command, so you can see which one a run chose.
- **`build`** — `pbuilder-dist <codename> ../<dsc>` — scratch-build
  the source package in the codename's chroot.

  `build` and `upload` consume what `source` produced. When `source`
  isn't part of the same run, dbranch checks the file first: if it was
  built before the current commit — or, for an upload, offers no orig
  tarball where one is needed — it offers to rerun `source` (default
  **yes**), and going ahead without it uses what is on disk. If it is
  missing altogether, dbranch instead offers to build it (default
  **no**) and otherwise stops, naming the stage to run. `--yes` accepts
  all of these, a non-interactive run takes the default (so a missing
  source package fails), and `--dry-run` skips the check.
- **`lint`** — `lintian -I` on the built **`.deb`s** in
  `~/pbuilder/<codename>_result/` (`-I` surfaces info-level tags too;
  linting the binaries directly, rather than the `.changes`, avoids
  lintian re-unpacking the source, which `debuild -S` already lints).
  lintian is quiet when clean, so its output is echoed with a
  tag-count summary. It uses lintian's default exit convention
  (non-zero on error-level tags) and propagates that status.
- **`push`** — when the branch carries a `debian/salsa-ci.yml`, first
  check the CI config path of the branch's remote project (`glab api
  --hostname <host> projects/<group%2Frepo>`): Salsa
  only runs the file when that setting points at it, and a project
  that never had the file has it unset, so the push would start no
  pipeline and setting it afterwards needs a re-push. If unset, dbranch
  offers to set it (`glab api --hostname <host> -X PUT projects/<id>
  -f ci_config_path=debian/salsa-ci.yml`, needs Maintainer; `-y` sets it
  unasked, a non-interactive run only warns with the command). A path
  set to something else (the stock pipeline) is left alone, since the
  setting is project-wide, but reported: a note on a `debian/*` branch,
  where only the `RELEASE` pin goes unused, a warning on a PPA branch,
  whose relaxations the stock pipeline lacks. Then push the
  branch to its remote (`git push -u <remote> <branch>` the first
  time, to set the upstream the new remote ref didn't have yet; a
  plain `git push` once it is configured to push there), then
  (unless `--nowait`)
  watch the pushed commit's GitLab CI pipeline to completion. dbranch
  polls `glab ci list -R <remote-url> --sha <commit> -F json`,
  targeting the **exact commit** rather than the branch — so it can't
  accidentally report the *previous* commit's pipeline in the window
  after `git push` before GitLab has created the new one — and the
  branch's remote explicitly, rather than letting glab pick one. It
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
  backport publishes to `trixie`, `update` publishes to `sid`. The
  project part defaults to the source package name, which fits a repo
  shipping a single package; `--debusine-project <project>` overrides
  it for a shared workspace hosting several packages (the wiki's
  `r-YOURNAME-PROJECTNAME` pattern). Needs
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

## System-wide configuration

This tool keeps no settings of its own, but it does read a `[defaults]`
table — for pinning the flags you always pass — from
`/etc/dbranch/config.toml` and `~/.config/dbranch/config.toml`, the user
file overriding the system one per key and command-line flags overriding
both. Either path may be absent. See the root `DEVELOPMENT.md` for the
table format.

## License

Licensed under either of

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT License](LICENSE-MIT)

at your option.
