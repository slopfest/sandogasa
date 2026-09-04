# TODO

Open work only. Findings and their figures live with the tool —
`tools/koji-lag/FINDINGS.md` for the s390x/ppc64le analysis — and
completed work lives in `CHANGELOG.md` and the git history. An entry
here that has stopped asking for something should be removed rather
than annotated as done.

## koji-lag

- (2026-08-22) **`fetch-store.sh` reaches only people with a checkout.**
  `cargo install` places binaries and nothing else, so the README's
  `scripts/fetch-store.sh <url>` does not exist for anyone who installed the
  tool the way the README tells them to. Documented for now with the four
  commands it wraps, verified against the live release.

  Two better answers, in order of preference:

  1. **Make it a subcommand.** `koji-lag fetch-store <url>` works wherever the
     binary works -- cargo install, an RPM, a checkout -- and removes the
     question. `reqwest` is already a dependency; it would need sha2 and the
     zstd crate. The tests must stay off the network, so the download itself
     wants to be a thin injectable seam with the verification tested against
     local fixtures, the way the HTTP clients here already are.
  2. **Ship the scripts in the distribution package.** Fedora packaging is
     expected to, which covers most of the people who will try this. Prefix
     the installed names (`koji-lag-fetch-store`) so they do not collide, and
     say so in the README once the path is decided -- the README currently
     says only "check what your package provides", because inventing a path
     before packaging exists would be worse than saying nothing.

- (2026-08-17) Packaging: koji-lag now links system SQLite through
  rusqlite, so its Fedora spec needs `BuildRequires: sqlite-devel`
  (`pkgconfig(sqlite3)`) and Debian's needs `libsqlite3-dev`. The
  `bundled` feature is deliberately not used — a vendored C SQLite is
  not acceptable in either archive. rust-rusqlite 0.38 and
  rust-libsqlite3-sys 0.36 are already packaged on rawhide, f43 and
  epel9.

- (2026-08-20) Decide what `queries/` content is worth embedding in the
  published notebook. The rest of that plan has landed: the store
  publishes (`scripts/publish-store.sh`, `scripts/fetch-store.sh`, the
  `koji-lag-store-*` release), the narrative notebook is written by hand
  in koji-lag-metrics, and this side supplies the scripts it calls.
  Keeping the notebook out of this tree also keeps a generated artefact
  out of one that would otherwise diff to noise on every re-run.

- (2026-08-20) **The SRPM stage ran on s390x for about ten months, and it
  should never run there at all.** Confirmed as a misconfiguration rather
  than a design: 112,327 `rebuildSRPM`/`buildSRPMFromSCM` tasks executed on
  s390x builders between 2024-12-31 and 2025-10-24, costing roughly 1,370
  builder-hours on the scarcest architecture in the fleet — 621 hours in
  2025-01, 486 in 2025-07, 266 in 2025-08.

  It is not the mass rebuild's doing, which was the first guess because the
  bursts line up with F42's and F43's rebuilds. Every submitter class did
  it, and koschei did most of it: koschei 87,472 tasks, the rebuild 12,164,
  maintainer official 7,036, hand scratch 4,189, CI 1,466. koschei skips
  s390x for `buildArch` almost entirely and *still* had its SRPM stage land
  there, so the cause sits in how the SRPM stage is scheduled — channel or
  policy — rather than in anyone's submission scripts.

  Two open ends. It stops in the store's data after 2025-10-24, but the
  report that prompted this was of a recurrence during the F46 cycle, which
  begins after the store's current last day (2026-08-10) — so re-check once
  collection passes that. And it barely moves the capacity conclusions
  above (utilisation reads 54/72/79/123% on `buildArch` alone against
  56/74/79/123% with the SRPM stage included), because the work is cheap
  per task; it is worth reporting for its own sake rather than as a
  correction.

