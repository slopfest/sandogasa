# TODO

## koji-lag

- (2026-08-17) Packaging: koji-lag now links system SQLite through
  rusqlite, so its Fedora spec needs `BuildRequires: sqlite-devel`
  (`pkgconfig(sqlite3)`) and Debian's needs `libsqlite3-dev`. The
  `bundled` feature is deliberately not used — a vendored C SQLite is
  not acceptable in either archive. rust-rusqlite 0.38 and
  rust-libsqlite3-sys 0.36 are already packaged on rawhide, f43 and
  epel9.

- (2026-08-20) Two report features that follow from the tail finding, and
  neither singles anybody out.
  - **`report --owner NAME` and `--package NAME`.** These do not exist,
    though DEVELOPMENT.md has been claiming "narrowing is `report
    --owner`/`--package` over the store" since the filters were taken out
    of the sweep — fix that sentence either way. With them, a maintainer
    can run a published store and see their own experience directly, which
    is the right answer to "how badly am I affected" and needs no cohort
    or naming at all.
  - **Cohort rows: top N submitters by volume, the next M, and the rest.**
    Publishable, and it carries the finding without a name. Measured for
    2026-07, official builds by people (338 submitters), s390x: the top
    ten hold 10,985 of the month's 14,402 tasks with a p90 of 2.9h and
    **27.8% of their builds waiting over an hour**, the next forty 8.6%,
    and the remaining 288 people 4.5%. ppc64le is starker still — top ten
    at a 1.4h p90 and 18.3% over an hour, everyone else at one minute and
    0.1%. Every cohort has a one-minute median, which is exactly how this
    stayed invisible.

- (2026-08-20) **ppc64le lost half its capacity in July 2025 and has not
  got it back.** From the `host_config` history now in the store: 132
  weight through 2025-06, then 64 from 2025-07 and 58-62 ever since. The
  administrative halving noted above for 2025-11-11 — 32 hosts to 16 — was
  therefore a further dip on top of a fleet already at half strength, not a
  fall from full strength. Worth stating whenever ppc64le is compared with
  s390x, since the two architectures' capacity moved in opposite directions
  over the same eighteen months: s390x up 57 to 93, ppc64le down 132 to 58.

- (2026-08-20, DONE) **Builder capacity is in the store.** `host_config`
  holds 12,592 revisions back to 2018-10-03, fetched by `sync` on every run
  in one call, and `Store::capacity_at` answers "enabled hosts and weight
  for this architecture at this instant". Validated against the figures
  previously computed outside the repo: the four rebuild windows read
  96/93/93/91.5, matching. What follows is why it was worth doing. Everything in the
  four-rebuild analysis comes from the store except its denominator:
  enabled hosts and their weight capacity, which came from a live
  `queryHistory(tables=['host_config'])` call cached outside the repo. So
  the central claim — capacity flat while service time doubled — is the
  one nobody else can reproduce, including us once that file is gone.

  `hosts` holds only id, name and arches. What is needed is the history:
  per host, the spans over which it was enabled and at what capacity,
  which is exactly what the hub returns. It is backfillable to 2020 in a
  single call, so this is a migration and one fetch rather than a
  collection campaign — and it turns "has s390x kept pace with the rest of
  the fleet?" into a query instead of an afternoon.

  Watch the reconstruction: enabled-at-an-instant from overlapping
  revisions is easy to get wrong, and the check that caught it was
  comparing against hosts observed serving work *at the same instant*
  rather than over the whole day.

- (2026-08-20, exploration) Emit reports as Jupyter notebooks, so a reader
  can change the question rather than only read our answer. Ship a
  notebook plus a database dump and the queries become a starting point:
  someone can re-cut a window, swap the architecture, add a cohort, or
  test a hypothesis of their own without waiting for us to build a flag
  for it.

  **Publishing the store itself is viable, which settles the question that
  was blocking this.** zstd at its default setting takes the store to 32%
  — measured 2026-08-20 at 2,402MB down to 763MB, and independently at
  1.95GB down to 645MB on an earlier backup. That is a download rather
  than a dataset release, so the notebook can query the real store and
  needs no CSV extract that goes stale the moment it is written.

  Decided 2026-08-20: **the notebook is written by hand in the
  koji-lag-metrics repo; this side supplies the scripts it calls.** So the
  work here is the publish half — producing the compressed store with a
  checksum, a helper that fetches and verifies it, and `queries/` content
  worth embedding — and none of it is notebook authoring. Keeping the
  notebook out of this repo also keeps a generated artefact out of a tree
  that would otherwise diff to noise on every re-run.

- (2026-08-20) **Correction to the arch-bottleneck story, and the
  reporting model that follows from it.** Earlier entries here read the
  s390x collapse during a mass rebuild as system-wide pain. It is not:
  the rebuild queues behind itself, and the numbers that said otherwise
  were aggregates across classes of build that should never have been
  added together.

  Measured for all three rebuilds, official builds only, s390x queue
  wait: releng's own tasks sat at 2.0h (F43), 4.3h (F44) and 4.2h (F45)
  medians, while **everyone else's stayed at one minute** through every
  burst, exactly as in the weeks before and after. Koji priority is why —
  maintainers submit at 19-20, packit at 20, releng's rebuild at 25,
  koschei at 50, and lower is served first — so the rebuild is
  deliberately deprioritised beneath interactive work and absorbs its own
  delay. The ~60,000 s390x delay-hours per rebuild are a throughput cost
  to the rebuild's completion, not contributors being blocked. This also
  contradicts the crate docs' premise that "scratch builds, which gate
  dist-git PR CI, run at lower priority still": packit CI waited 6m
  against official's 4.2h in F45's burst.

  **So report per class, always, and never aggregate across them.** The
  classes differ in priority, in architecture coverage and in meaning:

  | class | priority | s390x tasks/1000 | what it is |
  |:--|--:|--:|:--|
  | releng | 25 | 588 | bulk, self-contending, deprioritised |
  | maintainer official | 19-20 | 588 | the thing that must stay fast, and does |
  | packit PR CI | 20 | 576 | contributor-facing latency |
  | scratch by hand | 20-50 | 544 | maintainer testing |
  | ELN sync | 25 | 504 | bulk, automated, its own calendar |
  | ELN fix by hand | 20 | 494 | packager repairing an ELN failure |
  | koschei | 50 | 0.1 | dependency canary, skips s390x |

  **And lead with the tail, not the median — the median describes an
  experience nobody had.** During F44's burst the population median was
  1 minute and every one of the 74 hour-plus waits belonged to a *single*
  maintainer, who submitted 75 builds and had a personal median of 8¼
  hours while 63 colleagues saw one minute. F45 has a build that waited
  48.7 hours. A heavy maintainer meets the p90 many times over in one
  session, and bulk work across a dependency set serialises — each build
  waits on the last — so a tail event compounds into days for exactly the
  people doing the most. A report should therefore give p90 and max, and
  ideally per-submitter counts, so it can say "one maintainer absorbed
  all 74 bad waits" rather than "p90 was 8 hours".

  Behavioural model, all three rebuilds agreeing: maintainers stand down
  during a burst — non-releng official volume falls to a quarter or a
  third (1,772→887, 1,911→379, 1,540→379) — and those who keep building
  are disproportionately fixing what just broke, enriched 2.2x to 5.5x
  for failed packages (18.3% vs 3.3%, 10.7% vs 4.8%, 8.6% vs 3.2%).
  That is also why the same test over the *fallout* window came out flat:
  by then everyone has resumed and repair work is diluted.

- (2026-08-20) **Four mass rebuilds measured, and the s390x story is
  capacity rather than slowness.** F42's window was collected on
  2026-08-20, which gives a rebuild from before whatever changed and turns
  the other three from a pattern into a comparison.

  s390x queue wait for the rebuild's *own* tasks, dated from who submitted
  rather than from the schedule:

  | release | observed window | median | p90 | max | over 1h |
  |:--|:--|--:|--:|--:|--:|
  | F42 | 2025-01-16..19 | **53s** | 7m | 56.4h | 1.8% |
  | F43 | 2025-07-23..26 | 2.0h | 5.1h | 12.1h | 92.6% |
  | F44 | 2026-01-16..19 | 4.2h | 7.8h | 22.7h | 95.3% |
  | F45 | 2026-07-15..18 | 3.8h | 8.3h | 18.2h | 95.0% |

  F42 did not queue. The other three queue for hours. The mechanism is
  measured end to end and each link is a separate observation:

  1. **Capacity was stable across the four windows** — 19-20 enabled hosts
     at 92-96 weight — but see the correction below: over the longer run it
     *grew*, and that changes what the figures ask for.
  2. **Service time rose, and only on s390x.** `buildArch` duration for
     rebuild tasks, F42 against F45: median 1.4m to 2.2m (1.56x), p90 5.1m
     to 10.9m (2.13x), mean 3.7m to 8.6m (2.34x). The control says this is
     not the toolchain or the package mix: over the same span x86_64 went
     *faster* (1.5m to 1.2m, 0.78x), aarch64 was flat (1.07x) and ppc64le
     slightly faster (0.92x).
  3. **So weighted utilisation climbed** — 54%, 72%, 79%, and over 100% at
     F45 — and queueing is nonlinear as utilisation approaches one, which
     is the whole of why seconds became hours.
  4. **On a shrinking workload.** s390x carried 15,900 rebuild tasks at F42
     and 12,651 at F45, while x86_64 held near 17,000 throughout. It is
     being excluded from more packages every cycle and saturates anyway.

  Unchanged across all four, and the thing to lead with when this is
  written up: **maintainers never feel it.** Official builds sat at a
  roughly one-minute median in every window, with 0.0%, 0.0%, 0.2% and
  5.4% of tasks waiting over an hour. The cost falls on the rebuild itself
  and then on the classes at or below its priority — CI (10-45% over an
  hour) and ELN's sync (an 8.1h median during F44).

  **Correction, from putting capacity in the store (below): Fedora did
  invest in s390x, and a service-time regression ate the investment.**
  Capacity rose from 57 weight across 26 hosts in January 2024 to 96 across
  20 by that December — including a March 2024 consolidation that halved
  the host count while raising total weight, taking per-host capacity from
  2.2 to 5.8. F42 in January 2025 was running on exactly that new headroom,
  which is why it sat at 54% utilisation and never queued. Service time
  then doubled between January and July 2025 with capacity unchanged at 93,
  consuming the whole increase.

  That reframes the ask. It is not "s390x has been starved of builders" —
  builders were added, twelve months before the regression, and it did not
  hold. The question to put to infrastructure is why per-task wall-clock
  doubled while the fleet stood still, and the candidates are the March
  2024 consolidation onto fewer, larger VMs (more concurrent builds per
  physical machine, hence more contention per build), storage, or the
  hypervisor — none of which this data can separate.

  Three things this cannot yet say. The regression is unbracketed between
  2025-01-19 and 2025-07-23, since F42 is the only window predating it —
  **collecting 2025-02 through 2025-04 would date it**, which is now the
  main reason to finish F42's cycle beyond completing it. Wall-clock
  service time cannot separate slower hardware from contention or storage;
  the co-location finding above is a candidate and unproven. And the
  utilisation figure exceeding 100% means the weight integral over-counts
  somewhere, so treat the trend as sound and the absolute number as not.

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

- (2026-08-20) **Backfill floor: F42's mass rebuild (2025-01-15).** The
  store holds two complete release cycles, F43 and F44; F45's finishes on
  2026-10-20. A third complete cycle is available now by going backwards
  rather than waiting, since F42 ran 2025-01-15 to its release on
  2025-04-15, entirely before the store's first day (2025-06-23). That
  needs 2024-12-15 .. 2025-06-22, about six months and an estimated 1GB at
  the store's observed 160MB/month.

  Do not go back further than that. Cost per page grows with depth when
  the hub is healthy — 0.6-1.6s a day back against 17.7-24.1s at thirteen
  months — and F42 is already the point where a cycle costs hours; earlier
  cycles buy less relevant data for more hub time. F42 also gives a fourth
  mass rebuild, which is the sample that matters.

  Cost at that depth, measured 2026-08-20 with `sync`'s own query shape:
  about 30-60s per 4000-row page against 7s recently, so roughly 25-40
  minutes of listing per month of data before pacing doubles it. The
  children stage dominates the total — around 8 minutes per day of builds
  — which puts six months at the better part of a day of hub time. That is
  fine: a sync resumes, so it can run across an outage and be picked up
  after.

- (2026-08-20) **Identify a service by its build target, not by the
  account that submits it.** Filtering on `owner` alone reported that ELN
  did no building whatsoever before January 2026 — 31 consecutive weeks
  of zero — when in fact it had been running throughout. The account was
  renamed: `distrobuildsync-eln/jenkins-continuous-infra.apps.ci.centos.org`
  hands over to `eln-buildsync` on 2026-01-08, with both appearing that
  day. An owner string is an operational detail that changes without
  notice, and when it changes a filter built on it reports absence rather
  than failing, which is the worst way for it to be wrong.

  `target` survives the rename: everything ELN builds goes to `eln`,
  `eln-candidate` or an `eln-build-side-NNNNNN` side tag. Measured that
  way the store holds 61,257 ELN sync builds across 14 months, plus 5,838
  from CI (`packit`, `zuul`) and 2,572 submitted by people — a packager
  whose package failed in ELN building it by hand rather than waiting for
  the next sync. Three classes, and the reports must not add them up.

  The same trap caught an earlier reading of the post-branch spike: F44's
  8,003-build day looked like maintainers racing a deadline and was 91%
  ELN, because the filter for "not a service account" tested for a `/` in
  the name and `eln-buildsync` has none. Any such filter needs an
  explicit list of what the services are, plus a target test.

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

- (2026-08-20) **Detect single-architecture stalls, which are a different
  event from a busy window and are currently invisible.** For about two
  days in May 2026 every Fedora build needing s390x waited roughly two
  days for it, while every other architecture was served in a minute.
  There was no mass rebuild that month and the load was ordinary.

  Measured by task creation day: s390x `buildArch` tasks created
  2026-05-06 waited a mean of **46.0 hours** and those created 05-07
  **38.3 hours**, against 0.1 hours on 05-05 and 1 minute again by 05-10;
  25 of them never started at all. On 05-08, when the backlog was
  draining at a 37-hour mean, x86_64, aarch64, ppc64le and i386 all sat
  at 1-2 minutes. Volume was normal all week (371 s390x tasks created on
  05-06 against 291 and 260 the days before), so this is availability,
  not congestion, and it hit ordinary maintainers rather than bulk work.

  This matters for how the s390x question gets framed: the architecture's
  worst days in the store are not its mass-rebuild days. 2,243 minutes
  beats every rebuild day measured, by five times.

  `crate::stall` now finds these — one architecture's daily mean exceeding
  the median of its peers tenfold, with an hour's floor and a 50-task
  minimum — and its first run over the store says May was not a freak but
  the extreme of a recurring pattern. **19 stalls in 14 months, 14 of them
  outside any rebuild window**, all but two on s390x (ppc64le twice, on
  2025-09-19 and 2025-11-11). So an ordinary month carries about one
  single-architecture stall, ranging from 1.3 hours at 31x the fleet up to
  May's 46 hours at 3,779x, and 1,600 tasks across those 14 events never
  ran at all.

  **Why each one happened is now answered from the store, and the answer
  is mostly "the builders were working".** Throughput separates the two
  reasons a queue grows, and it needed no new collection: 17 of the 19 are
  congestion, where the architecture ran several times its ordinary
  concurrency and still fell behind, and only **two are outages** — the
  May event, where s390x ran *nothing at all* on 05-07 against a queue of
  412 having managed 5.9 the day before, and a single ppc64le day on
  2025-11-11 at 6.7 against an ordinary 23.1.

  So the recurring monthly stalls are demand, not broken hardware, which
  reframes them: s390x has an ordinary throughput of 6.3 concurrent tasks
  against ppc64le's 23.1, and it falls an order of magnitude behind
  roughly monthly because that capacity is thin, not because it fails.

  **The two outages have different causes, and the hub's own history
  names one of them.** Koji keeps full `host_config` history —
  `queryHistory(tables=['host_config'])` returns 12,593 revisions with
  `enabled`, `capacity`, `arches` and validity timestamps, back to 2020 —
  so builder capacity is backfillable rather than something to start
  collecting.

  - **ppc64le, 2025-11-11: administrative.** Half the fleet was disabled
    from 11-06 to 11-11 — 32 hosts at capacity 64 down to 16 at 32 — and
    the hosts involved carry `rdu3` names, so this looks like the
    datacentre move. Throughput of 6.7 against an ordinary 23.1 is what
    running on half a fleet looks like, and calling it an outage in the
    sense of a failure overstates it.
  - **s390x, 2026-05-07: not administrative.** 18 hosts stayed enabled at
    capacity 90 and *none of them served a single task*. A storage
    problem, tracked as
    https://forge.fedoraproject.org/infra/tickets/issues/13326, which is
    exactly the shape such a failure leaves in this data: capacity
    present, throughput zero.

  What history does not keep is `ready`, the live check-in state, which
  distinguishes those two without recourse to memory. Only `listHosts`
  reports it and only for the present, so that remains the one thing a
  snapshot collector would genuinely add.

  (An earlier note here claimed the reconstruction was untrustworthy
  because it found 16 enabled ppc64le hosts on a day 29 served. The
  reconstruction was right and the check was wrong: it compared a noon
  snapshot against activity from the whole day, and 15 of those hosts were
  re-enabled at 23:04. Validated at matching instants, served never
  exceeds enabled.)

- (2026-08-20) **Which builders share a machine is inferable, and s390x is
  concentrated enough that it matters.** Hosts on one physical mainframe
  get maintained together, so simultaneous `host_config` changes cluster
  them — and for s390x the clusters are unambiguous:

  | group | hosts | changes in the same minute | enabled capacity |
  |:--|:--|--:|--:|
  | A | `buildvm-s390x-01..14` | 30 | 75.0 (82%) |
  | B | `buildvm-s390x-15..24` | 17 | 16.5 (18%) |
  | C | `buildvm-s390x-25..35` | — | all disabled |

  Fourteen hosts are not disabled together thirty times by coincidence, and
  group A's members are individually larger too (capacity 6.0 against
  group B's 3.0). So **82% of Fedora's s390x capacity shares one
  maintenance group**, and if that group is one physical machine, losing it
  costs four fifths of the architecture.

  The May outage was *not* that, though: both groups went to zero on the
  same day, so it was site- or storage-level rather than one mainframe.
  Which means the concentration is a latent risk the store has not yet
  seen realised — worth saying plainly in the write-up, and worth
  confirming against infrastructure rather than asserting from
  correlation alone, since a shared maintenance window is evidence of
  shared handling and not proof of shared hardware.

  Same method applies to the other architectures; ppc64le's November
  grouping (16 disabled at once) suggests it is similarly clustered.

  Still to do: label each stall against the rebuild windows in the output,
  which is what `reports --schedule` should emit.

- (2026-08-20) Retire "build volume" as a measure, and say why in the
  write-up: Fedora's largest single source of builds is almost invisible
  to the architecture everyone worries about.

  Measured over 2025-12, the busiest month in the store at 259,062
  builds: `koschei` submitted 239,188 of them and produced **0.1 s390x
  tasks per 1,000 builds** (22 `buildArch` tasks in all), against **588
  per 1,000** for everyone else.

  It is not sampling one architecture for cheapness — it builds x86_64
  (124,670), aarch64 (123,557) and ppc64le (121,699) in that month and
  skips s390x and i386 almost entirely. So the asymmetry is a policy
  fact, and it cuts both ways: s390x is spared several hundred thousand
  canary tasks a year, and it also gets far less continuous coverage, so
  dependency breakage there surfaces only when a real build meets it.

  Its builds also carry 2.22 child tasks each against 4.08, which is why
  that month's ratio looks anomalous, and part of that is builds that
  never reach an arch task at all: per the wiki, koschei will not attempt
  a build whose dependencies are Unresolved or whose package is Blocked
  in Koji.

  What koschei actually does (https://fedoraproject.org/wiki/Koschei) is
  worth stating in the write-up, because it explains the arrival pattern:
  it tracks dependency changes in Rawhide and rebuilds packages whose
  dependencies change too much, from the latest available SRPM, on a
  priority queue weighted by distance in the dependency graph. Its
  activity therefore reflects dependency churn rather than anyone
  updating a package, and it drains on its own schedule — which is how it
  reached 15,600 builds a day over Christmas with nothing else happening.

  All of it is `scratch=1`, and scratch is 78-95% of every month's builds,
  so any figure quoted as "builds" is mostly canary traffic unless it says
  otherwise. The reports already split official from scratch; on official
  builds only, the s390x median is 47-63s in every ordinary month and
  4,917s / 7,936s / 8,602s in the three rebuild months.

  So the busiest month in the store is one of the calmest for arch
  pressure, and the pairs that once looked paradoxical are not:
  2026-07-12's 10,611 builds ran at a 50s s390x median (98% koschei, about
  one s390x task in the lot) while 2026-07-17's 9,587 produced a 7.4-hour
  median (95% releng, which builds everything for everything). The
  quantity that predicts pain is s390x-bound work, and it barely
  correlates with total builds.

  Corollary for reading any monthly figure: a busy month can be one
  service, one week, or one person. December 2025 was koschei running hard
  over Christmas plus a handful of maintainers doing bulk work — human
  submitters dropped from ~69 a day to ~23 while each did roughly twice as
  much — and 2026-01-08's 1,211-build peak was mostly a single account.
  None of it touched s390x. The submitter breakdown is what makes a
  monthly number interpretable at all, which is a further argument for
  graduating `submitters-by-day.sql` into a report section.

  Keep individuals out of anything published: the aggregate ("two thirds
  of contributors step away over the holidays while the rest double their
  output") carries the point without profiling volunteers.

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

- (2026-08-19) Collect the two 2025 mass rebuilds, so the s390x
  signature can be checked across four consecutive ones rather than the
  two we have. From the schedule XML — which carries explicit "Mass
  Rebuild starts"/"ends" milestones, better than inferring a window:
  F42 2025-01-15 to 02-04, F43 2025-07-23 to 08-12, F44 2026-01-14 to
  02-03 (held), F45 2026-07-15 to 08-11 (held). Both held rebuilds
  collapsed s390x identically — median wait 6,777s and 6,141s, 12.3%
  and 13.0% of attributable builds, ~60,000 hours each — and both burned
  through in six days rather than the three weeks the schedule allots.
  Four in a row would settle whether that is the mechanism or a
  coincidence of two.

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

- (2026-08-18) Report on schedule windows, not just calendar periods.
  A mass rebuild or a freeze straddles month boundaries — F44's ran
  2026-01-14 to 02-03, F45's 2026-07-15 to 08-11 — so monthly reports
  cut them in half and weekly ones scatter them. `report --since/--until`
  already covers the ad-hoc case; what is missing is deriving the
  windows from Fedora's schedule and emitting them as named periods,
  e.g. `reports/events/f-45/mass-rebuild/`.
  - Source: the schedule repo (MS Project XML, `<Task>` with `<Name>`,
    `<Start>`, `<Finish>`, one directory per release back to F10). Read
    it by path with a `--schedule DIR` flag; do not vendor it. Fedora's
    generated HTML works too but is the wrong format to depend on.
  - Detection is already calibrated, so no threshold guessing is needed.
    `releng`'s share of a day's builds is sharply bimodal: across 267 days
    held, 254 sit **under 1%** and eleven sit **above 25%**, with only two
    days anywhere between. Any cut between about 2% and 25% separates them
    exactly. Require two consecutive days over the threshold so a small
    targeted rebuild is not mistaken for a mass one.
  - Submission takes 3-4 days whatever the schedule allots. F43's window
    ran 2025-07-23 to 08-12 on paper and the submissions were entirely
    within 07-23..26 — August 2025 holds none of it. F44 and F45 are the
    same. The schedule's end date is the branch date, not the rebuild's.
  - A rebuild has three phases and an event report should name them.
    **Submission** runs 3-4 days (F43 2025-07-23..26, F44 2026-01-16..18,
    F45 2026-07-15..18) at 79-95% `releng`, during which releng's own
    s390x tasks queue behind each other at 2.0-4.3h medians while
    everyone else's stay at one minute. Maintainers largely stand down —
    non-releng official volume falls to a quarter or a third — and those
    who keep building are enriched 2.2x to 5.5x for packages that just
    failed. **Fallout** runs another 3-4 days: the population returns (33
    → 42 → 74 → 80 distinct packagers across F45's aftermath) and repair
    work mixes into resumed ordinary building, which is why the
    failed-package enrichment washes out in that window even though it is
    strong during submission and again in week two. Fedora's schedule
    marks the phase with a "File FTBFS bugs from mass rebuild" milestone.
    Then a **long tail** of months; see the FTBFS entry.
  - An event report must apply the per-class model, or it will repeat the
    mistake corrected on 2026-08-20: aggregating releng's self-contention
    with everyone else's builds reports a four-hour median that describes
    only the rebuild waiting for itself. Report releng, maintainer
    official, packit CI, hand-submitted scratch and koschei separately,
    and lead with p90 and max rather than the median.
  - Consequence for the eventual write-up: spreading a rebuild would
    spread the fallout too, since a maintainer cannot start fixing until
    their package has failed. Four days of submission concentrates the
    human response into the four days after it. Note also what spreading
    would *not* fix — contributor-facing latency is already protected by
    priority, so the case for spreading rests on the rebuild's own
    completion time and on the repair burden, not on unblocking anyone.
  - Report the *observed* window beside the announced one. They differ:
    F45's was scheduled over four weeks and submitted in four days
    (2026-07-15..18), F44's likewise (2026-01-16..18, against a 01-14
    announced start), F43's exactly on its announced start
    (2025-07-23..26 against 07-23). Our own data dates it far better than
    the schedule does — the days where `releng` submits 79-95% of builds
    — and the gap between planned and actual is itself a finding. The
    schedule drifts too, visibly: its git log carries commits like
    "updating f48 schedule with correct dates".
  - Completeness already works for arbitrary windows (`Store::analysable`
    takes any from/to), so an event window that is only partly held will
    decline to report itself, as a month does.

  Ready to implement: 413 unbroken days are held (2025-06-23 to
  2026-08-10), covering F43's and F44's full cycles and F45 to branching,
  with three rebuild windows measured consistently.

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

- (2026-08-20) Two things left over from removing the JSON dataset path,
  both now unused outside their own tests: `Dataset::merge` and
  `DatasetMeta::schema_version`. `merge` is kept deliberately — unioning
  rows from more than one source is exactly what querying several stores
  needs, which is recorded above — but a version number for a format with
  no reader is only misleading, and should go with the next pass that can
  break the library API.

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
