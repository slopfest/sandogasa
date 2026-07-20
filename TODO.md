# TODO

## cpu-sig-tracker

- (2026-07-20, follow-up to the claim-on-close rule) `retire` closes
  GitLab tracking issues but doesn't offer to assign them to the
  person running the command. The Bugzilla-side mechanics live in
  `sandogasa_bugzilla::claim`; a GitLab port needs the current user
  from `GET /api/v4/user` (the token-check endpoint already used for
  auth validation) and `assignee_ids` on the issue update. Reuse
  `resolve_claim` for the decision matrix so the semantics stay
  uniform.

## sandogasa-report

- (2026-07-10, maybe) `config` could take `report`'s repeatable `-d`
  flag to scope the credential prompts to a domain subset (default:
  all, the current behavior). A working implementation was drafted
  and reverted in favor of documenting the run-config-once path —
  revisit if configs grow enough domains that full walks get tedious.

- (2026-07-07, nice-to-have) readability polish deferred from the H1
  report review: suppress all-zero stat lines in the non-detailed view
  (e.g. "Releases published: 0 across 0 project(s)"), and consider an
  executive-summary block at the top (cross-domain totals)

## fesco-chair

- (2026-07-06) the `script` subcommand emits the
  `!forge issue fesco tickets NNNN` workaround because the `!fesco NNNN`
  alias is broken; switch back to `!fesco NNNN` once
  https://github.com/fedora-infra/maubot-fedora/pull/154 is merged AND
  deployed to the Fedora maubot

## ebranch

Done (2026-07-06):
- check-update `--submit`: check a side tag pre-emptively and submit it
  to Bodhi only when the check passes (notes via `--notes`/`--notes-file`,
  plus `--type`/`--severity`/`--bug`/karma-threshold flags)

## dbranch

- (2026-07-03, nice-to-have) the merge phase of proposed-updates and
  backports could run on non-Debian hosts (only build/upload truly need
  Debian); kept simple and symmetric for now with a full up-front host
  guard

Done (2026-07-06):
- upload stage supports Debusine personal repositories (`--debusine
  <name>` on rebuild + update → `dput -O debusine_workspace=… -O
  debusine_workflow=publish-to-<base-suite>-<srcpkg>
  debusine.debian.net`)

Done (2026-07-03):
- rebuild supports Debian backports targets (`debian/<codename>-backports`
  → `~bpo<N>+<M>`, gbp.conf debian-branch only, salsa-ci RELEASE pinned to
  `<codename>-backports` — leaving it untouched built against sid). Tested
  with iptstate.

## ebranch

- (2026-06-29) Follow-ups to the review-issue unification (deferred from
  the first cut):
  - Bulk/group-of-groups curation actions (e.g. "remove all
    installability") if per-finding-grouped prompting is still tedious on
    a 330-finding run.
  - Persist/resume curation decisions across runs (neither tool persists
    today).
  - Optionally curate stale-side-tag via keep/explain/remove too (it
    keeps its own regen flow for now).

## sandogasa-review adoption

- (2026-06-29) Surveyed the workspace for other tools that could adopt the
  keep/explain/remove resolver. Possible future fits (per-item interactive
  loops, but their decisions are *actions* not finding-validity, so adoption
  would reshape semantics — lower priority):
  - hs-relmon `prune-archived` / `review` — add an "explain" reason when
    keeping an ahead-of-stock build / skipping a karma vote.
  - poi-tracker `triage-updates` AskClose — per-bug explain instead of one
    batch y/N.
  - hs-intake `safe-to-backport` — only if it grows an interactive mode that
    breaks the aggregated "concerns" into per-item findings.
  Not applicable: sandogasa-pkg-health, koji-diff, cpu-sig-tracker, dbranch.

Done (2026-06-29):
- Unified review-issue handling: new `sandogasa-review` crate provides the
  keep/explain/remove resolver; `fedora-review-digest` refactored onto it
  (behavior-preserving) and `ebranch check-update --give-karma` now curates
  blocking findings (installability + reverse-dep breaks grouped by Provide)
  before deriving karma and posting — explained/removed findings don't
  downvote; explanations go in an "addressed by the reviewer" section.
- fedora-cve-triage adopted `sandogasa-review`: the false-positive detectors
  (interpreter-fps, js-fps, cross-ecosystem, unshipped-tools) now review each
  detected bug keep/explain/remove before closing as NOTABUG (explain appends
  a justification to the close comment), instead of one bulk y/N.
- check-update condenses large updates: counts by default, updated
  packages grouped by `old → new` version transition, new packages
  listed separately, actionable findings still shown in full, bulky
  lists behind `--detailed` (and capped at 15 otherwise).
- check-update memoizes stable-repo capability resolution
  (`provides_of_provider`) per capability, so libstdc++ / libQt6Core.so.6
  resolve once per run instead of once per requiring package. (A general
  fedrq-layer cache across all query methods is still possible if more
  memoization is needed — would touch ~20 `Fedrq {}` literals.)
- check-update side-tag NVRs now use `koji list-tagged --latest`, and
  the staleness check only flags repodata that's *older* than expected
  (rpmvercmp) — so a side tag that moved 6.7.0 → 6.7.1 no longer
  false-flags as stale.
- check-update now evaluates boolean/rich deps in the installability
  check with real semantics (`A if B` requires A only when B resolves,
  `unless`/`or`/`and`/`with`/`without` likewise) instead of requiring
  every capability — plus fixed the extraction bug that left a stray
  `)` on inner-group caps. Fixed the bogus plasma-settings issue; a
  flagged boolean dep now reports which capabilities failed.

## Dependencies / Fedora packaging

Done (2026-07-07): rust-quick-xml 0.41 landed in Fedora and EPEL, so
the `>=0.40, <0.42` range (introduced post-0.15.3 per the CLAUDE.md
range policy) is tightened back to `"0.41"` — no more floor/ceiling
double-testing at release time.

## hs-relmon

- (2026-06-26) add retire command to archive repo and untag builds. Test with sqlite
- (2026-06-26) check-latest tar --file-issue and check-manifest both
  do not close the issue even though it's up to date. 
  This is likely because it is not built for hs.el10 but that's because
  CentOS 10 is already up to date. Figure out how to handle it

Done (2026-07-02):
- --version now works on every tool (hs-relmon and hs-intake were
  missing the standard clap header; audited all 14 tools).

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

- (2026-07-02) check-crate: MSRV awareness for EPEL targets. The
  base-distro guard does NOT apply to check-crate — RHEL/CentOS Stream
  don't ship crates as RPMs (their Rust binaries vendor dependencies),
  so a crate dep can never be "in base, too old". The EPEL-specific
  failure mode is instead the **Rust toolchain**: EPEL 9 builds against
  a stable RHEL minor whose rustc can lag some crates' MSRV (EPEL 10 /
  CentOS Stream moves fast enough); nothing to do but wait for the next
  minor — but check-crate could *say so upfront*: compare each crate's
  `rust_version` (crates.io exposes it) against the target's rustc and
  flag chains that are blocked on the toolchain before any branch
  requests/builds are attempted.
