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

## Installation

```sh
cargo install cbspkg
```

`add-pkg` needs `koji` and `cbs` (centos-packager) and a CBS login
(`~/.centos.cert`, from `centos-cert`). `sig-repo` needs `git`. Creating a repo needs a GitLab token with the
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

The build is confirmed at a terminal and refused without one, after the
CBS login is checked. Once it is done, `build` prints the
`cbs tag-build` that offers it for testing. `--explain` and
`--dry-run` show each command.

### `config`

```sh
cbspkg config [--all]
```

Asks for the GitLab token, checks it with GitLab, and saves it to
`~/.config/cbspkg/config.toml`.

### `sig-repo`

```sh
cbspkg sig-repo <mr-url> [-C DIR] [--explain] [--dry-run] [--yes]
cbspkg sig-repo <package> --sig hyperscale|proposed-updates [--dry-run] [--yes]
```

Makes sure the SIG has its repo for a package. When it is missing, the
repo is created in the SIG's group (`CentOS/proposed_updates/rpms`,
`CentOS/Hyperscale/rpms`), public, and made a pull mirror of
`redhat/centos-stream/rpms/<package>`: every branch is mirrored, a
branch the SIG adds is never overwritten, and mirror updates start no
pipelines. `sig-repo` waits for GitLab's first mirror update before it
goes on. Creating the repo is confirmed at a terminal; `--yes` skips
the question.

Given a CentOS Stream merge request, a Proposed Update, it then
carries the change to the SIG's repo, from the dist-git checkout (the
current directory, or `-C`):

1. The SIG's repo is added as the remote `sig`, unless a remote already
   points at it.
2. `git fetch <stream repo> refs/merge-requests/<iid>/head` fetches the
   merge request's commits.
3. `git push --no-verify sig FETCH_HEAD:refs/heads/<branch>` pushes
   them as `<release>-proposed-<RHEL key>`, named after the merge
   request's target branch and the first RHEL issue it resolves,
   without running the checkout's pre-push hook (see DEVELOPMENT.md).
4. `git branch --track <branch> sig/<branch>` makes a local branch
   tracking it, unless one exists, without switching to it:
   `git checkout <branch>` is then the branch `add-pkg` and the other
   cbspkg commands work from.

`--explain` narrates each step, with the git commands as they would be
typed, pausing before each; `--dry-run` shows the steps without doing
anything. The GitLab steps are API requests, described in words with
where to do the same on the web.