- (2026-08-20) The branch date is a second cost multiplier, and the
  reports should treat it as a window in its own right. Once a release
  branches, a package that still does not build has to be fixed twice —
  once for Rawhide and once for the new branch — so repair work gets
  more expensive rather than less as the release approaches.

  The activity follows branch rather than preceding it, so this is not a
  deadline rush. F43 branched 2025-08-12 and the next weekday brought
  4,121 builds, then 6,575; F44 branched 2026-02-03 and peaked three days
  later. Maintainer volume roughly doubles or triples across the branch
  (5,518→14,563 and 5,131→9,396 in the two months measured), and the
  share aimed at the new branch rather than Rawhide climbs with it (27%,
  28%, 34% for F44). F43's spike was 81% one maintainer doing bulk work,
  split 53% to `f43-candidate` and 46% to `rawhide` — the doubling
  visible in one account's traffic.

  So the schedule-anchored windows should include branch, and the
  interesting figure there is the *ratio* of branch-target to Rawhide
  builds for the same package set, which is what the double cost looks
  like in data.

- (2026-08-20) ELN is a fourth bulk load source and deserves its own
  section in the write-up: it is heavy on the architecture the whole
  question is about, and it bursts on a calendar nobody publishes.

  It rebuilds whatever Rawhide builds that ELN tracks, since ELN becomes
  the next CentOS release, and s390x is a core architecture for
  RHEL/CentOS — so unlike koschei it does not skip it: **504 s390x tasks
  per 1,000 builds**, against 588 for ordinary builds and 0.1 for
  koschei. A consequence worth stating plainly, since it bears on any
  proposal to drop s390x from Fedora: ELN and EPEL would still need it.

  Its arrival pattern is the interesting part. ELN stands down while a
  mass rebuild is submitting and catches up the moment it stops, on all
  three rebuilds in the store:

  | release | during submission | first catch-up day |
  |:--|--:|--:|
  | F43 | 6-39 builds/day | 1,926 (1,133 s390x) |
  | F44 | 4-83 builds/day | 3,107 (1,697 s390x) |
  | F45 | 2-23 builds/day | 956, then 3,034 |

  So its catch-up lands *inside* the fallout window, on the bottleneck
  architecture, at the same time as maintainers repairing what the
  rebuild broke. Any account of the fallout period that attributes its
  s390x pressure to repair work alone is wrong by several thousand tasks.

  It also bursts with no rebuild anywhere near it — 3,757 builds into one
  side tag on 2025-11-27, four more bursts through June 2026 — which is
  ELN doing a bulk resync of its own. A window detector anchored on the
  Fedora schedule will not find these, so they have to be found from the
  data.

  It runs at **priority 25, the same as releng's rebuild**, so it sits
  beneath maintainer work and absorbs its own delay: mean s390x wait 55
  min in an ordinary month against 95 min in its own burst, while
  maintainers stay at one minute throughout. Chronically slow, harmless
  to everyone else — the releng pattern exactly.

  One hypothesis tested and **not supported**: that ELN gets switched off
  when Koji is having a bad day. Over 411 fully collected days the only
  quiet stretch outside a rebuild window is 2025-12-28..31, when s390x
  waits were 1 minute — that is the holidays, not a response to trouble.
  Worth re-testing as the store grows, since the absence of an event in
  14 months is not proof the practice does not exist.

