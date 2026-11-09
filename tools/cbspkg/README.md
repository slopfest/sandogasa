# cbspkg

Packaging for CentOS SIGs that build in the CentOS Build System (CBS),
in the spirit of `fedpkg` and `centpkg`, but knowing each SIG's own
conventions: where its package repos live, which branch builds into
which collection, and what its build targets and tags are called. The
SIGs known are Proposed Updates and Hyperscale (see
[sandogasa-cbs](../../crates/sandogasa-cbs/)).

A SIG's package repo is a pull mirror of the package's CentOS Stream
repo: GitLab keeps Stream's branches current, and the SIG's own
branches sit beside them. CBS builds from that repo.

`add-pkg`, `build`, `promote` and `upload-sources` act in CBS or the SIG
lookaside in your name: see
[Commands run by a person](../../README.md#commands-run-by-a-person).

## Installation

```sh
cargo install cbspkg
```

`add-pkg`, `build` and `upload-sources` need `koji`, `cbs` and
`centos-lookaside-upload-sig` (centos-packager), `centpkg`, and a CBS
login (`~/.centos.cert`, from `centos-cert`). `sig-repo` needs `git`. Creating a repo needs a GitLab token with the
`api` scope, from `GITLAB_TOKEN` or `cbspkg config`, belonging to an
account with the Maintainer role in the SIG's group.

## Commands

### `add-pkg`

```sh
cbspkg add-pkg [PACKAGE] [-C DIR] [--branch BRANCH] [--owner USER] \
    [--explain] [--dry-run]
```

Lists a package in the CBS tags its branch builds through, which a
package's first build needs: `-candidate` (where its build target puts
a build), `-testing` and `-release`, for the collection the branch
builds into. A Hyperscale Facebook branch also lists it in the
matching RHEL collection. The branch is the one checked out, or
`--branch`; the package is the one the checkout's CentOS Stream or SIG
remote is for, or the argument. A branch no SIG names, such as Stream's
`c9s` or a work branch `c10s-CVE-…`, is taken as Proposed Updates' for
that release when the checkout's only SIG remote is the Proposed
Updates repo; Hyperscale needs its own branch (`c9s-hs`, …), which
names the repository.

Tags that list the package already are left alone, so running it again
changes nothing. The owner is your CBS login (as `cbs moshimoshi`
greets it), or `--owner`; the login is checked first, and a missing or
expired `~/.centos.cert` is explained with the commands that fix it.
Listing the package is confirmed at a terminal and refused without
one; there is no option to skip the question.

`--explain` shows each `cbs list-pkgs` and `cbs add-pkg` command and
pauses before each; `--dry-run` shows them without changing anything.

### `build`

```sh
cbspkg build [-C DIR] [--branch BRANCH] [--scratch] [--owner USER] \
    [--explain] [--dry-run]
```

