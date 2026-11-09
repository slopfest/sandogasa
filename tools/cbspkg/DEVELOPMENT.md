# cbspkg development notes

Design decisions and rules for future work. What the tool does is in
README.md; why it changed is in the workspace CHANGELOG.md.

## `sig-repo` pushes with `--no-verify`

A CentOS Stream dist-git clone runs `centpkg pre-push-check` as its
pre-push hook. rpkg's check reads the spec from the pushed commit but
the `sources` file from the working tree, so a push of anything other
than the checked-out branch is checked against the wrong `sources`.
rsync's first `cpu-sig-tracker mr` run hit exactly that: pushing the
c10s branch from a c9s checkout was cancelled with "Source file
'rsync-3.5.0.tar.gz' was neither listed in the 'sources' file nor
tracked in git". `mr` answers it by checking the branch out for the
push and switching back.

`sig-repo` cannot do the same: what it pushes is the merge request's
head, fetched to `FETCH_HEAD` from `refs/merge-requests/<iid>/head`,
not a local branch, so there is nothing to check out short of creating
a branch only to delete it again. Nor does the check add anything here.
These are the very commits the hook already passed when `mr` (or
whoever opened the merge request) pushed them to the fork, and the
SIG's repo is a mirror for CBS to build from, not CentOS Stream's
lookaside-backed dist-git. So the push skips the hook, and says so in
the `--explain` narration.

Revisit this if the hook starts checking something that matters for
the SIG's repo, or if rpkg's pre-push check is fixed to read `sources`
from the pushed commit (in which case `--no-verify` can simply go).
