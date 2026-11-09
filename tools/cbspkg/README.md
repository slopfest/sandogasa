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

`sig-repo` needs `git`. Creating a repo needs a GitLab token with the
`api` scope, from `GITLAB_TOKEN` or `cbspkg config`, belonging to an
account with the Maintainer role in the SIG's group.

## Commands

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

`--explain` narrates each step, with the git commands as they would be
typed, pausing before each; `--dry-run` shows the steps without doing
anything. The GitLab steps are API requests, described in words with
where to do the same on the web.
