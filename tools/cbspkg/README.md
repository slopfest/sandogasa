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

A SIG's release package, `centos-release-<sig>`, which installs the
SIG's repository files, is built from the SIG's repo (`c9s-proposed`,
`c10s-sig-hyperscale`) but into Extras' common collection:
`extras<N>s-extras-common-el<N>s`, then the
`extras<N>s-extras-common-{candidate,testing,release}` tags. `add-pkg`,
`build`, `promote` and `upload-sources` use that collection for any
`centos-release-*` package, and say so before doing anything.

`add-pkg`, `build`, `promote` and `upload-sources` act in CBS or the SIG
lookaside in your name: see
[Commands run by a person](../../README.md#commands-run-by-a-person).

## Hyperscale variants

A Hyperscale branch builds into the variant its suffix names:

| Variant | Branch suffix | Also against RHEL |
|---|---|---|
| main | `hs` (or the older `sig-hyperscale`) | |
| spin | `hs-spin` (or `sig-hyperscale-spin`) | |
| experimental | `hsx` (or `sig-hyperscale-experimental`) | |
| kernel | `hsk` | |
| asahi | `hs+asahi` | |
| gnome (deprecated) | `hs+gnome` | |
| facebook | `hs+fb` | yes |
| intel (deprecated) | `hs+intel` | |

`build`, `promote` and `add-pkg` take `--variant <name>` to use other
variants instead, by name or branch suffix, and `--rhel <name>` to
build one against RHEL rather than CentOS Stream
(`hyperscale9-packages-facebook-el9`, which only facebook has). Both
repeat, so one commit can go to several: perf's `c9s-hs` is built with

```sh
cbspkg build --variant main --rhel facebook
```

into `hyperscale9s-packages-main-el9s` and
`hyperscale9-packages-facebook-el9`. `--variant` gives the Stream
builds in full, so `--variant facebook` builds facebook for Stream
only; `--rhel` alone adds RHEL builds to the branch's own variant, so
`cbspkg build --rhel facebook` does the same for perf. A deprecated
variant is still built, with a note. A package that is always built
that way can say so in `~/.config/cbspkg/config.toml`, which a plain
`cbspkg build` then follows on the branches of the variants it lists
(perf's `c9s-hs` and `c10s-hs`, not its `c10s-hsx`):

```toml
[packages.perf]
variants = ["main"]
rhel = ["facebook"]
```

## Installation

```sh
cargo install cbspkg
```

`add-pkg`, `build` and `upload-sources` need `koji`, `cbs` and
`centos-lookaside-upload-sig` (centos-packager), `centpkg`, and a CBS
login (`~/.centos.cert`, from `centos-cert`). `srpm`, `mockbuild` and
`verrel` need `koji` with the `cbs` profile, to read CBS, and
`rpm-build`; `mockbuild` needs `mock` and mock-centos-sig-configs.
`build` also needs `rpm-build`, and `fedrq` for a Stream collection, to
check the build's Release against its changelog and stock CentOS Stream.
`build`, `srpm`, `mockbuild` and `verrel` need `rpmautospec` for a spec
using `%autorelease` or `%autochangelog`. `sig-repo` needs `git`.
Creating a repo needs a GitLab token with the `api` scope, from
`GITLAB_TOKEN` or `cbspkg config`, belonging to an account with the
Maintainer role in the SIG's group.

## Commands

### `add-pkg`

```sh
cbspkg add-pkg [PACKAGE] [-C DIR] [--branch BRANCH] [--variant VARIANT]… \
    [--rhel VARIANT]… [--owner USER] [--explain] [--dry-run]
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
cbspkg build [-C DIR] [--branch BRANCH] [--variant VARIANT]… [--rhel VARIANT]… \
    [--scratch [--srpm [SRPM]]] [--background] [--nowait] [--owner USER] \
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
built for its RHEL collection too, and `--variant` and `--rhel` choose
the variants (see [Hyperscale variants](#hyperscale-variants)).

Several builds run at once: each is started with `cbs build --nowait`,
and one `cbs watch-task <task>…` waits for them all. `--nowait` leaves
them running and prints that `cbs watch-task` instead. `--background`
builds at a lower priority (`cbs build --background`).

Before building, `build` reads the spec in a worktree of the commit to
build with `rpmspec`, after `rpmautospec process-distgit` fills in
`%autorelease` and `%autochangelog` as CBS does, and checks that its Version and Release match its newest
`%changelog` entry, and that each Stream collection's build, with the
macros CBS builds with, sorts above stock CentOS Stream's current one,
which `fedrq` reads. A Proposed Update with the same Release as stock
sorts below it and is never installed. A mismatch is listed and the
build asks whether to go ahead anyway, defaulting to no; a dry run or
a scratch build only warns. A comparison whose data cannot be read is
skipped with a warning.

CBS fetches the sources from the SIG lookaside, so before building,
`build` checks for them and uploads what is missing as `upload-sources`
does, after asking; scratch builds need them too.

The build is confirmed at a terminal and refused without one, after the
CBS login is checked; the confirmation shows the exact `cbs build`
command, and the comment the package's tracking issue will get once the
build is done, as for `promote` below. To name the build in the comment,
`build` starts it with `cbs build --nowait` and waits with
`cbs watch-task`, then reads the build's NVR from its task; the comment
is posted with that NVR once the build has succeeded. Builds left
running with `--nowait`, and scratch builds, get no comment.

`--srpm` scratch-builds an SRPM instead of the branch's commit, in the
same targets: `cbs build --scratch <target> <file>.src.rpm`, the file
uploaded. With no path it builds one per target first, as `srpm` does,
for a change that is not committed or pushed yet. It skips what only a
build from git needs (the push check, the Release check, the lookaside
upload), and is refused without `--scratch`: a SIG build comes from the
SIG's git repo.

### `scratch-build`

```sh
cbspkg scratch-build [--srpm [SRPM]] …
```

`build --scratch`, under the name fedpkg gives it: the same options,
and the same code.

### `config`

```sh
cbspkg config [--all | --token HOST]
```

Asks for the GitLab token, checks it with GitLab, and keeps it in the
desktop keyring (through `secret-tool`), as the entry every sandogasa
tool reads for that GitLab instance (`secret-tool lookup application
sandogasa service gitlab.com`). Without a keyring to use it goes to
`~/.config/cbspkg/config.toml` instead; a token found there is moved to
the keyring the next time `config` runs. `GITLAB_TOKEN` overrides both.
`--all` or `--token gitlab.com` asks for a saved token again;
[docs/tokens.md](../../docs/tokens.md) lists what it needs.

### `mockbuild`

```sh
cbspkg mockbuild [-C DIR] [--branch BRANCH] [--variant VARIANT]… \
    [--rhel VARIANT]… [--arch ARCH] [--explain] [--dry-run]
```

Builds the branch's SRPM (as `srpm` does), then builds it locally in
the mock config whose buildroot matches each collection the branch
builds into (`centos-stream-hyperscale-10-x86_64`,
`centos-stream-10-x86_64` for Proposed Updates and Extras), with the
collection's macros from CBS passed to mock. The results go in
`results_<package>/<version>/<release>`, as `fedpkg mockbuild` puts
them. A collection mock ships no config for is skipped with a note,
such as the RHEL build a Facebook Stream branch also makes; one with
none left to build, such as Hyperscale's asahi and kernel repositories,
is refused. Build those in CBS with `cbspkg build --scratch`. `--arch`
builds for another architecture than this machine's.

### `new-sources`

```sh
cbspkg new-sources [-C DIR] [--offline] [--explain] [--dry-run] FILE...
```

Sets the package's source files, as `fedpkg new-sources` does in
Fedora: writes their sha512 sums to `sources`, replacing the files it
listed, adds `/<file>` to `.gitignore` for each file no pattern there
ignores yet (`git check-ignore` decides, so a glob such as `/*.tar.gz`
counts), and stages both with `git add`, leaving the commit to you. A
file given from outside the checkout is copied into it, and the copy
is reported; a different file of the same name already in the
checkout stops it.

It then uploads the files the SIG lookaside lacks, as `upload-sources`
does, confirmed at a terminal. `--offline` stops after staging, for
someone who cannot upload: whoever merges the change uploads the
files. `--explain` and `--dry-run` show each step's command; a dry run
prints what it would write and changes nothing.

For a package that reuses CentOS Stream's `sources` file unchanged (a
Hyperscale rebuild with other settings, a Proposed Update adding
patches), use `upload-sources` instead.

### `promote`

```sh
cbspkg promote [NVR] [-C DIR] [--branch BRANCH] [--variant VARIANT]… \
    [--rhel VARIANT]… [--to testing|release] [--explain] [--dry-run]
```

Moves a build to its collection's next stage: from `-candidate` to
`-testing`, from `-testing` to `-release`. The build is the NVR given,
or the package's newest in `-candidate` for each collection the branch
(checked out, or `--branch`) builds into; `--to` names the stage when it
is not the next one, such as `--to release` straight from `-candidate`.

A build is checked before it moves, by the check ebranch's
`check-update` runs, and reported the same way. Its Provides and
Requires, read from CBS, are compared with those of the package's
newest build in the collection's `-release`, which is what everyone
using the SIG's repos has, whether the build goes to `-testing` or
`-release`. What the collection's users have is looked in through
fedrq, as one repository: stock CentOS Stream (AlmaLinux for a
collection built for RHEL), EPEL, and the collection's published
`-release` (`-b c10s -e @epel -e @baseurl:<mirror URL>`). The check
finds the packages there that would no longer install (FTI) because
the build drops something they require, the SIG's own packages that
would no longer build (FTBFS) for the same reason, and the build's own
Requires that nothing provides. FTBFS is left out for stock packages:
many of them do not build from CentOS Stream's public repositories
anyway. Provides are compared across every architecture the build has;
for the dependency check, each binary package is read for the
machine's architecture where the build has one, else as `noarch` or
the first one built. A
collection whose `-release` is not on the mirror yet, before its first
release, is checked without it, and the report says so.

The plan before `Promote?` shows the report in color: what the build
removes, what would break and why, and a collapsed list of the
Provides that only change version. When something would break, a
Requires does not resolve, or the check could not run, Enter answers no
to `Promote?`. `--dry-run` shows the check as well. Without koji's
`cbs` profile, or with a hub that cannot be read, the plan says the
builds were not checked; without fedrq, it says the check could not
run.

The whole check also goes into the build's comment on its tracking
issue, every version change listed in a collapsed list, ending with the
cbspkg version and the command, so whoever tests or releases the build
can see what was checked; when nothing could be checked, the comment
says why. In the plan before `Promote?`, the comment's preview names
the check shown above it rather than repeat it; `--dry-run` shows each
comment exactly as it would be posted.

Promoting to `-release` also untags the build from `-testing`, so a
released build is not left in testing as well. A build released before
that, and still in `-testing`, is promoted again to finish the untag.

Older builds of the package that the promoted one replaces are then
offered for untagging, in a question of their own (default yes): those
in the stage it moves into, and for a release those in `-testing` too.
`-candidate` keeps every build. `--dry-run` shows the untags; a tag
that cannot be read leaves its builds alone, with a warning.

The build's tracking issue, in the package's SIG repo, is updated:

- for a Proposed Update, the open cpu-sig-tracker issue carrying the
  branch's release label (`c10s`) and whose `JIRA` line names its RHEL
  issue (`RHEL-306261`), as the branch `c10s-proposed-RHEL-306261`
  does;
- for Hyperscale, the open `rfe::new-version` issue hs-relmon filed,
  `ethtool-7.1 is available`; of several open, the one asking for the
  newest version the build reaches. A build older than that version
  (a release candidate, a rebuild) still gets its comment, saying so,
  but sets no `stage::` label or status and does not close the issue,
  which follow the version asked for.

It gets a comment saying where the build went, inviting testers, for a
`-testing` build, to react with 👍 or 👎 and to comment on what broke,
and, for a Proposed Update, what it was built from: the merge request's head, or the head
with SIG-only commits on top, each listed with its subject, or a commit
that lacks the merge request's current head. It also gets the scoped
label `stage::testing` or `stage::release` when the project or the
SIG's group has that label, and the status `In progress` if it was
still `To do`. A later comment about the same build points to the
SIG-only commits an earlier one listed, from `build` or the promotion
before, instead of listing them again; a different list, as after the
merge request was updated, is listed in full.

When the branch has commits the build was not made from, merged since,
the comment lists them too, with the day of the newest and the merge
request each merged, so whoever moves the build sees what it does not
carry. A merge commit that changes no file, merging the very commit
built, does not count.

A Hyperscale issue is done once every collection the package is built
for has the version it asks for, or a newer one, released. The
collections are those the checkout has a SIG branch for, its own or a
remote's, each branch its own: `c9s-hs` and `c10s-sig-hyperscale` are
main, `c9s-hsx` experimental, `c9s-hs+fb` facebook; deprecated variants
are left out. A collection with no build of the package in its
`-candidate`, `-testing` or `-release`, a branch made but never built
from, does not count, and the comment names it. Promoting the last of
them to `-release` adds that to the comment, with each collection's
build, sets the issue to `Done` and closes it. While another is behind
the issue stays open; hs-relmon closes it once main 9 and 10 catch up.

Each comment ends in a hidden marker, so a promotion already commented
on gets no second comment, and a build that reached its stage gets its
comment even when a later `cbs` command fails. Posting
needs the GitLab token from `cbspkg config` or `GITLAB_TOKEN`, and the
confirmation says when there is none; a failure posting is a warning,
as the build has moved by then.

Each `cbs tag-build` and `cbs untag-build` is shown in the confirmation,
with the comment the tracking issue will get, which is asked at a
terminal after the CBS login is checked, and refused without one.
`--explain` and `--dry-run` show each step's command, and `--dry-run`
the comment.

### `sig-repo`

```sh
cbspkg sig-repo <mr-url>… [-C DIR] [--explain] [--dry-run] [--yes]
cbspkg sig-repo <package> --sig proposed-updates [-C DIR] [--explain] [--dry-run] [--yes]
cbspkg sig-repo <package> --sig hyperscale [--dry-run] [--yes]
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

Each run also makes the repo open on its newest SIG branch: Proposed
Updates' `c<N>s-proposed-<RHEL key>`, Hyperscale's `c<N>s-hs` or
`c<N>s-sig-hyperscale`. That is the one for the newest Stream release,
and of several for it, the one just pushed, else the one with the newest
head commit. A repo with no SIG branch yet keeps the default it has.
Changing it needs the Maintainer role; without it, or without a
terminal or `--yes`, the default is left as it is, and the run says so.

Merge requests into a SIG's repos land the way the SIG has chosen:
both SIGs fast-forward them (`ff`), so they are rebased and history
stays linear. For Proposed Updates, whose branches are the Stream merge
request's commits as they are, that also keeps a merge request from
putting a commit on one that the Stream merge request does not have.
A new repo is created with the SIG's merge method, and an
existing one that differs is offered it, the same way as the default
branch. A SIG's choice can be overridden in
`~/.config/cbspkg/config.toml`, with GitLab's names for the methods
(`ff`, `merge` or `rebase_merge`):

```toml
[sigs.proposed_updates]
merge_method = "rebase_merge"
```

A checkout's remotes are known by the project they point at, not by
name: CentOS Stream's repo, a SIG's, or a personal fork
(`<user>/centos_rpms_<package>`), so `origin` can be any of them. A
remote `sig-repo` adds is named after the SIG (`proposed_updates`,
`hyperscale`); in a checkout of the package, it adds the SIG's repo
when no remote points at it, and fetches that remote.

Given CentOS Stream merge requests, Proposed Updates, it then carries
each change to the SIG's repo. Several merge requests of one package,
such as the same fix for `c9s` and `c10s`, are carried in one run, all
read from GitLab in one request. Given only the package and
`--sig proposed-updates`, it carries the merge requests the SIG repo's
open tracking issues name, saying which it found, and skipping one
closed in CentOS Stream without being merged.

The changes are pushed from the dist-git checkout: the current
directory, or `-C`. A `-C` directory that does not exist yet, or is
empty, gets a clone of the SIG's repo, after asking (or with `--yes`);
any other directory must be a checkout of the package. For each merge
request:

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

The run then checks out the newest branch it carried, the newest
Stream release's, unless the checkout has uncommitted changes, which
it leaves where they are.

A SIG branch can carry SIG-only commits on top of the merge request's
head, ones that must never go to CentOS Stream, such as a BuildRequires
pin working around CBS's buildroots. A branch the merge request's head
is inside of is left as it is, and the run lists the commits it carries
on top. When the merge request has moved on and the branch has commits
its new head lacks, nothing is pushed, since that would take a force
push: the run lists them, says how to rebase them onto the new head by
hand, carries on with the rest, and fails at the end.

Run again for the same merge request, `sig-repo` asks the SIG's repo
(`git ls-remote`) whether the branch is at the merge request's head
already, and skips the fetch and push when it is, pushing only when the
merge request has moved on. It ends by naming the SIG's tracking issue
for each merge request, or, when there is none yet, the
`cpu-sig-tracker file-issue` that files one.

`--explain` narrates each step, with the git commands as they would be
typed, pausing before each; `--dry-run` shows the steps without doing
anything. The GitLab steps are API requests, described in words with
where to do the same on the web.

### `srpm`

```sh
cbspkg srpm [-C DIR] [--branch BRANCH] [--variant VARIANT]… \
    [--rhel VARIANT]… [--explain] [--dry-run]
```

Builds the branch's SRPM in the checkout, with the dist tag CBS would
give it: `rsync-3.2.7-5~proposed.el10.src.rpm`, not the host's `.fc44`.
The dist tag comes from the collection's buildroot tag in CBS, read
with `koji -p cbs call getBuildConfig`: Hyperscale's Stream collections
set `dist` itself (`.hs.el10`), while Proposed Updates and Extras set a
`distprefix` or `distsuffix` (`~proposed`, `s`) that `srpm` wraps
around `.el<N>`. A source file the `sources` file names and the
checkout lacks is fetched from the SIG lookaside first, checked against
its sha512; one the checkout has with another sum is an error, not
replaced. A branch building into several collections gets an SRPM for
each. The spec and sources are the working tree's, so `--branch` must
name the branch checked out, as all three commands require. A spec
using `%autorelease` or `%autochangelog` is read, and its SRPM built,
from a copy that `rpmautospec process-distgit` fills in from the
branch's history, as fedpkg, centpkg and CBS do.

### `untag`

```sh
cbspkg untag NVR --reason rejected|retired|landed [--landed-in NVR] \
    [--from testing|release]… [-C DIR] [--branch BRANCH] \
    [--variant VARIANT]… [--rhel VARIANT]… [--explain] [--dry-run]
```

Takes a build out of its collection's `-testing` and `-release` tags,
wherever it is in them, or only those `--from` names, for each
collection the branch (checked out, or `--branch`) builds into.
`-candidate` keeps the build history and is not touched.

The build's tracking issue, found as `promote` finds it (a Hyperscale
build older than the version its issue asks for sets no label), gets
a comment
saying where the build left and why, from `--reason`:

- `rejected`: turned down in review, so the update needs another
  build. The issue is labeled `stage::rejected`, which flags it for
  attention until the next build reaches a stage: `build` and
  `promote` take the label off then.
- `retired`: the package is retired from the SIG, labeled
  `stage::retired`.
- `landed`: the change landed in stock CentOS Stream, in the build
  `--landed-in` names.

Each label is set only when the project or the SIG's group has it. The
comment ends in a hidden marker, so an untag already told is not told
again, and the work item's status is left alone. Each `cbs
untag-build` is shown in the confirmation, with the comment, which is
asked at a terminal after the CBS login is checked, and refused
without one; `--explain` and `--dry-run` show each step's command.

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

### `verrel`

```sh
cbspkg verrel [-C DIR] [--branch BRANCH] [--variant VARIANT]… \
    [--rhel VARIANT]… [--explain] [--json]
```

Prints the version-release the branch builds in CBS, with the same dist
tag as `srpm`: `centos-release-proposed_updates-9-2.el9s` for a release
package built in Extras. A branch building into several collections
prints one line for each, naming its target. `--json` prints a list of
`{"target", "verrel"}`.