- (2026-08-19) Measure the FTBFS tail as part of the write-up — it may
  be the strongest thing the store can say about a mass rebuild, and it
  is invisible in the queue figures everyone looks at.

  The same shape twice, a year apart: F43 (2025-07) built 24,398 packages
  and 1,410 failed (5.8%), of which 32.3% still had no successful build
  thirteen months later; F44 built 23,249 and 1,466 failed (6.3%), 34.9%
  unrepaired after seven. Both repaired about 42% within six weeks and
  then crawled.

  F44 built 23,249 packages and 1,466 failed (6.3%). Of those failures,
  counting only successful non-scratch builds afterwards: 25% repaired
  within two weeks, 43% by six, then a crawl of ten points across four
  months, leaving **511 packages (35%) with no successful build seven
  months later**. The jump from 55% to 63% in July is F45's own mass
  rebuild sweeping some up rather than anyone attending to them.

  So a rebuild's cost has two halves on very different timescales: a
  queue collapse of ~60,000 s390x delay-hours that resolves within a
  week, and roughly 1,500 packages needing human repair, a third of
  which are still broken half a year on.

  Three caveats to carry with any published figure. We hold no data for
  2025-09 to 2025-11, so an F43 package repaired only in those months and
  never rebuilt since is miscounted as unrepaired. Retired and orphaned
  packages are not distinguished from abandoned ones — that needs
  dist-git or PDC state the store does not hold — and the window ends
  wherever collection ends, so late repairs are invisible. Both are
  reasons to report the curve rather than a single number.

  A tested counter-hypothesis worth keeping, since it is the intuitive
  one: fallout builds in the first days after a rebuild are *not*
  disproportionately the packages that failed (8.6% of packager builds,
  against a 10.6% control week before the rebuild). The failed set is
  simply the large, actively-maintained packages, which are rebuilt
  constantly anyway. The repair signal only separates from baseline in
  week two (24.9%) and after.

- (2026-08-19, partly done) `--page-size` now defaults to 4000, which
  was the whole of the win in practice; what remains is whether deeper
  windows want more still. The measurements behind the default are in
  DEVELOPMENT.md: `createdBefore` is not flat with depth (a 1000-row
  page costs 0.6-1.6s a day back and 17.7-24.1s at thirteen months) but
  the cost is the seek rather than the rows, so 4000 rows cost 20.8s
  against 18.3s for 1000 at that depth. July 2025's listing went from an
  estimated five hours to twenty-one minutes.
  - Open question: 8000 and 16000 were only measured near the present
    (3.5s and 5.0s, against 1.3s for 4000), never at depth. If the seek
    still dominates at thirteen months, a deep window might want 16000
    and finish in a quarter of the time again. Measure it during the
    next backfill rather than as a separate exercise.
  - If it does help, scale by the window's age rather than probing:
    age is what drives the fixed cost, it needs no learning page, and a
    predictable size loses less work when a page fails.
  - Bound whatever it becomes. 4000 decoded build tasks is already tens
    of megabytes of XML held at once; 16000 is four times that.
  - Not a politeness question either way: pacing is a duty cycle, so a
    larger page leaves our share of the hub unchanged while asking it
    for far fewer of the expensive seeks.

- (2026-08-19) Graduate two analyses from `queries/` into `report`, since
  they carry the FESCo argument and a published command is a stronger
  citation than "we ran some SQL":
  - **Per-day submitter share.** Which accounts submitted a period's
    builds, per day. This is what dates a mass rebuild from evidence —
    its days run 79-95% `releng` while a busy continuous-rebuild day is
    90-98% `koschei` and behaves nothing like it — and the schedule-window
    work above needs it anyway to report the observed window beside the
    announced one. `queries/submitters-by-day.sql`.
  - **Per-package build hours by arch.** Which packages consume an
    architecture's capacity: on 2026-07-17 `gcc` alone took 48.9 hours of
    s390x build time against a pool of sixteen active hosts, and ten
    packages took a quarter of the day. Only possible since builds gained
    package names from their children.
    `queries/package-build-hours.sql`.

  The other two queries are diagnostics rather than reports and can stay
  as SQL: `arch-load-vs-wait.sql` (the capacity curve) and
  `long-builds.sql` (whether `CREATE_GRACE_SECS` still holds).