- (2026-07-02) check-crate: feature-aware dependency resolution. Optional
  deps are all-or-nothing today — `should_expand` skips `optional=true`
  deps unless `--include-optional`, which then pulls in *every* crate's
  optional deps. But an optional dep enabled by a feature the root crate
  activates is effectively required, e.g. routinator → rpki `^0.19.3` →
  quick-xml `^0.39.4` (optional, behind rpki's `rrdp` feature that
  routinator turns on). Fedora rawhide has quick-xml 0.40.1 + compat
  0.31/0.36/0.37/0.38 — no 0.39.x — so it's genuinely unmet, but
  check-crate never checks it (default) or over-reports it (with
  `--include-optional`). Proper fix: resolve the enabled feature set from
  the root down and follow only the optional deps those features
  activate — needs each crate's `features` map, per-dep
  `features`/`default-features`, and the root's enabled features (the
  Cargo feature-unification problem).
- (2026-07-02, remaining half) check-crate: flip `--include-optional` to
  on by default (rename to `--exclude-optional`) once the feature-aware
  resolution above lands — flipping it earlier is NOISY (it includes
  optional deps the root doesn't enable). The `--include-unmet` half was
  flipped to `--exclude-unmet` in v0.16.0.
- (2026-07-02, remaining nicety) check-crate: optionally annotate the
  `--koji`/`--copr` machine output with needed-version comments (e.g.
  `# quick-xml: need ^0.39.4, Fedora has 0.40.1`) — pipe-safe as
  shell/TOML comments. The main ask (human report to stderr alongside
  machine stdout) shipped 2026-07-02.

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
  - Decided NOT to switch the presence check to `fedrq pkgs --src`:
    `subpkgs` reads the *binary* repo and `pkgs --src` the *source* repo
    (separate repos that sync independently), so mixing them could have
    the presence gate pass off the source repo while the binary side
    still lags — more inconsistency, not less. Stay on `subpkgs`
    throughout (one repo); the only switch worth making is the wholesale
    move to koji below, which is consistent AND mirror-immune.
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

- (2026-07-02) Extend the commit detail-level policy to the other
  sections. Commits now render as: summary = total + repo count;
  `--detailed` = per-repo counts; `--detailed --detailed` = individual
  commits with subject (see `DEVELOPMENT.md` "Commit detail levels"). The
  PR / issue / patch / ticket sections (github/gitlab/forgejo/sourcehut)
  still list every item at `--detailed` with no level-1-vs-2 distinction.
  Decide whether they want the same three-tier treatment (e.g. `--detailed`
  = counts or a compact list, `--detailed --detailed` = full per-item
  detail) and apply it uniformly. Likely presentation-only.
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

