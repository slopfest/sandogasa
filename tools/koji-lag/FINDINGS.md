# Fedora build capacity: what eighteen months of Koji data says

Source material for a write-up, not the write-up. Every figure here is
measured from a `koji-lag` store covering 2024-12-24 onward and is
reproducible from `notebooks/s390x-lag.ipynb`; the caveats are kept next to
the numbers deliberately, because several of the conclusions here replaced
earlier ones that looked just as solid.

Four mass rebuilds are in scope: F42 (2025-01-16..19), F43 (2025-07-23..26),
F44 (2026-01-16..19) and F45 (2026-07-15..18). Each window is dated from who
submitted each day rather than from the schedule, because the two disagree —
F45's was announced across four weeks and submitted in four days.

## 1. Where the bottleneck is

**s390x, and only for bulk work.** In every rebuild the architecture that
finishes last is s390x, and the delay lands on the rebuild itself rather
than on contributors.

| rebuild | rebuild's own s390x wait (median / p90) | maintainer official (median) | over 1h |
|:--|--:|--:|--:|
| F42 | 53s / 7m | ~1m | 0.0% |
| F43 | 2.0h / 5.1h | ~1m | 0.0% |
| F44 | 4.2h / 7.8h | ~1m | 0.2% |
| F45 | 3.8h / 8.3h | ~1m | 5.4% |

Koji priority explains it: maintainers submit at 19-20, packit CI at 20,
releng's rebuild at 25, koschei at 50, and lower is served first. A rebuild
is deliberately behind interactive work, so it absorbs its own delay. The
classes that *do* suffer alongside it are the ones at or below its priority
— CI (10-45% of tasks over an hour) and ELN's bulk sync (an 8.1h median
during F44).

**But "maintainers are fine" is a median talking, and the median describes
an experience nobody had.** Split human official builds by how much each
person submits and the cost lands very unevenly. July 2026, s390x:

| cohort | people | tasks | median | p90 | over 1h |
|:--|--:|--:|--:|--:|--:|
| top 10 submitters | 10 | 5,680 (67%) | 1.0m | **1.83h** | **12.8%** |
| next 40 | 40 | 1,594 | 0.8m | 3m | 5.5% |
| everyone else | 238 | 1,177 | 0.8m | 2m | 4.8% |

Ten people submit two thirds of the architecture's human workload, and their
p90 is thirty-seven times the next cohort's while every cohort's median stays
at about a minute. ppc64le shows the same shape more mildly — top ten at a
16m p90 and 2.7% over an hour, everyone else at a minute and 0.1%.

Inside a rebuild window it concentrates further still. During F45 the top ten
had a 23.0m p90 against 2.8m for the next forty, and of the 28 hour-plus
waits **26 fell on a single submitter**.

Two mechanisms compound for heavy maintainers, neither visible in a
population median. They meet the p90 many times in one session rather than
once. And bulk work across a dependency set serialises — each build waits on
the one before it — so a single tail event becomes a day.

**This is also the harm that is new.** In January 2025 every cohort sat at a
one-minute p90 with at most 0.1% of tasks over an hour; by January 2026 the
top ten were at 4m and 1.8%, and by July 2026 at 1.83h and 12.8%. The
even distribution was real and it has gone.

**Report per class, never aggregated.** An earlier reading of this data
produced a headline "60,000 s390x delay-hours per rebuild" that turned out
to describe releng waiting for releng. The classes differ in priority, in
architecture coverage and in what a delay means; koschei alone is 84% of all
builds and produces 7 s390x tasks per 1,000, so any figure quoted as
"builds" is mostly canary traffic unless it says otherwise.

## 2. The trend over time

**The cost of building has risen, and s390x absorbed it worst.** Comparing
the same packages in F42 and F45 — 9,952 that built successfully on s390x in
both, failures excluded — separates two independent effects.

| population | median ratio | p90 | mean |
|:--|--:|--:|--:|
| c++ | 1.53x | 2.74x | — |
| c | 1.40x | 2.19x | — |
| python | 1.35x | 2.44x | — |
| other | 1.28x | 2.06x | — |
| rust | **1.16x** | 1.88x | — |