- (2026-08-18) Write up the arch-bottleneck analysis for FESCo. The
  precondition is met — four mass rebuilds and an unbroken store — and
  the material is in `tools/koji-lag/FINDINGS.md`, which is source for a
  write-up rather than the write-up. What is left is the document FESCo
  actually reads: shorter, one ask per section, and answering their
  original hypothesis (mass-rebuild months worst for s390x, freeze months
  best) in the two parts the data supports — s390x pain is episodic
  rather than load-driven, and ppc64le is the chronic tax that fixing
  s390x would not touch.

- (2026-08-17) Split the store by period *when backups start to hurt*,
  not when the file gets large — measured, a decade of data still
  answers a daily report in milliseconds because every query is
  index-bounded, while `VACUUM INTO` rewrites the whole file on every
  backup. Deferred deliberately; the design constraints (query each
  store and merge, never `ATTACH` + `UNION ALL` — 5,600x slower
  measured; a build and its children stay in one store) are recorded
  in the tool's DEVELOPMENT.md.

- (2026-08-20) `Dataset::merge` is unused outside its own tests and kept
  deliberately — unioning rows from more than one source is exactly what
  querying several stores needs, which is recorded above.
  (`DatasetMeta::schema_version`, the other half of this entry, was
  removed in the breaking pass.)

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

## poi-tracker: essential-deps as a materialized view (2026-09-01)

- The offline recompute is built (`poi-tracker derive`, 2026-09-01):
  reachable-from-keeps ∩ owned, witness-edge reasons, diff-first
  report, idempotent `--apply`. Validated against the first real
  prune: 353 packages in ~1s, name-for-name what the verification
  walk re-derived.
- Both are only as true as the last full walk — rawhide drift (renamed
  sources like pandoc→pandoc-cli, changed Requires/providers) is
  invisible offline. Keep the hour-long full walk as a periodic
  calibration, run when Fedora has moved rather than when keeps
  change.
- Not urgent by the user's own call (one hour every few months is
  livable); build it the next time a prune session actually stalls on
  the walk.

## poi-tracker: the kondo maintenance loop needs a user-facing design (2026-09-04)

The first cycle closed today (releng removed the 39 ask-level ACLs;
`sync-distgit --prune` dropped them and picked up 3 new grants), and
what comes next is the part nobody can hold in their head. After any
change to the keep set — a package released, a new grant, a
retirement — the current procedure is: run `keep`/`unkeep` per
package, iterate the dependency walk to a fixpoint so the new
dependencies are known, run `dependents` to see what became a leaf or
is only carried, decide which of the newly reachable packages deserve
an *essential* entry of their own rather than riding along in
essential-deps, run `derive --apply`, then `kondo` again for what fell
out — each step a different subcommand with its own inventory
arguments and ordering rules. It works, and it is convoluted: the
user's words were "very convoluted and hard to keep track of".

Nothing of this is released, so the design is still free to change.
Sketch the loop as one thing the user drives rather than five they
sequence: a single command (or `kondo` itself) that takes the
inventories and the graph, recomputes what a change implies (new
dependencies, orphaned carried packages, leaves that lost their
reason), presents the decisions it cannot make — "X is now reachable
only through Y; keep it as essential, let it ride, or cull?" — and
writes every file itself. Decide where the fixpoint iteration lives
(inside that command, with the graph as its memory), what the human is
asked and in what order, and how a half-finished session resumes. The
existing subcommands stay as the plumbing. Do this before the release
that first ships kondo; it is a day's work, not an afternoon's.

## poi-tracker / sandogasa-pkg-health seam

Decision (2026-07-21): keep both tools — pkg-health **observes**
(read-only, credential-free, persisted/aged reports), poi-tracker
**acts** (Bugzilla writes, inventory curation). Documented in both
READMEs. Done (2026-07-21): version classifier extracted to
`sandogasa_bugclass::semver` (spec parsing to
`sandogasa_distgit::spec`); pkg-health gained the `pending_update`
check; semver-audit stays in poi-tracker as the interactive
one-shot view. Follow-ups:

