# TODO

## dbranch

- (2026-06-19) Target-type / version-scheme abstraction — the rest of
  the big piece. Live-test it when updating `archlinux-keyring`'s
  `debian/trixie`. A per-target notion driving `changelog::
  rebuild_version` + `normalize_top_stanza`: Ubuntu PPA `~<codename>+N`;
  Debian backports `~bpoN+M`; proposed-updates `+debNuM`;
  unstable/testing = no suffix. Branch taxonomy → target: `master`/
  `main`/`debian/unstable` → Debian unstable (`dput` default, or
  mentors for a new pkg / proposed NMU); `ubuntu/*` or an Ubuntu-release
  codename → PPA; `debian/<codename>` (e.g. archlinux-keyring's
  `debian/trixie`) → special, kept current in stable. Also rename the
  `codename` value → `distribution` (what pbuilder/gbp/changelog call
  it), keeping "codename" only for the `~<codename>` version suffix.
  The build-suite decoupling means a target has both a changelog
  *distribution* and a *build suite*, on top of the version scheme.

- (2026-06-19, low priority) Optional per-package waiving of a specific
  salsa-ci job (e.g. `test-uscan` fails on trixie when the watch file
  uses a uscan standard newer than trixie's uscan). Not blocking: `push`
  (CI watch) is separate from `upload`/`tag`, so a red job doesn't stop
  an upload. Keep it a targeted job-skip, not a blanket relaxation
  (proposed-updates should face the normal checks).

  (Proposed-updates themselves — `~debNuM` version, `gbp dch --stable`,
  salsa-ci preset, Debian-host gate, dput-default upload — and the
  `update` default-target upload guard are all done.)

Note (2026-06-19): bulk is deliberately **local-branch only** — a local
branch *is* the opt-in. To include a release, check it out once; to
drop it, delete the local branch. (Remote-inclusive bulk was
considered and rejected.)

Done (shipped): the `push`/`upload`/`tag` stages, per-job CI watch
progress, `git push -u`→plain-push simplification, `--quiet` mode, the
`--source` merge-source override, the `fixup` subcommand, stale-chroot
auto-refresh (`--refresh-chroot`/`--no-refresh-chroot`), grouped
`--help` sections, the safer bulk run (Ubuntu-codename selection
via `ubuntu-distro-info`, EOL skip + `--include-eol`, newest-first
order, confirmation + `--yes`), and the `update` subcommand
(new-upstream import of the Debian branch, sharing the build→…→tag
pipeline; `--build-suite`, dput-default upload).

## fedora-review-digest

- (2026-06-23) pyp2spec support: a Python checklist + post-import
  boilerplate (terminology: "Python package (from PyPI)", not module).
  Generator detection is already wired; `infer`/`render_post_import`
  just need the Python branch.
- (2026-06-23, later) Run `fedora-review -b <id>` ourselves instead of
  only pointing at an existing result dir.

Done (shipped): the core digest + interactive `+1/0/-1` finalization,
`--post` (comment + `fedora-review` flag + status POST + bug claim) and
the `config` subcommand, rust2rpm spec/license fixes, the
builds-and-installs item reading fedora-review's install verdict,
interactive issue resolution (keep/explain/remove → APPROVED flip), and
the statically-linked-deps license verification (build-log LICENSE
SUMMARY vs the spec's folded `License:`, confirmed on rust-git-absorb).

## ebranch

- Second-level branch-request escalation: when a `needinfo?` ping
  (the level-1 escalation `escalate` already does) goes unanswered
  for another N days, file a releng ticket. Blocked on a Forgejo
  client — releng's tracker moved from Pagure to Forgejo, which
  sandogasa has no client for yet. Also needs the report's
  per-request escalation state to grow from `pinged: bool` to a
  level (none → needinfo'd → releng-filed) so `escalate` knows
  which step each request is on. Plan: add a `sandogasa-forgejo`
  crate (issue create/search), extend `BranchRequest`, and add the
  releng-filing branch to `escalate`.

## sandogasa-report

- Debug CVE/security bug reporting: the query may be too narrow or
  the keyword filter may not match Bugzilla's actual keyword values.
  Test with known CVE bugs and compare against manual Bugzilla search.
- (2026-06-24) Apply the Forgejo "(applied)" detection (closed-unmerged
  PR whose commit landed out-of-band) to GitHub and GitLab too. The
  approach is identical — each forge has a compare endpoint
  (GitHub `/compare/{base}...{head}` → `status`/`ahead_by`; GitLab
  `/repository/compare?from=&to=` → empty `commits`) — but it's a
  per-crate implementation: add `pull_request`/`merge_request` detail +
  a `commit_contained` method to `sandogasa-github` and
  `sandogasa-gitlab` (neither has them). GitHub slots in cheaply (its
  reporter is search-based like Forgejo, so annotate the opened list the
  same way); GitLab needs more (its reporter is *event*-based and
  doesn't currently enumerate closed-unmerged MRs to annotate).
- (2026-06-24) Forgejo: detect a closed PR whose work landed via a
  *reworded/rebased* commit (different SHA, so the `head.sha`-on-
  default-branch check used for the "applied" state misses it). Run it
  as a FALLBACK only when the SHA check (#1) is negative, to keep that
  path precise (zero false positives). Mechanics (verified against
  rhbz-style codeberg data):
  - The PR's `Fixes #N` link is FREE — the pulls search result already
    includes `body` (and `state`/`closed_at`), so no fetch to find the
    linked issue.
  - `GET /repos/{o}/{r}/issues/{N}/timeline` is ONE call and yields
    both a `pull_ref` (the PR) and a `commit_ref` (the landing commit
    SHA) directly — exactly the join we want.
  - Trusting the `commit_ref` alone is 1 call but fuzzy: it means "a
    commit referenced the issue," not "your PR's commit is on the
    default branch" — so a different person fixing the same issue would
    falsely credit a declined PR. To stay safe, confirm the commit's
    author is the user and/or that it's on the default branch
    (`commit_contained`), which costs ~1 more call (back to ~2, same as
    #1 but with reworded coverage). Gate on the PR carrying a
    `Fixes #N` so we only spend calls where there's something to find.
- (2026-06-24) Document the required GitLab and GitHub token
  permissions/scopes in the README, the way the Forgejo
  authentication section now does (exact scopes per operation, and
  which are only needed by `config`'s token validation). Determine
  the *minimum* fine-grained scopes — currently the GitHub side is
  being used with a legacy/coarse-grained classic PAT, so figure out
  the least-privilege fine-grained-PAT permission set (Contents,
  Pull requests, Metadata, etc.) and the GitLab equivalent (`read_api`
  vs `api`, and whether `read_user` is needed for the username
  lookup). Cross-check against `validate_token` and the actual
  endpoints each `*_report` calls.