Per architecture, with rust as the control population (it does not consume
Fedora's C flags):

| arch | rust baseline | c++ | c++ ÷ rust |
|:--|--:|--:|--:|
| s390x | **1.16x** | 1.53x | 1.31x |
| aarch64 | 0.94x | 1.13x | 1.20x |
| ppc64le | 0.73x | 0.96x | 1.32x |
| x86_64 | 0.69x | 0.79x | 1.14x |

- **A Fedora-wide compile cost.** C++ builds rose 1.14x to 1.32x against
  rust on *every* architecture. No platform fault can do that; compiler
  flags can, and the hardening Changes are the obvious candidate.
- **An s390x platform regression.** Its baseline rose 1.16x while every peer
  got *faster*, so s390x lost roughly 1.5x relative to the fleet on work
  involving no compiler flags at all. Cause unknown; the shape (sparing
  cargo and rustc, punishing fork-and-I/O-heavy autotools and C++) points at
  storage, the hypervisor or a kernel change rather than processor speed.

On s390x the two multiply: 1.16 × 1.31 = 1.52, against the 1.53x measured.

**Capacity did not stand still — which makes it worse, not better.** s390x
went from 57 weight across 26 hosts in January 2024 to 96 across 20 that
December, including a March consolidation onto fewer, larger builders. F42
ran on that new headroom at 54% utilisation and never queued. The regression
then consumed the entire increase within six months. Meanwhile ppc64le lost
half its capacity in July 2025 (132 weight to 64) and has not recovered it.

**Utilisation is the number that ties it together**, because queueing is
nonlinear in it:

| rebuild | offered weight | capacity | utilisation | observed median wait |
|:--|--:|--:|--:|--:|
| F42 | 52 | 96 | **0.54** | 53s |
| F43 | 67 | 93 | 0.72 | 2.0h |
| F44 | 73 | 93 | 0.79 | 4.2h |
| F45 | 114 | 92 | **1.24** | 3.8h |

Caveat worth carrying: F44 at 0.79 was *worse* than F45 at 1.24, so these
estimates carry real error and the M/M/1 formula only half-validates against
them. What the anchors support is a threshold, not a curve — below about 0.6
the waits are minutes, above about 0.7 they are hours.

**What is not the cause**, recorded so nobody re-derives it: Rust's share of
s390x rebuild tasks did rise, 18.5% to 26.1%, and it is not responsible —
rust packages build at the same median speed as everything else and slowed
least. Nor is the workload growing: s390x carried 15,900 rebuild tasks at
F42 and 12,651 at F45, while x86_64 held near 17,000. The architecture is
being excluded from more packages each cycle and saturates anyway.

## 3. Recommended capacity

Offered load equals delivered load to within a percent in all four windows,
so this is **headroom rather than throughput** — the fleet finishes the work,
and the hours of waiting come from releng submitting in bursts against a
queue with no slack.

At F45's offered 114 weight against today's 92:

| goal | capacity | vs today | extra builders (at 4.9 weight each) |
|:--|--:|--:|--:|
| utilisation ≤ 0.7 — hours become tens of minutes | 162 | 1.8x | **+14** |
| utilisation ≤ 0.6 — F42-like, waits in minutes | 190 | 2.1x | **+20** |
| utilisation ≤ 0.5 — survives a couple of cycles | 227 | 2.5x | +28 |

**But three cheaper things come first, in this order.**

1. **A per-task time limit, around 40-48 hours.** In F45 four tasks ran past
   50 hours and produced nothing: `libredwg` (FAILED, 56.1h) and `libunwind`
   (FAILED, 56.0h), which take under half an hour on every other
   architecture and so are genuine s390x faults; and `gnulib` (CANCELED,
   52.0h) and `m4` (CANCELED, 50.2h), which hung on all four architectures
   and were cancelled by hand. Those four are 214 builder-hours — **11.8% of
   the architecture's entire rebuild** — and a limit near 48h would reclaim
   all of it while touching nothing that succeeded (`gcc` is the longest
   legitimate build at 37.5h). Worth checking what Fedora's current limit
   actually is, since these ran past 50 hours.
2. **Fix the two s390x-specific package faults** above.
3. **Diagnose the platform regression.** Returning mean service time to
   F42's level drops offered load to 48 weight — utilisation 0.53 at today's
   capacity, waits back to minutes, no hardware at all. The regression is
   worth about twice the fleet, and capacity was already bought once and
   lost to it.

Two things to say alongside any purchase: it sizes for two weeks a year, so
pacing the rebuild over more days lowers peak utilisation at no cost and
belongs on the table; and the +20 figure has no growth margin, since
Fedora's package count only rises.

**On the compile cost, the ask is process rather than reversal.** A hardening
Change is a deliberate security trade-off and its cost does not make it
wrong. What the data supports is a future checklist item: a Change that
raises what every package costs to build should carry an estimate of its
build-capacity impact, with the constrained architectures called out. On an
architecture near saturation, 20% more per package is the difference between
queueing for seconds and queueing for hours. The method for producing that
estimate is the paired comparison in section 2.

## 4. Recommended monitoring

The metrics that found all of this are not the ones the periodic reports
produce. None of them need new collection, and that splits the work in two:
**what the tool should surface by itself**, and **what infrastructure has to
watch that a per-period report cannot see**. The first is the more useful
half, because an alert nobody has to remember to look for is the only kind
that works — and it gives infrastructure a reference implementation to
replicate rather than thresholds to invent.

### 4a. What `koji-lag report` should surface automatically

All of these are computable from the store today; the work is emitting them
per period with a threshold attached, so a monthly report says "s390x
utilisation 0.78, above the 0.7 line" without anybody running a query.

| metric | threshold | why this one |
|:--|:--|:--|
| weighted utilisation per arch | warn 0.6, act 0.7 | leads everything; nonlinear in wait |
| service time median / p90 / mean per arch | drift >20% year-on-year | localises a regression to an architecture |
| the same, against a rust control population | divergence between the two | separates platform from compiler cost |
| wasted builder-hours (failed + cancelled) | above 10% | pure loss, cheapest to recover |
| long-build tail share (tasks over 6h) | above 15% | catches hangs while they are few |
| per-class queue wait, never aggregated | maintainer p90 above 10m | the contributor-experience number |
| the same split by submitter volume (top 10 / next 40 / rest) | top-10 p90 above 20m, or above 5x the next cohort | a population median hid this entirely for eighteen months |
| single-arch stall events, congestion or outage | any outage | an event, not a trend |

Two implementation notes for whoever builds this. The rust control needs no
spec checkout if it keys on the `rust-` name prefix rather than on
`BuildRequires` — accurate enough for a control population, and it keeps the
report self-contained. And every one of these must be *per class*: a single
aggregated utilisation figure is how the original "60,000 delay-hours"
mistake happened.

### 4b. What infrastructure should watch that a report cannot

- **`ready` state per builder.** The hub keeps `enabled` in history but not
  `ready`, the live kojid check-in. That gap is exactly what made
  2026-05-07 ambiguous from the data alone: 18 hosts enabled, none serving.
  Only a poller catches that, and only going forward.
- **Storage and hypervisor metrics for the s390x fleet**, since the
  regression's shape points there and build wall-clock cannot distinguish
  the causes.
- **Physical host placement.** Configuration history implies
  `buildvm-s390x-01..14` are maintained together and hold 82% of enabled
  capacity, but that is shared handling rather than proof of shared
  hardware. Infrastructure can confirm it in a minute and it matters: if
  that group is one machine, losing it costs four fifths of the
  architecture.

## Open questions, and where other expertise would help

Stated as questions rather than caveats on purpose: each is something this
data cannot settle, and most of them somebody else can — the point of
publishing them is to find that person rather than to hedge.

- **When did the platform regression start?** Bracketed between 2025-01-19
  and 2025-07-23, because F42 is the only rebuild predating it. Collecting
  2025-02 to 2025-06 closes this to within a month and is in progress. Once
  it is dated, whoever knows what changed on those builders that month can
  probably name the cause in a sentence.
- **Why did it start?** Build wall-clock cannot separate slower hardware
  from storage contention or a hypervisor or kernel change. The shape is a
  clue rather than an answer: it spares cargo and rustc, which are CPU-bound
  and spawn few processes, and doubles autotools and C++ builds, which fork
  constantly and hammer the filesystem. Somebody who administers those
  machines will read that differently from us.
- **Is `buildvm-s390x-01..14` one physical machine?** Configuration history
  shows those fourteen maintained together thirty times over, holding 82% of
  enabled capacity. That is shared *handling*, and inferring shared hardware
  from it would be overreach. Infrastructure can confirm or deny it quickly,
  and the answer matters: if it is one machine, losing it costs four fifths
  of the architecture.
- **How exact are the utilisation figures?** The weight integral reads above
  100% at F45, and F44 at 0.79 measured *worse* than F45 at 1.24 — so the
  ordering is not perfectly monotone and the absolute values are
  approximate. The threshold framing (minutes below 0.6, hours above 0.7) is
  what the measured anchors support; a queueing theorist could probably do
  better with the same data.
- **What is Fedora's actual per-task build limit?** Four tasks ran past 50
  hours in F45, so either there is no cap or it is higher than that. This is
  a configuration question we could not answer from the outside.
- **Did the SRPM-on-s390x misconfiguration recur?** It ran for about ten
  months to 2025-10-24 and is absent from 2026-08-11..19, but those episodes
  came in bursts tied to rebuilds, so nine ordinary days prove little. The
  next mass rebuild is the real test.
- **Is the compile-cost increase actually the hardening Changes?** The
  evidence is a per-architecture pattern consistent with compiler flags and
  inconsistent with a platform fault. It is not a bisection over flag sets,
  which is what would settle it — and somebody on the toolchain side could
  do that far faster than we could.
