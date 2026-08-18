# TODO

## koji-lag

- (2026-08-17) Packaging: koji-lag now links system SQLite through
  rusqlite, so its Fedora spec needs `BuildRequires: sqlite-devel`
  (`pkgconfig(sqlite3)`) and Debian's needs `libsqlite3-dev`. The
  `bundled` feature is deliberately not used — a vendored C SQLite is
  not acceptable in either archive. rust-rusqlite 0.38 and
  rust-libsqlite3-sys 0.36 are already packaged on rawhide, f43 and
  epel9.

- (2026-08-18) Write up the arch-bottleneck analysis for FESCo, once two
  or three full release cycles are collected — one mass rebuild is an
  anecdote. FESCo's hypothesis (mass-rebuild months worst for s390x,
  freeze months best through low load) held for 2026/07 but needs
  restating in two parts, both measured:
  - s390x pain is **episodic and not load-driven**. July 16-18 produced
    88% of the month's 65,850 hours of s390x delay on ordinary volume,
    while July 12 (10,611 builds) and 24 (11,852) were untouched at a
    50s median. The bad days were 79-95% `releng` (the mass rebuild);
    the busy fine days were 91-98% `koschei` (steady low-priority
    scratch rebuilds). July had 23% more builds than March and a 118x
    worse s390x median wait, with x86_64 unmoved at 52s. Capacity: 35
    s390x-capable hosts against 190 x86_64 and 114 ppc64le, and the
    same 16 hosts ran 146 s390x tasks on the 12th and 4,601 on the 16th.
  - **ppc64le is the chronic tax** and the bigger steady cost: last to
    finish on 75-86% of attributable builds every month at 33-43s each,
    losing more total wall-clock than s390x in both freeze months
    (3,713h vs 2,877h in March; 3,108h vs 2,085h in April). Fixing
    s390x would not touch it.

  Months held as of 2026-08-18: 2026/01 (collecting), 03, 04, 06, 07,
  plus 08/01-09. Needed for two full cycles: 2025/11 and /12 and
  2026/02 (~2.5h each), 2026/05 (import, raw data on the laptop), then
  2026/09 and /10 once they happen. The per-day and per-submitter
  tables are what make the case — a monthly summary hides the three
  days that did the damage.

- (2026-08-17) Split the store by period *when backups start to hurt*,
  not when the file gets large — measured, a decade of data still
  answers a daily report in milliseconds because every query is
  index-bounded, while `VACUUM INTO` rewrites the whole file on every
  backup. Deferred deliberately; the design constraints (query each
  store and merge, never `ATTACH` + `UNION ALL` — 5,600x slower
  measured; a build and its children stay in one store) are recorded
  in the tool's DEVELOPMENT.md.

- (2026-08-17) Remove `import` and the JSON dataset read path once the
  last pre-store dataset is folded in (May 2026, kept on another
  machine). It is hidden from `--help` in the meantime and must not
  appear in a release: strip its paragraphs from the Unreleased
  CHANGELOG entries at the same time, so no published version
  documents a command that never shipped. Going with it: `report`'s
  file arguments, `Dataset::load`/`save`, `json_schema()`, the
  committed `data/koji-lag-dataset.schema.json` and its snapshot test,
  and the serde/schemars derives that exist only to serialise a
  dataset — a store travels as itself, one SQLite file, and CSV serves
  everyone who wants the rows elsewhere.

- (2026-08-17) Ask the Fedora Data WG whether what they have is what
  they wanted. There are two CSV shapes now: `export` dumps the
  store's own rows (builds, tasks, hosts, channels) for anyone doing
  their own analysis, and `report --csv`/`reports --csv` write the
  computed per-arch tables per period. A third shape — one row per
  build with its arch stages spread across columns — is what a
  spreadsheet user often wants and is a writer over the existing
  query rather than a second code path, but nobody has asked for it
  yet and guessing would be inventing a requirement.

- (2026-07-22) DB-dump ingestion: for whole-history analysis, an
  extraction script over a Koji database dump (SELECT just the task
  columns we store, bounded by completion timestamp, to keep the
  dump small) plus an `import` subcommand reading its output. The
  API sweep stays the path for maintainers without dump access.