Builds the checked-out branch (or `--branch`, which may exist only on
the SIG's remote) in CBS, working out what `cbs build` needs from it:
the collection's build target, and the SIG repo's URL at the branch's
full commit hash:

```sh
cbs build proposed_updates10s-packages-main-el10s \
    'git+https://gitlab.com/CentOS/proposed_updates/rpms/rsync.git#e4441b13…'
```

CBS fetches the commit from the SIG's repo, so it has to be pushed
there first: `build` fetches the SIG remote and stops with the
`git push` to run when the commit is not on it. A real build needs the
package listed in the collection's tags; when it is not, `build` lists
it as `add-pkg` does, after asking. `--scratch` builds without keeping
the result, which needs no listing. A Hyperscale Facebook branch is
built for its RHEL collection too.

CBS fetches the sources from the SIG lookaside, so before building,
`build` checks for them and uploads what is missing as `upload-sources`
does, after asking; scratch builds need them too.

The build is confirmed at a terminal and refused without one, after the
CBS login is checked; the confirmation shows the exact `cbs build`
command. Once it is done, `build` names the `cbspkg promote` (and the
`cbs tag-build` it comes to) that offers it for testing. `--explain` and
`--dry-run` show each command.

### `config`

```sh
cbspkg config [--all]
```

Asks for the GitLab token, checks it with GitLab, and keeps it in the
desktop keyring (through `secret-tool`), as the entry every sandogasa
tool reads for that GitLab instance (`secret-tool lookup application
sandogasa service gitlab.com`). Without a keyring to use it goes to
`~/.config/cbspkg/config.toml` instead; a token found there is moved to
the keyring the next time `config` runs. `GITLAB_TOKEN` overrides both.

### `upload-sources`

```sh
cbspkg upload-sources [-C DIR] [--branch BRANCH] [--explain] [--dry-run]
```

Uploads the files the branch's `sources` file names to the SIG
lookaside (`git.centos.org/sources/<pkg>/<file>/sha512/<sum>/<file>`),
which is where CBS fetches a SIG build's sources, not CentOS Stream's
lookaside. The `sources` file is read from the branch's commit, and
the lookaside is asked for each file first, so files already there are
left alone and a second run does nothing.

A file the checkout lacks, or has with another sum, is fetched with
`centpkg sources`, from CentOS Stream's lookaside. That fetches the
checked-out branch's files, so when the branch is not checked out,
`upload-sources` checks it out for it (tracking the SIG remote's when
there is no local branch) and switches back afterwards, whether or not
the fetch worked; the fetched files are untracked, so they stay. A
checkout with uncommitted changes is not switched: that stops it with
what to do instead. Every file is checked against its sha512 before
`centos-lookaside-upload-sig -f <file> -n <pkg>` uploads it. The
upload is confirmed at a terminal, showing each command, and refused
without one. `--explain` and `--dry-run` show each step's command.

### `promote`

```sh
cbspkg promote [NVR] [-C DIR] [--branch BRANCH] [--to testing|release] \
    [--explain] [--dry-run]
```

Moves a build to its collection's next stage: from `-candidate` to
`-testing`, from `-testing` to `-release`. The build is the NVR given,
or the package's newest in `-candidate` for each collection the branch
(checked out, or `--branch`) builds into; `--to` names the stage when it
is not the next one, such as `--to release` straight from `-candidate`.

Promoting to `-release` also untags the build from `-testing`, so a
released build is not left in testing as well. A build released before
that, and still in `-testing`, is promoted again to finish the untag.

Each `cbs tag-build` and `cbs untag-build` is shown in the confirmation,
which is asked at a terminal after the CBS login is checked, and
refused without one. `--explain` and `--dry-run` show each step's
command.

### `sig-repo`

```sh
cbspkg sig-repo <mr-url> [-C DIR] [--explain] [--dry-run] [--yes]
cbspkg sig-repo <package> --sig hyperscale|proposed-updates [--dry-run] [--yes]
```

Makes sure the SIG has its repo for a package. When it is missing, the
repo is created in the SIG's group (`CentOS/proposed_updates/rpms`,
`CentOS/Hyperscale/rpms`), public. When CentOS Stream has the package
(always, for a Proposed Update), the repo is made a pull mirror of
`redhat/centos-stream/rpms/<package>`'s release branches (those
matching `^c[0-9]+s$`, not its topic or automation branches): a branch
the SIG adds is never overwritten, and mirror updates start no
pipelines, and `sig-repo` waits for GitLab's first mirror update. A
Hyperscale package Stream does not have, one tracking Fedora or a new
one, gets an empty repo, with no branch pushed to it. When the repo
exists already, `sig-repo` checks that its mirror takes only the
release branches, and offers to set that when it takes more (branches
it copied already are left for you to delete). Creating or changing
the repo is confirmed at a terminal; `--yes` skips the question.

A checkout's remotes are known by the project they point at, not by
name: CentOS Stream's repo, a SIG's, or a personal fork
(`<user>/centos_rpms_<package>`), so `origin` can be any of them. A
remote `sig-repo` adds is named after the SIG (`proposed_updates`,
`hyperscale`); in a checkout of the package, it adds the SIG's repo
when no remote points at it.

Given a CentOS Stream merge request, a Proposed Update, it then
carries the change to the SIG's repo, from the dist-git checkout (the
current directory, or `-C`):

1. The SIG's repo is added as the remote `proposed_updates`, unless a
   remote already points at it.
2. `git fetch <stream repo> refs/merge-requests/<iid>/head` fetches the
   merge request's commits.
3. `git push --no-verify <remote> FETCH_HEAD:refs/heads/<branch>` pushes
   them as `<release>-proposed-<RHEL key>`, named after the merge
   request's target branch and the first RHEL issue it resolves,
   without running the checkout's pre-push hook (see DEVELOPMENT.md).
4. `git branch --track <branch> <remote>/<branch>` makes a local branch
   tracking it, unless one exists, without switching to it:
   `git checkout <branch>` is then the branch `add-pkg` and the other
   cbspkg commands work from.

`--explain` narrates each step, with the git commands as they would be
typed, pausing before each; `--dry-run` shows the steps without doing
anything. The GitLab steps are API requests, described in words with
where to do the same on the web.
