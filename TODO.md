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
  for another N days, file a releng ticket on Forgejo (releng's
  tracker moved from Pagure to Forgejo). The `sandogasa-forgejo`
  crate now exists (added 2026-06-25) with `create_issue` /
  `search_issues`, so this is unblocked on the client side. Still
  needs: the releng Forgejo repo coordinates, growing the report's
  per-request escalation state from `pinged: bool` to a level
  (none → needinfo'd → releng-filed) so `escalate` knows which step
  each request is on, and the releng-filing branch in `escalate`.
- (2026-06-25, EXPLORATORY — may not be worth it) check-update: source
  a Bodhi update's Provides from koji instead of fedrq `@testing`, to
  dodge mirror-propagation flakiness. NOT decided — the current
  `@testing` approach may be good enough if we just accept up to ~1 day
  of mirror lag (the note already explains the transient case). Capture
  before deciding:
  - The obvious "reuse the side-tag path" does NOT work: `@koji:<tag>`
    404s for `updates-testing` (koji serves on-demand repos for side
    tags, not for updates-testing — it's composed into the public mirror
    repo instead). Verified.
  - What DOES work, fully mirror-immune: `koji call getRPMDeps <rpmID>
    1` returns a binary RPM's Provides straight from koji's DB (proven
    on build 3022363). Path: `getBuild <nvr>` → `listRPMs <buildID>` →
    `getRPMDeps` per binary RPM. Needs a new `sandogasa-koji` method.
  - If we do it, use getRPMDeps on BOTH sides — ask koji for the stable
    (old) build's Provides too, not just the new one — so old vs new are
    apples-to-apples from the same source (don't mix koji-new with
    fedrq-stable; formats/arch handling would differ).
  - Real risk to validate first: `compare_provides` is old-driven and
    string-exact (an old provide is "unchanged" only if its exact string
    is in the new set). koji returns `{name, version, flags}`, so the
    strings must be formatted byte-identically (sense-flag operators,
    epochs, bare file/soname provides) and arch-selected consistently,
    or every provide shows as "updated". Validate the diff is clean on a
    real package before trusting it.
  - Evidence from the debugging session (f43 iptstate, 2.3.0-1 in
    testing): `subpkgs -S` returned EMPTY against `@testing` while
    `pkgs --src`/`pkgs` returned 2.3.0; `subpkgs` works on stable and
    for bash/python-setuptools — and the author believes the disagreement
    was transient mirror-propagation skew (different queries hitting
    differently-synced mirrors; he, on better US mirrors, saw them
    agree). So this is propagation, not a deterministic `subpkgs` bug.
  - DONE already this session: the accurate `skip_reason` note, and
    `--refresh` now also clears `~/.cache/libdnf5` (the libdnf5 system
    cache was a separate culprit — it made the *native* branch return
    stale data; `fedrq make-cache` only touches `~/.cache/fedrq`).

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