## sandogasa-report

- (2026-07-10, maybe) `config` could take `report`'s repeatable `-d`
  flag to scope the credential prompts to a domain subset (default:
  all, the current behavior). A working implementation was drafted
  and reverted in favor of documenting the run-config-once path —
  revisit if configs grow enough domains that full walks get tedious.

- (2026-07-07, nice-to-have) readability polish deferred from the H1
  report review: consider an executive-summary block at the top
  (cross-domain totals). The other half of this entry — suppressing
  all-zero stat lines — has landed.

## dbranch

- (2026-07-03, nice-to-have) the merge phase of proposed-updates and
  backports could run on non-Debian hosts (only build/upload truly need
  Debian); kept simple and symmetric for now with a full up-front host
  guard

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

## hs-relmon

- (2026-06-26) add retire command to archive repo and untag builds. Test with sqlite
- (2026-06-26) check-latest tar --file-issue and check-manifest both
  do not close the issue even though it's up to date. 
  This is likely because it is not built for hs.el10 but that's because
  CentOS 10 is already up to date. Figure out how to handle it

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

## fedora-review-digest

- (2026-06-23) pyp2spec support: a Python checklist + post-import
  boilerplate (terminology: "Python package (from PyPI)", not module).
  Generator detection is already wired; `infer`/`render_post_import`
  just need the Python branch.
- (2026-06-23, later) Run `fedora-review -b <id>` ourselves instead of
  only pointing at an existing result dir.

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
- (2026-07-02, resolved 2026-09-03) check-crate: feature-aware
  dependency resolution — done in the form that fits Fedora's
  packaging, and deliberately no further. Root kind comes from
  crates.io `bin_names` (application → default features; library →
  all features), transitive crates always count all features
  (rust2rpm `-a`), repo checks are batched per dependency list. A
  "hybrid" that would delegate rawhide-packaged crates to `resolve`'s
  real BuildRequires closure was designed and then dropped: the Rust
  workflow is (a) bring or update the package in rawhide, then (b)
  branch or rebase it in the stable series — check-crate serves (a)
  against rawhide, where source and target coincide and the
  delegation case cannot arise, and `resolve` already *is* step (b).
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

## ebranch check-update (2026-08-07)

- (2026-08-07) On our own updates, prefer fixing the update to voting
  on it. (`bug_verdict` now returns `Missing` with a reason when a
  recognized bug's package is in no build of the update, so the -1
  suggestion itself is done.) A -1 from the submitter is nearly useless
  anyway — Bodhi zeroes the submitter's overall karma, and karma.rs
  already detects this as `own_update` — while the bug being listed at
  all is the actual mistake. So:
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

## fedora-cve-triage: find the affected tool by name, not by position (2026-08-25)

`unshipped-tools` learns the affected tool's name by reading the word
before `tool`/`utility`/`binary`/`executable` in NVD's prose. Of the
seven xmllint CVEs on NVD, that phrasing appears in two. The other five
name xmllint with no positional cue: "as demonstrated by xmllint"
(2008-4409, 2018-9251, 2018-14567), "libxml2's xmllint" (2021-3516,
and the possessive form is deliberately excluded as too noisy), "An
issue was discovered in xmllint (from libxml2)" (2024-34459). No
amount of widening the qualifier list reaches those.

The other direction would: we already fetch the component's spec and
its `shipped_binaries`, so ask whether any name the package actually
ships appears in the description, instead of guessing a name from prose
and asking whether it is shipped. That inverts the check — it looks for
a *presence* in a candidate set that is known and small, which is the
safer shape for something that acts on a negative, and it needs no
list of English words at all.

Not free: a package whose binary is a common word (`less`, `file`,
`test`, `sort`) would match prose that never meant the program, so the
candidate set needs a length or specificity floor, and the two routes
probably want to run together rather than one replacing the other.
Worth doing when this check next produces a wrong answer.