- (2026-07-22) Validate the `stream` and `cbs` instances — older
  hub versions may omit or rename listTasks fields; only `fedora`
  has been probed live. HubTask decodes tolerantly, so the risk is
  silently-missing fields, not crashes.
- (2026-07-22) Chainbuild ancestry: buildArch tasks whose parent
  isn't a `build` task (e.g. chainbuild layers) stay unattributed;
  walking further up the task tree would attribute them.
- (2026-07-22) Per-host medians report (`--per-host`, deferred from
  v1): median build time per (host, arch) to spot sick builders —
  host names are already captured in the dataset.

## hs-relmon

## Cross-cutting

## poi-tracker / sandogasa-pkg-health seam

Decision (2026-07-21): keep both tools — pkg-health **observes**
(read-only, credential-free, persisted/aged reports), poi-tracker
**acts** (Bugzilla writes, inventory curation). Documented in both
READMEs. Done (2026-07-21): version classifier extracted to
`sandogasa_bugclass::semver` (spec parsing to
`sandogasa_distgit::spec`); pkg-health gained the `pending_update`
check; semver-audit stays in poi-tracker as the interactive
one-shot view. Follow-ups:

- Done (2026-07-22): poi-tracker `adopt` — walks the inventory for
  orphan-owned packages and takes them via the dist-git
  take-orphan endpoint, per-package confirmation.

## cpu-sig-tracker

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
- (2026-07-02, done for `--copr`) check-crate annotates the generated
  Copr script with why each package is in it. `--koji` deliberately
  does not: its output is one chain-build argument string, so a
  comment line lands inside `$(...)` as an argument — the premise
  that comments are pipe-safe holds for a script and not for that.
  If the chain output ever needs explaining, the place for it is
  stderr beside the human report.

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



## ebranch check-update (2026-08-07)

- (2026-08-07) When a recognized bug's package is in no build of the
  update, suggest -1 instead of falling through to a bare prompt. If
  the update ships nothing for that package it does not fix that bug,
  which is what a -1 on Bodhi means, and a silent default of 0 leaves
  the user wondering why nothing was decided.

  Scope it to the two kinds that name their package outright: an
  update request (its Bugzilla component, confirmed by the summary
  parsing as `<component>-<version> is available`) and a review
  request (the package under review, from the title). Everything else
  — CVE, FTBFS, plain bugs — is not classified here and must keep
  today's behavior of no verdict and a 0 default; those are the ones
  where a component can be stale after a package rename, and we have
  no basis to read anything into a missing match.

  Treat it as a suggestion rather than a verdict: the interactive
  prompt defaults to -1 and prints the reason ("this update builds no
  rust-dtor"), and `--yes` takes the suggestion rather than 0, since
  0 would post a claim we have reason to think is wrong. The reason
  goes in the vote plan either way, which is confirmed before
  anything is posted.

  On our own updates, prefer fixing the update to voting on it. A -1
  from the submitter is nearly useless anyway — Bodhi zeroes the
  submitter's overall karma, and karma.rs already detects this as
  `own_update` — while the bug being listed at all is the actual
  mistake. So:
  - under `--submit`, when a bug we are about to attach would score
    -1, offer to leave it off the update before submitting;
  - under `--give-karma` on an update we submitted, offer to edit the
    update and drop the bug instead of casting the -1.

  Editing an update's bug list is safe: it does not unpush the update
  or reset its state, unlike touching its builds. `sandogasa-bodhi`
  has no edit method yet (`new_update_from_tag` and `comment` are the
  only writes), so this needs one — a POST to the update's edit
  endpoint carrying the existing fields with a modified `bugs` list.
  Confirm before writing, and print the bugs being dropped with the
  reason each was flagged.

- (2026-08-07) Judge FTBFS bugs — the strongest verdict available,
  and worth doing first. An FTBFS bug claims a package does not build
  on a release; a successful, tagged build of that package for that
  release refutes the claim outright. That is proof rather than
  inference: no model of the repo, no prediction, just the artifact
  the bug says cannot exist. The pieces exist: `bugclass::classify`
  already identifies FTBFS by the tracker the bug blocks, and the
  bug's `version` field carries the release. What needs care is
  matching that release against the update's — Bugzilla's `version`
  values ("41", "rawhide", "epel9") and Bodhi's release identifiers
  (F41, EPEL-9) are not the same strings, so a mapping is needed,
  and a mismatch has to mean "no verdict" rather than a guess.

- (2026-08-07) Judge FTI bugs from the installability check we
  already run. An FTI bug says a package fails to install on a
  release, and `check-update` computes exactly that: it resolves the
  update's subpackage Requires and reports `installability_issues`.
  So for an FTI bug against a component in the update, on a matching
  release, the check's own result is the verdict — clean is +1, still
  broken is -1 with the unresolved requirement as the reason.

  Weaker than the FTBFS case, though, and worth treating as such: the
  check resolves dependencies against a repo snapshot we assemble, so
  it is a prediction of what dnf would do rather than an observation.
  It can over-report when a touched capability is also provided by an
  unrelated package, and the repo set it resolves against is not
  necessarily the one the reporter had. Good enough to suggest, and
  the reason should name the requirement so the user can judge it.

- (2026-08-07) Judge a bug that depends on a satisfied update
  request. If a bug's component is a package in the update, *and* the
  bug depends on another bug that is an update request for the same
  package, *and* the update satisfies that update request, then the
  update carries the version the bug was waiting for. A CVE against
  foo 4 that depends on "foo-4.1 is available" is closed by an update
  shipping foo 4.1.

  This is also how CVE bugs become proposable by `--submit`'s bug
  discovery, which offers only what the vote logic would score +1 and
  so skips them today: give CVEs a verdict and they are discovered for
  free, with no special case. Note when doing it that a CVE is often
  filed once per distro rather than once per release — one bug marked
  `[fedora-all]` in its summary and one `[epel-all]` — so the release
  scoping lives in that tag rather than in the `version` field.

  Both conditions are required. The component matching a package in
  the update is what keeps this from firing on an unrelated
  dependency, and the depended-on bug being satisfied is what
  supplies the version evidence — neither is enough alone.
  `Bug::depends_on` is already on the model, and the depended-on bug
  is judged by the same `bug_verdict` path, so this is mostly a
  second Bugzilla fetch for the dependencies plus the two guards.
  Only ever a +1. A bug whose chain is *not* satisfied is not thereby
  unfixed, and the reason is worth spelling out because it is easy to
  get wrong: the-new-hotness rewrites an update request's summary as
  new upstream versions appear, so the bug that said "foo-4.1 is
  available" when the CVE was linked to it may say "foo-4.2 is
  available" by the time the update is submitted. An update shipping
  4.1 then correctly scores -1 on that update request — it really is
  behind again — while the CVE it was linked for really is fixed,
  because the fix landed in 4.1. The two verdicts diverge, so an
  unsatisfied chain has to mean silence, never -1.

- (2026-08-07) Discover an update's bugs instead of making the
  maintainer find them: `--submit` should propose the bug list.
  Finding which bugs belong to a big update is one of the more
  time-consuming parts of the flow. Two sources, and the second is
  the one that earns its keep:
  1. Open Bugzilla bugs against the components being built — update
     requests (`<pkg>-<version> is available`, FutureFeature) and
     package reviews. `sandogasa_bugclass::bugzilla::classify`
     already sorts these, and the anchored `extract_new_version`
     tells us whether the build actually satisfies the request.
  2. `rhbz#NNN` mentions in the builds' RPM changelogs. A bug fixed
     in Rawhide is auto-closed when that build lands, so it is no
     longer *open* against the component and source 1 misses it —
     but the EPEL/branch update still fixes it and should list it.
     Read the changelog from the build (koji, or the spec in
     dist-git) and parse the usual `rhbz#NNN` / `RHBZ#NNN` /
     `bz#NNN` / bugzilla URL forms.

  Do not let the update's `--type` decide which kinds to look for.
  An update is routinely a mix: the epel9 update this work came out
  of was `--type enhancement` and carried three Review Request bugs
  for new packages alongside two update requests. Bodhi's type is a
  single value chosen for the update as a whole, so it says nothing
  about which bugs belong to it. Search both kinds for every
  component, whatever the type.

  Present the union for confirmation with each bug's provenance
  (which build's changelog, or which component's open-bug query) and
  let the user drop entries, the way the per-bug vote plan already
  works. Bugs already attached to the update are left alone.

## COPR-staged update workflows (2026-08-07)

- (2026-08-07) Big multi-package updates get staged in a COPR first,
  and the COPR is currently a fact that lives only in the
  maintainer's head. Several tools already speak COPR — `ebranch
  check-update` accepts an `owner/project` spec or project URL as
  its subject, `fedora-review-digest` can review against one, and
  `sandogasa-copr` is the shared read-only client — so this is
  mostly about connecting them rather than new plumbing. Wanted:
  - Record the COPR on a review request when filing it, and find it
    again when reviewing. Real example: the reviews for the
    uutils/nushell stack carry the project only as a build URL in a
    comment — bug 2498026 has
    `copr.fedorainfracloud.org/coprs/g/rust/uutils-and-nushell/build/10697994/`
    in comment 2, while the bug's `url` field points at crates.io.
    So detection means scanning comments for
    `coprs/(g/)?<owner>/<project>` and normalizing to the
    `@group/project` spec the other tools take.
  - Given a review request that names a COPR, have
    `fedora-review-digest` enable that repo for the review run
    automatically, instead of the reviewer wiring it up by hand.
  - Given the COPR, report the state of the whole effort: which
    packages are built there but have no review request, which have
    a review in progress, which are approved and need importing,
    which are in Rawhide but not yet branched, and which need a
    Bodhi update. That is the missing overview for something like
    <https://copr.fedorainfracloud.org/coprs/g/rust/uutils-and-nushell/>,
    and it is the piece that decides what to do next.

  Design settled so far (2026-08-07), the rest still open.

  It goes in ebranch, as a `copr-status` subcommand. ebranch already
  owns every action the report dispatches to — `file-request`,
  `resolve`, `check-update --submit`, `check-pkg-reviews` — and
  already takes a COPR project as a `check-update` subject. The
  report is a status view over capabilities that exist, not new
  plumbing; what is actually new is the state model.

  Each package sits in one state, observed from a client we already
  have, and each state has one thing that unblocks it:

  | state              | observed via            | next action              |
  |--------------------|-------------------------|--------------------------|
  | staged             | sandogasa-copr monitor  | file a review request    |
  | review filed       | Bugzilla Package Review | wait / nag               |
  | review approved    | the fedora-review flag  | SCM request              |
  | repo created       | sandogasa-distgit       | import + build           |
  | in rawhide         | sandogasa-koji          | ebranch file-request     |
  | branched           | dist-git branches       | ebranch resolve          |
  | built for target   | Koji / side tag         | check-update --submit    |
  | in update          | sandogasa-bodhi         | wait / karma             |
  | stable             | sandogasa-bodhi         | done                     |

  An existing package being updated skips the first four states and
  enters at "in rawhide" with a new version, so this is one model
  with the review states marked N/A — not two.

  A package the report cannot place is reported as unknown with no
  suggested action, the same principle the karma verdicts settled
  on: something built in the COPR with no review bug and no dist-git
  repo may be a build-only dependency that is never meant to ship,
  and guessing would nag about it every run.

  The effort is tracked in a local state file, not read fresh from
  the COPR each run. A COPR is a staging area that shrinks: once a
  package is reviewed, imported and built for Rawhide it gets
  removed, and with it every trace that it was ever part of the
  effort. Deriving the package set from the COPR alone would make
  finished work vanish from the report exactly when you want to see
  that it is done. The file also holds what no service can tell us —
  whether a package is meant to ship at all, and which review bug
  belongs to it.

  So each run reconciles rather than rebuilds:
  - in the COPR, not in the file → new work, add it;
  - in both → refresh the observable fields (version, build state);
  - in the file, not in the COPR → keep it, and stop calling it
    staged. It is finished, or moved on. Dropping it needs an
    explicit `--prune`, following poi-tracker's convention.

  State is not monotonic per package, which is the part worth
  getting right: the review states happen once in a package's life,
  but the delivery states repeat per version. A newer build
  appearing in the COPR for something already in Rawhide means that
  package is staged again at the new version — reviews stay done,
  everything downstream of them reopens. So the file records the
  version currently being pushed through, and the delivery states
  are relative to it.

  `check-crate --toml` already persists a `CheckCrateReport` across
  runs, and `check-pkg-reviews` writes discovered review bugs back
  into its `review_bugs` map — this pattern in miniature. Reuse it,
  but as a separate document that feeds the effort file rather than
  one merged shape. The obstacle is lifetime, not fields: a rerun of
  `check-crate` has to be free to overwrite its output wholesale,
  while the ledger must never lose an entry. One file cannot have
  both properties, and a union of the two field sets would leave
  half of them meaningless in each use.

  Three seams to reuse instead:
  - `review_bugs` (package → bug ID) is the same durable fact the
    ledger needs. Give it one owner: the ledger, with
    `check-pkg-reviews` writing there when one is in play and
    falling back to the analysis TOML otherwise.
  - `transitive_build_order` is already "what to build, in what
    order" — consume those `BuildPhase`s rather than recomputing a
    build order from the ledger.
  - `TransitiveDep` carries both `name` (the crate) and `package`
    (the RPM), which is exactly the crate↔package mapping the ledger
    needs and would otherwise have to derive.

  Seeding is not one path, and the COPR is the primary one: point
  ebranch at an existing project and it should create the ledger
  from what is built there, with no prior analysis. A `check-crate
  --toml` run is an alternative seed for work that starts from a
  crate rather than a COPR ("these 8 crates need packaging, in this
  order"), and enrichment for a ledger that already exists.

  That sets the ledger's floor: it has to be useful knowing only
  package names and versions, because that is all a COPR gives.
  Every field that comes from an analysis — the crate a package
  builds, the build order, the dependency edges — is optional and
  fillable later. Placing a package in its state needs only its
  name, since the review bug, dist-git repo, Koji builds and Bodhi
  updates are all looked up by package name; the build order matters
  only for the "branched, not built" action, and its absence should
  degrade that to an unordered list rather than blocking the report.

  An explicit path fits ebranch's habits better than a
  `dirs::state_dir()` location (only fesco-chair uses that today,
  for its saved agenda), and an effort file is something you may
  want to keep beside the work rather than under `~/.local/state`.

  CentOS SIG efforts are not supported, and would not be a small
  addition. `check-wip` takes each branch's Koji tag from Bodhi's
  release list and asks Bodhi which update carries a build — neither
  exists for CBS, where content moves by tagging rather than through
  updates, and `product_version_for_branch` has no Bugzilla product
  for a `c*s` branch either. A `--koji-profile` flag was added and
  then removed: with no tag source for those branches it had nothing
  to act on, and a flag that looks like it enables something it
  cannot is worse than its absence. Doing it properly means a
  per-target notion of how content reaches a release — Bodhi for
  Fedora and EPEL, tagging for CBS — settled before any lookup runs.

  Tying a package to its review bug has three ways in, in order of
  how much they cost the user: set it directly in the ledger; scan
  the ledger for packages whose review bug is unknown and, for each,
  either take an ID or search for one; or search silently where the
  answer is unambiguous. The search itself already exists in
  `review_deps.rs` — product=Fedora, component=Package Review,
  `short_desc` substring on `Review Request: <pkg> - ` — and should
  be extracted rather than written again.

  Two changes to it for this use:
  - Post-filter with `bugzilla::review_request_package(summary) ==
    package` instead of the `starts_with` prefix. It is exact where
    a substring is not, which is the mistake that produced a wrong
    +1 on an update this week, and it is also more tolerant: the
    prefix form requires the summary to use " - " as its separator
    and silently finds nothing when a reviewer wrote something else.
  - On multiple candidates, ask rather than take the newest.
    `review_deps` prefers the latest open bug, which is fine for
    linking Depends On, but the ledger is durable and a wrong ID
    persists until someone notices. Show the candidates with their
    status and let the user pick.

  Closed reviews are wanted, not noise: an approved-and-closed
  review is exactly how a package that already graduated is
  recognized, and filtering to open bugs would lose that.

  Better still, make the field the *route* a package takes out of
  the COPR rather than a review bug with exceptions. There are
  three, and every package is on exactly one:
  - review — a new package, tracked by its Review Request bug;
  - pull request — an update to an existing package, tracked by its
    dist-git PR (someone else's package, or your own when you want
    it reviewed);
  - direct — your own package, pushed and built with nothing to
    track.

  Plus "unknown", meaning nobody has said yet. That subsumes the
  "not applicable" case a review-bug-shaped field would need: an
  existing package is not a review with no bug, it is a different
  route. Without it, a scan re-searches Bugzilla for the same
  package forever and the report shows an entry that can never
  resolve.

  It also fixes an oversimplification above: an existing package
  does not skip straight to "in rawhide". It has middle states of
  its own — PR open, PR merged — and the direct route has none at
  all. Three entry paths that converge once the build lands in
  Rawhide, after which every package follows the same branch,
  build, update sequence.

  `sandogasa-distgit` already has `user_pull_requests` and
  `user_actionable_pull_requests`, keyed by user, which covers "my
  PR" — the usual case when staging an effort. Finding anyone's PR
  against a given package would want a per-project query
  (`/api/0/rpms/<pkg>/pull-requests`), a small addition to the
  client if it turns out to be needed.

  Two modes, split on facts versus decisions rather than read
  versus write. By default `copr-status` reports: it refreshes what
  the services can tell us — COPR versions, Koji builds, dist-git
  branches, Bodhi updates — writes those back to the ledger, prints
  the report, and leaves every unknown as unknown. It never prompts
  and never guesses. Writing the refreshed facts back is deliberate
  rather than a surprise on a read command: they were expensive to
  gather and they are observations, not choices, so discarding them
  would just make the next run pay again.

  `--update` adds the part that needs a person: searching for a
  review bug or PR, asking which route a package takes, resolving
  ambiguous candidates. In `--json` mode or without a terminal it
  must not prompt; it can still fill in what is unambiguous and
  leave the rest, the way the rest of the workspace behaves.

  `--offline` prints the ledger without contacting anything. Not a
  nicety to add later: the point of keeping durable state is that it
  survives, so reading it must not require five services to be
  reachable. Someone on a train wants to know where the effort
  stands.

  Which makes staleness the thing to get right. Each fact records
  when it was last refreshed, so an offline report says "built for
  rawhide (as of 2026-08-05)" rather than presenting a week-old
  observation as current. A status tool that silently shows stale
  data is worse than one that refuses to run.

  Refreshing stays the default, because the usual question is where
  things stand now — but it degrades per source rather than all or
  nothing. A spotty connection is the awkward case: Koji answers,
  Bodhi times out. Keep the last-known Bodhi facts with their old
  timestamp, warn, and report the rest, instead of failing the run
  or blanking a field we simply could not check this time.

  Setting one association directly should not require a full run,
  since it is the fastest path when you already know the answer:
  something like `copr-status --set rust-foo=pr:1234` that writes
  the ledger and exits.

  This is the package → review bug direction. The other direction —
  review bug → COPR, by scanning a bug's comments for
  `coprs/(g/)?<owner>/<project>`, which is how the project behind
  bug 2498026 was found — is still wanted, but for a different
  feature: enabling the right repo when reviewing. Keep them
  separate.
  Targets are derived from the COPR's chroots, narrowed by
  `--target`. The chroots are the universe of what is possible, so a
  `--target` naming a release the COPR does not build for is an
  error that lists the available ones, not an empty report.
  `sandogasa_copr::chroot_prefix` already maps a branch to its
  chroot prefix (rawhide, eln, f42, epel9, c10s) and
  `available_chroots` lists what a project has, so the mapping is
  done.

  One ledger holds every target, rather than one ledger per
  release. Sort the facts by whether they depend on the target and
  the reason is plain:
  - target-independent: the route and its review bug or PR (a
    review happens once in a package's life), the crate↔package
    mapping, whether the package is in scope at all, and whether it
    reached Rawhide;
  - per-target: branched, built, in an update, which side tag.

  Splitting per release would copy the first group into every file,
  and copies of durable facts drift. It would also make a backport
  phase start empty, re-asking which review bug belongs to each of
  forty packages that already know. With one ledger, phase two is
  adding `epel9` to the targets and everything human-supplied is
  already there — the phased workflow is a view, `--target`, not a
  storage layout.

  Rawhide is not a peer of the other targets, which is what makes
  this shape fall out: a package lands in Rawhide first and is
  branched from there, so "in rawhide" belongs to the shared spine
  (staged → route → in rawhide) and only branched/built/update
  repeat per target. A ledger targeting Rawhide alone simply has no
  per-target section yet, which is exactly the state the
  uutils-and-nushell effort is in now.

  Write the settled model into ebranch's DEVELOPMENT.md when the
  work starts; it lives here until then because none of it exists.
