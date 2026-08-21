// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The signals that say an architecture is in trouble, and the thresholds
//! that say so out loud.
//!
//! Everything here was found by hand first, over eighteen months of stored
//! data, and none of it would have been noticed by anybody who did not go
//! looking — which is the argument for computing it every period instead.
//! See DEVELOPMENT.md, "Anything worth reporting belongs in `report`".
//!
//! Three of these exist because a population median hid something real:
//!
//! - **Per class.** Adding a mass rebuild's queue wait to a maintainer's
//!   produced a "60,000 delay-hours" figure that turned out to describe
//!   releng waiting for releng. The classes differ in priority and in what
//!   a delay means, so they are never summed.
//! - **Per cohort.** In July 2026 ten people submitted 67% of Fedora's
//!   human s390x workload at a 1.83h p90 while *every* cohort's median sat
//!   at about a minute. Heavy maintainers meet the p90 many times in one
//!   session, and bulk work across a dependency set serialises, so one tail
//!   event becomes a day. A median over submitters cannot show this.
//! - **Wasted and tail share.** Four tasks that ran past fifty hours and
//!   produced nothing took 11.8% of one rebuild's s390x capacity. They are
//!   invisible in a duration median and obvious as a share of builder-hours.

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_kojihub::hub::{TASK_CANCELED, TASK_CLOSED, TASK_FAILED};
use serde::{Deserialize, Serialize};

use crate::class::{self, Class};
use crate::dataset::{BUILD_ARCH, BuildRecord, Dataset, TaskRecord};
use crate::stats::{DistSummary, median, percentile, summarize};

/// Builder time in a single task beyond which it is treated as tail rather
/// than as a build.
///
/// Six hours: the longest legitimate build seen in a Fedora mass rebuild is
/// `gcc` at 37.5h, so this does not separate "long" from "broken" — it
/// separates the handful of tasks that dominate an architecture's capacity
/// from the thousands that do not.
pub const TAIL_SECS: f64 = 6.0 * 3600.0;

/// One submitter and the queue waits they saw, ranked by volume.
type Submitter<'a> = (&'a &'a str, &'a Vec<f64>);

/// Cohort band edges by submission volume, as `(upper bound, label)`.
///
/// Finer than the original top-10/next-40 split, because measuring it
/// showed the cliff falls *inside* the first band: on s390x in July 2026 the
/// busiest five had a 189.5m p90 with 31.3% of their builds over an hour
/// while ranks 6 to 10 had 9.8m and 5.6%.
///
/// Rank remains a proxy, and a leaky one — see [`ArchHealth::submitters_slow`].
const BANDS: &[(usize, &str)] = &[(5, "top-5"), (10, "6-10"), (20, "11-20"), (50, "21-50")];

/// Tasks a population needs before a threshold may fire on it.
///
/// Not presentation, unlike `--min-samples`: a share computed over three
/// tasks is arithmetic rather than evidence, and one seven-hour build on a
/// quiet day would otherwise report a 90% tail and send somebody hunting.
/// Twenty is small enough that a real day of any architecture clears it and
/// large enough that a single task cannot carry a warning on its own.
const MIN_FOR_WARNING: usize = 20;

/// Queue wait for one class of build on one architecture.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClassStats {
    pub arch: String,
    /// [`Class::slug`].
    pub class: String,
    pub queue_wait: Option<DistSummary>,
    /// Share of this class's tasks that waited over an hour.
    pub over_hour_pct: f64,
}

/// Queue wait for one band of submitters on one architecture.
///
/// Bands are by volume, not by identity: nobody is named, which is what
/// makes this publishable. "How badly am I affected" is `report --owner`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CohortStats {
    pub arch: String,
    /// `top-10`, `next-40`, or `rest`.
    pub cohort: String,
    pub people: usize,
    pub queue_wait: Option<DistSummary>,
    pub over_hour_pct: f64,
    /// Share of the architecture's human official tasks this band submitted.
    pub share_of_tasks: f64,
}

/// Build toolchains, guessed from the package name.
///
/// Fedora names most of its language ecosystems by prefix, which makes a
/// usable toolchain split available in the store with no spec checkout and
/// no `BuildRequires` scan. Each family is then compared only with *its own*
/// earlier self, so what the prefix misses matters much less than it would
/// for a census: `golang-` catches Go libraries and not Go applications, and
/// a Go application's cost still shows up somewhere, just under `other`.
///
/// Measured on s390x's F45 rebuild, as a share of its 12,612 compiles:
/// rust 26.0%, other 56.2%, haskell 4.8%, perl 3.8%, golang 3.4%,
/// python 2.4%, ocaml and r 1.3% each, ruby 0.4%.
///
/// **`golang` is not usable as a population and is expected to keep
/// shrinking.** Fedora vendors Go dependencies by default from F43, so the
/// `golang-` library packages are being retired rather than rebuilt: 1,434
/// of them built in F42's mass rebuild, 1,321 in F43's, 613 in F44's and 435
/// in F45's. What remains is Go *applications*, which do not carry the
/// prefix. Detecting those needs the spec — `BuildRequires: go-vendor-tools`
/// — or a `fedrq` query, neither of which belongs in a metric computed from
/// the store, and Go's compiler is fast enough that it is an unlikely place
/// for a build-time problem anyway. The population check in
/// [`crate::trend`] reports the family as not comparable, which is the
/// correct outcome without any of that work.
///
/// See <https://docs.fedoraproject.org/en-US/packaging-guidelines/Golang/>.
pub const FAMILIES: &[(&str, &[&str])] = &[
    ("golang", &["golang-"]),
    ("haskell", &["ghc-"]),
    ("ocaml", &["ocaml-"]),
    ("perl", &["perl-"]),
    ("python", &["python-", "python3-"]),
    ("r", &["R-"]),
    ("ruby", &["rubygem-"]),
    // `cosmic-` is a Rust desktop, published without the prefix.
    ("rust", &["rust-", "cosmic-"]),
];

/// Everything the prefixes do not name, which is mostly C and C++.
pub const OTHER_FAMILY: &str = "other";

/// Which toolchain a package name suggests.
pub fn family_of(package: &str) -> &'static str {
    FAMILIES
        .iter()
        .find(|(_, prefixes)| prefixes.iter().any(|p| package.starts_with(p)))
        .map_or(OTHER_FAMILY, |(name, _)| name)
}

/// How long one toolchain's packages took to build, within one window.
///
/// Read **down the column across periods, never across the row within
/// one**. A family's own change over time is a fact about that family; the
/// gap between two families in the same window is mostly how big their
/// packages are. On s390x in July 2026 the Rust packages averaged 3.8
/// minutes against 8.9 for everything else, a 2.3x gap that says only that
/// crates are small — their *medians* were within seconds of each other.
///
/// See [`crate::trend`], which compares each family with its own earlier
/// self and nothing else.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Population {
    /// The toolchain family, from [`family_of`].
    pub name: String,
    pub tasks: usize,
    /// Median and p90 build time in seconds, `None` below a usable count.
    pub p50: Option<f64>,
    pub p90: Option<f64>,
}

/// How an architecture's builder time was spent, and how close to full it
/// ran.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ArchHealth {
    pub arch: String,
    /// Enabled builder weight during the window, from the hub's
    /// configuration history. `None` when the store has no capacity for
    /// this architecture, or the selection has no window.
    ///
    /// **This is capacity *able to serve* the architecture, not capacity
    /// dedicated to it**, and the difference matters wherever hosts serve
    /// more than one. Fedora's i386 builders are its x86_64 builders — every
    /// one of the 33 hosts that took i386 work also took x86_64 work, and
    /// the hub records them as `i386 x86_64` — so both architectures' totals
    /// include the same machines and both utilisation figures understate how
    /// busy the hardware is. During F45's rebuild that reads as i386 0.19
    /// and x86_64 0.35 against a combined 0.52 on the metal.
    ///
    /// s390x and ppc64le run on hosts of their own, so for them the two
    /// readings coincide and a capacity ask computed from this is sound.
    /// Do not read a low figure on a shared architecture as spare hardware.
    ///
    /// The architectures are still reported separately, because a task's
    /// wait is a fact about that task whoever's machine it ran on. It is
    /// *utilisation* that needs the shared denominator, and it has one: see
    /// [`ArchHealth::pool`], which is what `utilisation` divides by.
    pub capacity: Option<f64>,
    /// Mean weight in use: task weight integrated over the window and
    /// divided by its length, so it compares with `capacity` directly.
    pub offered_weight: Option<f64>,
    /// The builder pool this architecture belongs to, as a space-separated
    /// list — `s390x`, or `i386 i686 x86_64`. `None` when the store holds no
    /// host configuration for it.
    #[serde(default)]
    pub pool: Option<String>,
    /// The pool's enabled weight, each host counted once, and the offered
    /// weight of every architecture in it.
    #[serde(default)]
    pub pool_capacity: Option<f64>,
    #[serde(default)]
    pub pool_offered: Option<f64>,
    /// `pool_offered / pool_capacity`, counting only work that competes.
    ///
    /// Computed over the *pool* and not this architecture alone, so every
    /// architecture sharing hosts reports the same figure — which is the
    /// truth about those machines. Per architecture it read i386 0.19 and
    /// x86_64 0.35 during F45's rebuild while the hardware they share was at
    /// 0.52, and 136 weight units of i386 headroom appeared to exist that
    /// could not be redeployed because it was x86_64's headroom counted
    /// twice.
    ///
    /// Queueing is nonlinear in this, which is why it leads every other
    /// signal here: across four Fedora mass rebuilds it read 0.56, 0.72,
    /// 0.78 and 1.19 while the wait those rebuilds actually saw went 53s,
    /// 2.0h, 4.2h and 3.8h.
    ///
    /// It is not sufficient on its own, which is why the wait columns are
    /// still reported beside it: a fleet can be nominally full of work that
    /// yields, and then nobody waits. See [`Class::filler`].
    pub utilisation: Option<f64>,
    /// Tasks with both timestamps, i.e. those the shares are computed over.
    pub tasks: usize,
    pub builder_hours: f64,
    /// Share of builder time in tasks that failed or were cancelled —
    /// capacity spent producing nothing.
    pub wasted_pct: f64,
    /// Share of builder time in tasks longer than [`TAIL_SECS`].
    pub tail_pct: f64,
    pub tail_tasks: usize,
    /// Submitters with at least [`MIN_FOR_WARNING`] official builds here,
    /// and how many of those had **their own** p90 queue wait over an hour.
    ///
    /// The cohort bands rank by volume, and rank is a leaky proxy for who is
    /// affected: on s390x in July 2026 the submitters at ranks 1, 4, 5, 10
    /// and 12 had p90 waits of 211, 213, 84, 153 and 132 minutes while ranks
    /// 2, 3, 6 to 9 and 11 all sat between 1 and 13. Rank 12 is hit and rank
    /// 11 is not, so the affected set is defined by what people build rather
    /// than by how much of it.
    ///
    /// Counting them directly names nobody, so it stays as publishable as a
    /// band, and it does not depend on the bands being drawn in the right
    /// place.
    #[serde(default)]
    pub submitters: usize,
    #[serde(default)]
    pub submitters_slow: usize,
    /// Share of this architecture's official human tasks submitted by those
    /// people. Quote this rather than the count.
    ///
    /// The count is sensitive to [`MIN_FOR_WARNING`] in a way the share is
    /// not, because lowering the floor admits people with few builds — who
    /// add to the count while contributing almost nothing to the share.
    /// Over July 2026 on s390x the tool reports 7 of 47 regular submitters
    /// holding 20% of human builds; a looser floor over a slightly wider
    /// population puts it nearer half. Both say the same thing about who
    /// carries the delay, and neither is the "right" count, which is the
    /// argument for reporting the share.
    #[serde(default)]
    pub submitters_slow_task_pct: f64,
    /// Build time split by [`CONTROL_PREFIX`]. An ingredient for
    /// [`crate::trend`]; not interpretable within one window on its own.
    pub service: Vec<Population>,
}

/// How much of an architecture's builder time went to each distribution.
///
/// The measure a scope decision needs. Adding builders and narrowing what an
/// architecture is built for are alternative answers to the same queue, and
/// they are not comparable until the second one has a number: on s390x, ELN
/// and EPEL together hold about a fifth of builder hours, so the other four
/// fifths is what narrowing would free — a bigger change than any plausible
/// hardware order, and a policy decision rather than an efficiency one.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StreamShare {
    pub arch: String,
    /// `fedora`, `eln` or `epel`, from [`Class::stream_of`].
    pub stream: String,
    pub tasks: usize,
    pub builder_hours: f64,
    /// Share of this architecture's builder hours in the window.
    pub pct: f64,
}

/// Multi-arch builds are only finished when their slowest architecture is,
/// and this is the count of times each was the slowest.
///
/// The metric a per-architecture view cannot show. Each architecture can
/// look healthy on its own terms — its own queue short, its own builders
/// busy — while one of them sets the completion time of nearly every build,
/// because a build is not output until every architecture it targets has
/// produced its share. Fedora's F45 mass rebuild had s390x finishing last
/// for 91.7% of builds, which spent a median 4.0 hours with every other
/// architecture already finished, so the rest of the fleet was done while
/// s390x still had some two hundred builds to grind through.
///
/// Beware the obvious wrong version of this, which is to read the last
/// *timestamp* per architecture: in that same rebuild aarch64's final task
/// landed eight hours after s390x's, on the strength of two builds, one of
/// which was a six-minute build submitted a day after the rest.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Straggler {
    pub arch: String,
    /// Builds this architecture finished last.
    pub builds: usize,
    /// Share of the multi-arch builds considered.
    pub pct: f64,
    /// Gap between the first and last architecture finishing, over the
    /// builds this one came last in — what the others spent already
    /// finished.
    ///
    /// Distributed, not averaged. The spread has a long tail and a mean
    /// sits well above the typical build: over F45's rebuild the builds
    /// s390x came last in had a median spread of 4.0 hours, a p90 of 8.6
    /// and a maximum of 63.5, so the mean of 5.1 describes neither the
    /// ordinary build nor the bad one.
    pub spread: Option<DistSummary>,
}

/// Architectures a build must reach before it counts as multi-arch here.
///
/// Three rather than two so a package built for one primary architecture
/// plus one other does not read as the whole fleet waiting on it.
pub const MULTIARCH_MIN: usize = 3;

/// Share of builds one architecture may finish last before it is worth
/// saying so. Above half, it is setting the pace for the whole fleet.
pub const STRAGGLER_WARN_PCT: f64 = 50.0;

/// A threshold that has been crossed, phrased for a reader.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Warning {
    /// Short machine-readable key, e.g. `wasted-share`.
    pub metric: String,
    /// What it is about: an architecture, or an architecture and cohort.
    pub subject: String,
    pub text: String,
}

/// Everything in this module, for one report.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Health {
    pub arches: Vec<ArchHealth>,
    /// Which architecture finished each multi-arch build last.
    #[serde(default)]
    pub stragglers: Vec<Straggler>,
    /// Where each architecture's builder time went, by distribution.
    #[serde(default)]
    pub streams: Vec<StreamShare>,
    pub classes: Vec<ClassStats>,
    pub cohorts: Vec<CohortStats>,
    pub warnings: Vec<Warning>,
}

/// Compute the health signals for a selection of tasks.
///
/// `selected` is what the report is already reporting on, so a narrowed
/// report narrows these too rather than quietly widening back to everything.
pub fn assess(
    dataset: &Dataset,
    selected: &[&TaskRecord],
    window: Option<(f64, f64)>,
    cohorts_wanted: bool,
) -> Health {
    let build_of = |task: &TaskRecord| -> Option<&BuildRecord> {
        dataset
            .builds
            .get(&format!("{}:{}", task.instance, task.parent?))
    };

    // (builder secs, wasted secs, tail secs, tail tasks, tasks, weight secs)
    let mut per_arch: BTreeMap<&str, (f64, f64, f64, usize, usize, f64)> = BTreeMap::new();
    let mut per_class: BTreeMap<(&str, Class), Vec<f64>> = BTreeMap::new();
    // Human official work only, since a cohort of submitters means people.
    let mut per_person: BTreeMap<&str, BTreeMap<&str, Vec<f64>>> = BTreeMap::new();
    // (arch, stream) -> (tasks, builder secs).
    let mut per_stream: BTreeMap<(&str, &'static str), (usize, f64)> = BTreeMap::new();
    // Candidate rows for the toolchain split, held until every build's
    // architecture count is known: (build key, arch, family, secs).
    let mut candidates: Vec<((&str, i64), &str, &'static str, f64)> = Vec::new();
    // parent build -> (arch, completion) for each architecture it reached.
    let mut per_build: BTreeMap<(&str, i64), Vec<(&str, f64)>> = BTreeMap::new();

    for task in selected {
        let build = build_of(task);
        let cls = build.map(class::of_build);
        if let (Some(start), Some(done)) = (task.start_ts, task.completion_ts) {
            let secs = (done - start).max(0.0);
            let e = per_arch.entry(&task.arch).or_default();
            e.0 += secs;
            e.4 += 1;
            // Weight, not task count: Koji schedules against a host's
            // weight capacity, and a buildArch task weighs anywhere from
            // 1.5 to 6. Clamped to the window so a build spanning its edge
            // is charged only for the part inside.
            //
            // Filler classes are excluded, and this is the difference
            // between a useful number and a monthly false alarm. koschei
            // runs at priority 50 and is *designed* to occupy builders that
            // would otherwise idle: in March 2026 it was 177,966 of
            // ppc64le's tasks, which put naive utilisation at 1.39 while
            // every class on that architecture sat at a one-minute median
            // and 0.0% of tasks over an hour. Counting work that yields to
            // everything as pressure measures the opposite of pressure.
            if let Some((from, to)) = window
                && !cls.is_some_and(Class::filler)
            {
                let overlap = (done.min(to) - start.max(from)).max(0.0);
                e.5 += overlap * task.weight.unwrap_or(1.0);
            }
            if task.state == TASK_FAILED || task.state == TASK_CANCELED {
                e.1 += secs;
            }
            if secs > TAIL_SECS {
                e.2 += secs;
                e.3 += 1;
            }
            // Successful builds only: a build that failed or was killed
            // stopped when it stopped, and averaging that with work that
            // ran to completion measures the failure, not the toolchain.
            // `buildArch` only, for both of the metrics below. An
            // architecture's compile cost is what it compiled, and a
            // `buildSRPMFromSCM` task is not compilation -- it is a
            // checkout and a tarball, taking seconds. Worse, its `arch` is
            // the host the hub happened to pick rather than anything the
            // build targets, so counting it attributes another
            // architecture's work to this one.
            //
            // Measured: F42's s390x rebuild carried 5,980 SRPM tasks
            // against 14,632 compiles, and F45's carried none, because the
            // misassignment that produced them was corrected in October
            // 2025. Including them dragged F42's median down and reported a
            // 1.50x platform regression where the compile-only figure is
            // 1.25x. Divergence survived it (1.30x against 1.31x, the
            // ratio of ratios cancelling a common contamination), which is
            // the only reason the conclusion did.
            //
            // Every task, whatever its method: this asks where an
            // architecture's builder time went, and an SRPM checkout spends
            // builder time like anything else.
            let e = per_stream
                .entry((
                    task.arch.as_str(),
                    Class::stream_of(build.and_then(|b| b.target.as_deref())),
                ))
                .or_default();
            e.0 += 1;
            e.1 += secs;

            // Scoped to these two metrics rather than skipping the task:
            // utilisation, builder hours, the per-class queue wait and the
            // cohorts all keep the SRPM work, because it occupies a builder
            // and waits in a queue whatever it is doing.
            if task.method == BUILD_ARCH {
                if let Some(parent) = task.parent {
                    per_build
                        .entry((&task.instance, parent))
                        .or_default()
                        .push((&task.arch, done));
                }
                if task.state == TASK_CLOSED
                    && let Some(pkg) = build
                        .and_then(|b| b.package.as_deref())
                        .or(task.package.as_deref())
                    && let Some(parent) = task.parent
                {
                    candidates.push((
                        (task.instance.as_str(), parent),
                        task.arch.as_str(),
                        family_of(pkg),
                        secs,
                    ));
                }
            }
        }
        let Some(wait) = task.start_ts.map(|s| (s - task.create_ts).max(0.0)) else {
            continue;
        };
        let Some((build, cls)) = build.zip(cls) else {
            continue;
        };
        per_class.entry((&task.arch, cls)).or_default().push(wait);
        if cls == Class::Official
            && let Some(owner) = build.owner.as_deref()
        {
            per_person
                .entry(&task.arch)
                .or_default()
                .entry(owner)
                .or_default()
                .push(wait);
        }
    }

    // A build that produced exactly one architecture's task is not that
    // architecture's work. Koji records a noarch build's `buildArch` task
    // against whichever host it picked, so `python-requests` appears under
    // aarch64 in one rebuild and somewhere else in the next -- and it
    // compiled nothing either time.
    //
    // Measured, and this is why it is excluded rather than noted: s390x
    // hosted 2,291 of them in F42's rebuild at a 1.44m mean against 4.15m
    // for real compiles, and 5 in F45's. That alone moved its apparent
    // population by 20% and its median by a fifth. With them out, F42 and
    // F45 compare 9,430 against 9,327 tasks -- the like-for-like comparison
    // the metric was supposed to be.
    //
    // It over-excludes: a package with `ExclusiveArch: x86_64` also produces
    // one task and is genuinely that architecture's work. Accepted, because
    // the error is one of omission rather than of mixing another
    // architecture's cheap work into this one's compile cost.
    // Counted over the whole dataset rather than over `selected`, because
    // whether a build is noarch is a fact about the build and not about
    // what this report was narrowed to. Counting the selection instead made
    // `report --arch s390x` see every build as single-architecture and
    // silently emptied the table.
    let mut arch_count: BTreeMap<(&str, i64), BTreeSet<&str>> = BTreeMap::new();
    for task in dataset.tasks.values() {
        if task.method == BUILD_ARCH
            && let Some(parent) = task.parent
        {
            arch_count
                .entry((task.instance.as_str(), parent))
                .or_default()
                .insert(task.arch.as_str());
        }
    }
    let mut per_pop: BTreeMap<(&str, &'static str), Vec<f64>> = BTreeMap::new();
    for (key, arch, family, secs) in candidates {
        if arch_count.get(&key).map_or(0, BTreeSet::len) > 1 {
            per_pop.entry((arch, family)).or_default().push(secs);
        }
    }

    let over_hour = |xs: &[f64]| match xs.len() {
        0 => 0.0,
        n => 100.0 * xs.iter().filter(|w| **w > 3600.0).count() as f64 / n as f64,
    };

    // Per architecture: submitters, and those whose own p90 is over an hour.
    let slow_submitters: BTreeMap<&str, (usize, usize, f64)> = per_person
        .iter()
        .map(|(arch, people)| {
            // A p90 over three builds is noise, so only submitters with
            // enough work to have a distribution are judged.
            let eligible: Vec<&Vec<f64>> = people
                .values()
                .filter(|w| w.len() >= MIN_FOR_WARNING)
                .collect();
            let slow: Vec<&&Vec<f64>> = eligible
                .iter()
                .filter(|waits| {
                    let mut xs = (**waits).clone();
                    xs.sort_by(f64::total_cmp);
                    percentile(&xs, 0.9).is_some_and(|p| p > 3600.0)
                })
                .collect();
            let all: usize = people.values().map(Vec::len).sum();
            let held: usize = slow.iter().map(|w| w.len()).sum();
            let pct = if all > 0 {
                100.0 * held as f64 / all as f64
            } else {
                0.0
            };
            (*arch, (eligible.len(), slow.len(), pct))
        })
        .collect();

    // Offered weight per architecture, then summed per pool: the
    // denominator is shared, so the numerator has to be too.
    let offered_of = |arch: &str, weight_secs: f64| -> Option<f64> {
        let _ = arch;
        window.and_then(|(from, to)| {
            let span = to - from;
            (span > 0.0).then_some(weight_secs / span)
        })
    };
    let pool_of: BTreeMap<&str, &crate::dataset::Pool> = dataset
        .pools
        .iter()
        .flat_map(|p| p.arches.iter().map(move |a| (a.as_str(), p)))
        .collect();
    let mut pool_offered: BTreeMap<&str, f64> = BTreeMap::new();
    for (arch, (_, _, _, _, _, weight_secs)) in &per_arch {
        if let Some(pool) = pool_of.get(*arch)
            && let Some(o) = offered_of(arch, *weight_secs)
        {
            *pool_offered.entry(pool.arches[0].as_str()).or_default() += o;
        }
    }

    let arches: Vec<ArchHealth> = per_arch
        .iter()
        .map(
            |(arch, (total, wasted, tail, tail_n, tasks, weight_secs))| {
                let capacity = dataset.capacity.get(*arch).copied();
                let offered = offered_of(arch, *weight_secs);
                let pool = pool_of.get(*arch);
                let pooled = pool.and_then(|p| pool_offered.get(p.arches[0].as_str()).copied());
                ArchHealth {
                    arch: (*arch).to_string(),
                    capacity,
                    offered_weight: offered,
                    pool: pool.map(|p| p.arches.join(" ")),
                    pool_capacity: pool.map(|p| p.capacity),
                    pool_offered: pooled,
                    utilisation: match (pooled, pool.map(|p| p.capacity)) {
                        (Some(o), Some(c)) if c > 0.0 => Some(o / c),
                        // No host configuration: fall back to this
                        // architecture's own figures rather than reporting
                        // nothing, since single-arch fleets are the common
                        // case and the two agree there.
                        _ => match (offered, capacity) {
                            (Some(o), Some(c)) if c > 0.0 => Some(o / c),
                            _ => None,
                        },
                    },
                    tasks: *tasks,
                    builder_hours: total / 3600.0,
                    wasted_pct: if *total > 0.0 {
                        100.0 * wasted / total
                    } else {
                        0.0
                    },
                    tail_pct: if *total > 0.0 {
                        100.0 * tail / total
                    } else {
                        0.0
                    },
                    tail_tasks: *tail_n,
                    submitters: slow_submitters.get(*arch).map_or(0, |x| x.0),
                    submitters_slow: slow_submitters.get(*arch).map_or(0, |x| x.1),
                    submitters_slow_task_pct: slow_submitters.get(*arch).map_or(0.0, |x| x.2),
                    service: per_pop
                        .iter()
                        .filter(|((a, _), _)| a == arch)
                        .map(|((_, family), xs)| {
                            let mut xs = xs.clone();
                            xs.sort_by(f64::total_cmp);
                            Population {
                                name: (*family).to_string(),
                                tasks: xs.len(),
                                p50: median(&xs),
                                p90: percentile(&xs, 0.9),
                            }
                        })
                        .collect(),
                }
            },
        )
        .collect();

    // Who came last, and what the rest of the fleet spent waiting for it.
    let mut last: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
    let mut multiarch = 0usize;
    for arches_done in per_build.values() {
        let distinct: BTreeSet<&str> = arches_done.iter().map(|(a, _)| *a).collect();
        if distinct.len() < MULTIARCH_MIN {
            continue;
        }
        // Last per architecture first: an architecture that built several
        // subpackages should be judged by when it actually finished.
        let mut latest: BTreeMap<&str, f64> = BTreeMap::new();
        for (arch, done) in arches_done {
            let e = latest.entry(arch).or_insert(*done);
            *e = e.max(*done);
        }
        let Some((arch, max)) = latest.iter().max_by(|a, b| a.1.total_cmp(b.1)) else {
            continue;
        };
        let min = latest.values().copied().fold(f64::INFINITY, f64::min);
        multiarch += 1;
        last.entry(arch).or_default().push(max - min);
    }
    let mut stragglers: Vec<Straggler> = last
        .into_iter()
        .map(|(arch, mut spreads)| Straggler {
            arch: arch.to_string(),
            builds: spreads.len(),
            pct: if multiarch > 0 {
                100.0 * spreads.len() as f64 / multiarch as f64
            } else {
                0.0
            },
            spread: summarize(&mut spreads),
        })
        .collect();
    stragglers.sort_by_key(|s| std::cmp::Reverse(s.builds));

    let classes: Vec<ClassStats> = per_class
        .into_iter()
        .map(|((arch, cls), mut waits)| ClassStats {
            arch: arch.to_string(),
            class: cls.slug().to_string(),
            over_hour_pct: over_hour(&waits),
            queue_wait: summarize(&mut waits),
        })
        .collect();

    let mut cohorts = Vec::new();
    // Bands compare submitters with each other, which is not a question a
    // report narrowed to chosen accounts asked. It also answers it wrongly:
    // `--owner NAME` reported "the ten busiest submitters carry 100% of
    // human builds" about one person, which is the filter restated as a
    // finding about the fleet. That person's own wait is in the per-arch
    // rows already.
    //
    // Not a floor on how many submitters there are: ten submitters on a
    // small instance still all land in the top band, and their p90 is a
    // real finding about them even with no band to compare against.
    for (arch, people) in per_person.iter().filter(|_| cohorts_wanted) {
        let mut ranked: Vec<Submitter> = people.iter().collect();
        // Busiest first, and by name within a tie so the bands are stable
        // between runs over the same data.
        ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
        let total: usize = ranked.iter().map(|(_, w)| w.len()).sum();
        let mut bands: Vec<(&str, &[Submitter])> = Vec::new();
        let mut lo = 0usize;
        for (hi, name) in BANDS {
            let (a, b) = (ranked.len().min(lo), ranked.len().min(*hi));
            bands.push((name, &ranked[a..b]));
            lo = *hi;
        }
        bands.push(("rest", &ranked[ranked.len().min(lo)..]));
        for (name, band) in bands {
            if band.is_empty() {
                continue;
            }
            let mut waits: Vec<f64> = band.iter().flat_map(|(_, w)| w.iter().copied()).collect();
            cohorts.push(CohortStats {
                arch: (*arch).to_string(),
                cohort: name.to_string(),
                people: band.len(),
                over_hour_pct: over_hour(&waits),
                share_of_tasks: if total > 0 {
                    100.0 * waits.len() as f64 / total as f64
                } else {
                    0.0
                },
                queue_wait: summarize(&mut waits),
            });
        }
    }

    let streams: Vec<StreamShare> = per_stream
        .iter()
        .map(|((arch, stream), (tasks, secs))| {
            let total: f64 = per_stream
                .iter()
                .filter(|((a, _), _)| a == arch)
                .map(|(_, (_, s))| s)
                .sum();
            StreamShare {
                arch: (*arch).to_string(),
                stream: (*stream).to_string(),
                tasks: *tasks,
                builder_hours: secs / 3600.0,
                pct: if total > 0.0 {
                    100.0 * secs / total
                } else {
                    0.0
                },
            }
        })
        .collect();

    let warnings = warn(&arches, &cohorts, &stragglers);
    Health {
        arches,
        stragglers,
        streams,
        classes,
        cohorts,
        warnings,
    }
}

/// Thresholds, and why each is where it is.
///
/// Every number here comes from a measured contrast rather than from taste,
/// and the doc comment is the argument — a threshold nobody can justify gets
/// tuned away the first time it fires.
fn warn(arches: &[ArchHealth], cohorts: &[CohortStats], stragglers: &[Straggler]) -> Vec<Warning> {
    let mut out = Vec::new();
    // Said once, about the architecture that sets the pace, rather than
    // once per architecture: the interesting fact is that one of them is
    // deciding when everybody's builds finish.
    for a in arches {
        if a.submitters_slow > 0 && a.submitters >= MIN_FOR_WARNING {
            out.push(Warning {
                metric: "slow-submitters".to_string(),
                subject: a.arch.clone(),
                text: format!(
                    "{}: {} of {} regular submitters (at least {} builds \
                     each) have their own p90 queue wait over an hour, and \
                     they account for {:.0}% of this architecture's human \
                     builds. Quote the share rather than the count, which \
                     moves with that floor. Volume rank does not identify \
                     them: on s390x the busiest and the twelfth-busiest \
                     submitters were both affected and the second and \
                     eleventh were not",
                    a.arch,
                    a.submitters_slow,
                    a.submitters,
                    MIN_FOR_WARNING,
                    a.submitters_slow_task_pct
                ),
            });
        }
    }
    if let Some(s) = stragglers.first()
        && s.pct >= STRAGGLER_WARN_PCT
        && s.builds >= MIN_FOR_WARNING
    {
        out.push(Warning {
            metric: "straggler".to_string(),
            subject: s.arch.clone(),
            text: format!(
                "finished last for {:.1}% of {} multi-arch builds, which \
                 spent a median {:.1}h and a p90 {:.1}h with every other \
                 architecture already finished -- none of those builds is \
                 output until {} lands, however idle the rest of the fleet \
                 looks",
                s.pct,
                s.builds,
                s.spread.as_ref().map_or(0.0, |d| d.median) / 3600.0,
                s.spread.as_ref().map_or(0.0, |d| d.p90) / 3600.0,
                s.arch,
            ),
        });
    }
    for a in arches {
        if a.tasks < MIN_FOR_WARNING {
            continue;
        }
        // Below about 0.6 the observed waits are minutes and above about
        // 0.7 they are hours — measured across four rebuilds rather than
        // taken from a formula, which only half-validates against them.
        // This is the one signal that leads rather than follows, so it
        // fires at the lower line.
        if let Some(u) = a.utilisation
            && u > 0.6
        {
            let (line, what) = if u > 0.7 {
                ("0.7", "act: queueing is nonlinear from here")
            } else {
                ("0.6", "watch: the next increment costs more than this one")
            };
            out.push(Warning {
                metric: "utilisation".into(),
                subject: a.arch.clone(),
                text: format!(
                    "{}: utilisation {:.2} ({:.0} of {:.0} weight), above the {line} line — {what}",
                    a.arch,
                    u,
                    a.offered_weight.unwrap_or(0.0),
                    a.capacity.unwrap_or(0.0)
                ),
            });
        }
        // Wasted share ran 6.8% at F42 and 12.5% by F45. Ten percent sits
        // between the two, and this is capacity spent producing nothing —
        // the cheapest kind to recover.
        if a.wasted_pct > 10.0 {
            out.push(Warning {
                metric: "wasted-share".into(),
                subject: a.arch.clone(),
                text: format!(
                    "{}: {:.1}% of builder time went to failed or cancelled tasks \
                     ({:.0} of {:.0} hours), above the 10% line",
                    a.arch,
                    a.wasted_pct,
                    a.builder_hours * a.wasted_pct / 100.0,
                    a.builder_hours
                ),
            });
        }
        // Tail share went 5.8% to 23.8% over the same span; fifteen percent
        // catches it while the tail is still a handful of tasks.
        if a.tail_pct > 15.0 {
            out.push(Warning {
                metric: "tail-share".into(),
                subject: a.arch.clone(),
                text: format!(
                    "{}: {} task(s) over 6h took {:.1}% of builder time, above the \
                     15% line — check for hangs before buying capacity",
                    a.arch, a.tail_tasks, a.tail_pct
                ),
            });
        }
    }
    let (lead, second) = (BANDS[0].1, BANDS[1].1);
    for c in cohorts.iter().filter(|c| c.cohort == lead) {
        let Some(top) = &c.queue_wait else { continue };
        if top.count < MIN_FOR_WARNING {
            continue;
        }
        // Twenty minutes: the heaviest cohort sat under two minutes in
        // January 2025 and at 1.83h by July 2026, so this fires well inside
        // that drift rather than after it.
        if top.p90 > 20.0 * 60.0 {
            out.push(Warning {
                metric: "cohort-p90".into(),
                subject: format!("{} top-10", c.arch),
                text: format!(
                    "{}: {lead} submitters have a {:.0}m p90 and carry {:.0}% \
                     of human builds — a population median will not show this",
                    c.arch,
                    top.p90 / 60.0,
                    c.share_of_tasks
                ),
            });
        }
        // Five times the next band means the cost has stopped being shared,
        // which is the shape that stayed invisible for eighteen months.
        if let Some(next) = cohorts
            .iter()
            .find(|o| o.arch == c.arch && o.cohort == second)
            .and_then(|o| o.queue_wait.as_ref())
            && next.p90 > 0.0
            && top.p90 > 5.0 * next.p90
        {
            out.push(Warning {
                metric: "cohort-divergence".into(),
                subject: format!("{} top-10", c.arch),
                text: format!(
                    "{}: {lead} wait {:.0}x {second} at p90 ({:.0}m against \
                     {:.0}m) — the delay is landing on a few people",
                    c.arch,
                    top.p90 / next.p90,
                    top.p90 / 60.0,
                    next.p90 / 60.0
                ),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::BUILD_ARCH;

    fn build(id: i64, owner: &str, scratch: bool) -> BuildRecord {
        BuildRecord {
            instance: "fedora".into(),
            task_id: id,
            package: Some("pkg".into()),
            nvr: None,
            target: Some("rawhide".into()),
            owner: Some(owner.into()),
            scratch,
            state: 2,
            create_ts: 0.0,
            start_ts: Some(0.0),
            completion_ts: Some(100.0),
            priority: None,
            host_id: None,
        }
    }

    fn task(id: i64, parent: i64, arch: &str, wait: f64, dur: f64, state: i64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(parent),
            method: BUILD_ARCH.into(),
            arch: arch.into(),
            package: None,
            state,
            create_ts: 0.0,
            start_ts: Some(wait),
            completion_ts: Some(wait + dur),
            host_id: None,
            channel_id: None,
            weight: None,
        }
    }

    /// One build per submitter, `n` tasks each, all waiting `wait`.
    fn population(specs: &[(&str, usize, f64)]) -> Dataset {
        let mut ds = Dataset::new();
        let mut id = 1;
        for (owner, n, wait) in specs {
            for _ in 0..*n {
                let b = build(id, owner, false);
                ds.builds.insert(b.key(), b);
                let t = task(id + 100_000, id, "s390x", *wait, 60.0, 2);
                ds.tasks.insert(t.key(), t);
                id += 1;
            }
        }
        ds
    }

    /// A build whose architectures finish at different times, so the
    /// straggler is unambiguous.
    fn multiarch(id: i64, done: &[(&str, f64)]) -> (BuildRecord, Vec<TaskRecord>) {
        let build = BuildRecord {
            instance: "fedora".to_string(),
            task_id: id,
            package: Some("gzip".to_string()),
            nvr: None,
            target: Some("f45-build".to_string()),
            owner: Some("someone".to_string()),
            scratch: false,
            state: 2,
            create_ts: 0.0,
            start_ts: Some(0.0),
            completion_ts: Some(done.iter().map(|(_, d)| *d).fold(0.0, f64::max)),
            priority: Some(20),
            host_id: None,
        };
        let tasks = done
            .iter()
            .enumerate()
            .map(|(i, (arch, d))| TaskRecord {
                instance: "fedora".to_string(),
                task_id: id * 1000 + i as i64,
                parent: Some(id),
                arch: (*arch).to_string(),
                method: "buildArch".to_string(),
                package: Some("gzip".to_string()),
                state: 2,
                create_ts: 0.0,
                start_ts: Some(0.0),
                completion_ts: Some(*d),
                host_id: None,
                channel_id: None,
                weight: Some(1.5),
            })
            .collect();
        (build, tasks)
    }

    #[test]
    fn cohort_bands_are_not_computed_for_a_report_narrowed_to_accounts() {
        // Narrowed to one account, the bands would state that the ten
        // busiest submitters carry 100% of human builds -- the filter
        // restated as a finding about the fleet.
        let build_by = |id: i64, owner: &str| BuildRecord {
            instance: "fedora".to_string(),
            task_id: id,
            package: Some("gzip".to_string()),
            nvr: None,
            target: Some("f45-build".to_string()),
            owner: Some(owner.to_string()),
            scratch: false,
            state: 2,
            create_ts: 0.0,
            start_ts: Some(0.0),
            completion_ts: Some(100.0),
            priority: Some(20),
            host_id: None,
        };
        let mut with = Dataset::new();
        let mut without = Dataset::new();
        for id in 1..=60i64 {
            // `with`: sixty distinct people. `without`: all one person.
            with.builds
                .insert(format!("fedora:{id}"), build_by(id, &format!("p{id}")));
            without
                .builds
                .insert(format!("fedora:{id}"), build_by(id, "solo"));
            for ds in [&mut with, &mut without] {
                let t = TaskRecord {
                    instance: "fedora".to_string(),
                    task_id: 1000 + id,
                    parent: Some(id),
                    arch: "s390x".to_string(),
                    method: "buildArch".to_string(),
                    package: Some("gzip".to_string()),
                    state: 2,
                    create_ts: 0.0,
                    start_ts: Some(30.0),
                    completion_ts: Some(100.0),
                    host_id: None,
                    channel_id: None,
                    weight: Some(1.5),
                };
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        // Sixty people, asked for: three bands.
        assert_eq!(assess_all(&with).cohorts.len(), 5);

        // The same sixty, narrowed: no bands, and no warning derived from
        // them. Not a judgement about the population -- the population is
        // identical -- but about the question having been narrowed away.
        let selected: Vec<&TaskRecord> = with.tasks.values().collect();
        let narrowed = assess(&with, &selected, None, false);
        assert!(narrowed.cohorts.is_empty(), "{:?}", narrowed.cohorts);
        assert!(
            !narrowed
                .warnings
                .iter()
                .any(|w| w.metric.starts_with("cohort")),
            "{:?}",
            narrowed.warnings
        );
        // The rest of the assessment is unaffected.
        assert_eq!(narrowed.arches.len(), assess_all(&with).arches.len());
    }

    #[test]
    fn the_last_architecture_to_finish_is_the_straggler_and_the_others_wait() {
        let mut ds = Dataset::new();
        // Thirty builds, s390x last in every one by an hour.
        for id in 1..=30 {
            let (b, tasks) = multiarch(
                id,
                &[("x86_64", 600.0), ("aarch64", 900.0), ("s390x", 4200.0)],
            );
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        let health = assess_all(&ds);
        assert_eq!(health.stragglers.len(), 1, "{:?}", health.stragglers);
        let s = &health.stragglers[0];
        assert_eq!((&*s.arch, s.builds, s.pct), ("s390x", 30, 100.0));
        // 4200 - 600: what x86_64 and aarch64 spent already finished.
        let d = s.spread.as_ref().expect("spread");
        assert_eq!(
            (d.count, d.median, d.p90, d.max),
            (30, 3600.0, 3600.0, 3600.0)
        );
        assert!(
            health
                .warnings
                .iter()
                .any(|w| w.metric == "straggler" && w.subject == "s390x"),
            "{:?}",
            health.warnings
        );
    }

    #[test]
    fn a_two_architecture_build_is_not_the_fleet_waiting_on_one() {
        let mut ds = Dataset::new();
        for id in 1..=30 {
            let (b, tasks) = multiarch(id, &[("x86_64", 600.0), ("s390x", 4200.0)]);
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        assert!(assess_all(&ds).stragglers.is_empty());
    }

    #[test]
    fn builder_time_is_attributed_to_the_distribution_it_was_for() {
        let mut ds = Dataset::new();
        // Three ELN builds at an hour, one Fedora build at an hour: ELN
        // holds three quarters of the hours, whatever the task counts say.
        for (id, target) in [(1, "eln"), (2, "eln-extras"), (3, "eln"), (4, "f45-build")] {
            let (mut b, tasks) = multiarch(id, &[("s390x", 3600.0), ("x86_64", 3600.0)]);
            b.target = Some(target.to_string());
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        // And one EPEL build, to show the third bucket is not a catch-all.
        let (mut b, tasks) = multiarch(5, &[("s390x", 3600.0), ("x86_64", 3600.0)]);
        b.target = Some("epel10.3".to_string());
        ds.builds.insert("fedora:5".into(), b);
        for t in tasks {
            ds.tasks.insert(format!("fedora:{}", t.task_id), t);
        }

        let h = assess_all(&ds);
        let get = |stream: &str| {
            h.streams
                .iter()
                .find(|s| s.arch == "s390x" && s.stream == stream)
                .unwrap_or_else(|| panic!("{stream}"))
        };
        assert_eq!(get("eln").tasks, 3);
        assert_eq!(get("eln").builder_hours, 3.0);
        assert_eq!(get("eln").pct, 60.0);
        assert_eq!(get("epel").pct, 20.0);
        assert_eq!(get("fedora").pct, 20.0);
        // A target nobody set is Fedora's, not a fourth bucket.
        assert_eq!(Class::stream_of(None), "fedora");
        assert_eq!(Class::stream_of(Some("f45-rebuild")), "fedora");
        assert_eq!(Class::stream_of(Some("eln")), "eln");
        assert_eq!(Class::stream_of(Some("epel9-next")), "epel");
    }

    #[test]
    fn srpm_work_counts_as_load_but_not_as_an_architectures_compile_cost() {
        let mut ds = Dataset::new();
        for id in 1..=30i64 {
            // One compile at ten minutes per architecture, plus an SRPM
            // checkout at ten seconds landing on an s390x host.
            let (b, mut tasks) = multiarch(id, &[("s390x", 600.0), ("x86_64", 600.0)]);
            let mut srpm = tasks[0].clone();
            srpm.task_id = id * 1000 + 500;
            srpm.method = "buildSRPMFromSCM".to_string();
            srpm.completion_ts = Some(10.0);
            tasks.push(srpm);
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        let h = assess_all(&ds);
        let a = h.arches.iter().find(|a| a.arch == "s390x").expect("s390x");
        // The compile cost is the compiles: thirty of them, ten minutes
        // each. Averaging in a ten-second checkout would report 5m.
        let other = a.service.iter().find(|p| p.name == "other").expect("other");
        assert_eq!((other.tasks, other.p50), (30, Some(600.0)));
        // The checkout still occupied a builder, so it is still load, and
        // it still waited in a queue, so it is still in the class figures.
        assert_eq!(a.tasks, 60);
        assert_eq!(a.builder_hours, 30.0 * 610.0 / 3600.0);
        assert!(
            h.classes.iter().any(|c| c.arch == "s390x"),
            "SRPM work must still reach the per-class wait"
        );
    }

    #[test]
    fn build_time_splits_by_toolchain_family() {
        let mut ds = Dataset::new();
        for id in 1..=30 {
            let control = id % 2 == 0;
            let secs = if control { 60.0 } else { 600.0 };
            // Two architectures, so these are not taken for noarch builds.
            let (mut b, mut tasks) = multiarch(id, &[("s390x", secs), ("x86_64", secs)]);
            let pkg = if control {
                "rust-serde".to_string()
            } else {
                "gzip".to_string()
            };
            b.package = Some(pkg.clone());
            for t in &mut tasks {
                t.package = Some(pkg.clone());
            }
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        let health = assess_all(&ds);
        let s390x = &health
            .arches
            .iter()
            .find(|a| a.arch == "s390x")
            .expect("s390x")
            .service;
        let get = |name: &str| s390x.iter().find(|p| p.name == name).expect(name);
        assert_eq!((get("rust").tasks, get("rust").p50), (15, Some(60.0)));
        assert_eq!((get("other").tasks, get("other").p50), (15, Some(600.0)));
        // Named by toolchain, so a Go library is not filed under C.
        assert_eq!(family_of("golang-github-spf13-cobra"), "golang");
        assert_eq!(family_of("python-requests"), "python");
        assert_eq!(family_of("cosmic-term"), "rust");
        assert_eq!(family_of("gzip"), "other");
    }

    fn assess_all(ds: &Dataset) -> Health {
        let selected: Vec<&TaskRecord> = ds.tasks.values().collect();
        // No window: the shares still compute, utilisation does not.
        assess(ds, &selected, None, true)
    }

    /// The same, over a one-day window with a stated capacity — what a real
    /// report has.
    fn assess_day(ds: &Dataset, arch: &str, capacity: f64) -> Health {
        let mut ds = ds.clone();
        ds.capacity.insert(arch.to_string(), capacity);
        let selected: Vec<&TaskRecord> = ds.tasks.values().collect();
        assess(&ds, &selected, Some((0.0, 86_400.0)), true)
    }

    #[test]
    fn wasted_builder_time_is_a_share_of_hours_not_of_tasks() {
        // One cancelled task that ran for ten hours next to twenty-nine that
        // took a minute: 1 task in 30, but 95% of the capacity.
        let mut ds = Dataset::new();
        for i in 1..=29 {
            let b = build(i, "alice", false);
            ds.builds.insert(b.key(), b);
            let t = task(i + 100, i, "s390x", 1.0, 60.0, 2);
            ds.tasks.insert(t.key(), t);
        }
        // Id 30, not 10: the loop above already claimed 1..=29, and reusing
        // one silently replaced a task instead of adding one.
        let b = build(30, "alice", false);
        ds.builds.insert(b.key(), b);
        let t = task(130, 30, "s390x", 1.0, 36_000.0, TASK_CANCELED);
        ds.tasks.insert(t.key(), t);

        let h = assess_all(&ds);
        let arch = &h.arches[0];
        assert_eq!(arch.tasks, 30);
        assert!(arch.wasted_pct > 90.0, "{:?}", arch);
        assert_eq!(arch.tail_tasks, 1);
        assert!(arch.tail_pct > 90.0);
        let keys: Vec<&str> = h.warnings.iter().map(|w| w.metric.as_str()).collect();
        assert!(keys.contains(&"wasted-share"), "{keys:?}");
        assert!(keys.contains(&"tail-share"), "{keys:?}");
    }

    #[test]
    fn shared_builders_are_one_denominator_and_not_two() {
        // Two architectures on the same 100 units of hardware, each
        // offering 30. Per architecture that reads 0.30 twice and suggests
        // 140 units of headroom; the machines are at 0.60.
        let mut ds = Dataset::new();
        for (id, arch) in (1..=60i64).map(|i| (i, if i % 2 == 0 { "i386" } else { "x86_64" })) {
            let (b, tasks) = multiarch(id, &[(arch, 1800.0), ("s390x", 1800.0)]);
            ds.builds.insert(format!("fedora:{id}"), b);
            for t in tasks {
                ds.tasks.insert(format!("fedora:{}", t.task_id), t);
            }
        }
        ds.capacity.insert("i386".into(), 100.0);
        ds.capacity.insert("x86_64".into(), 100.0);
        ds.pools.push(crate::dataset::Pool {
            arches: vec!["i386".into(), "x86_64".into()],
            capacity: 100.0,
        });
        let selected: Vec<&TaskRecord> = ds.tasks.values().collect();
        let h = assess(&ds, &selected, Some((0.0, 86_400.0)), true);
        let get = |arch: &str| h.arches.iter().find(|a| a.arch == arch).expect(arch);

        // Both architectures report the pool's utilisation, and it is the
        // sum of what they offer over what they share.
        let (a, b) = (get("i386"), get("x86_64"));
        assert_eq!(a.pool.as_deref(), Some("i386 x86_64"));
        assert_eq!(a.utilisation, b.utilisation);
        assert_eq!(a.pool_capacity, Some(100.0));
        let pooled = a.pool_offered.expect("pool offered");
        assert!(
            (pooled - (a.offered_weight.unwrap() + b.offered_weight.unwrap())).abs() < 1e-9,
            "{pooled} != {:?} + {:?}",
            a.offered_weight,
            b.offered_weight
        );
        // Each architecture's own capacity is still reported, and still
        // says what can serve it.
        assert_eq!(a.capacity, Some(100.0));

        // An architecture with no pool falls back to its own figures
        // rather than reporting nothing.
        assert!(get("s390x").pool.is_none());
        assert!(get("s390x").utilisation.is_none()); // no capacity for it
    }

    #[test]
    fn utilisation_is_weight_in_use_over_capacity() {
        // Twenty tasks, each weighing 6 and running six hours inside a
        // one-day window: 20 * 6 * 6h / 24h = 30 weight in use. Against a
        // capacity of 50 that is 0.60 — just at the line, so no warning.
        let mut ds = Dataset::new();
        for i in 1..=20 {
            let b = build(i, "alice", false);
            ds.builds.insert(b.key(), b);
            let mut t = task(i + 100, i, "s390x", 1.0, 6.0 * 3600.0, 2);
            t.weight = Some(6.0);
            ds.tasks.insert(t.key(), t);
        }
        let h = assess_day(&ds, "s390x", 50.0);
        let a = &h.arches[0];
        assert_eq!(a.capacity, Some(50.0));
        assert!((a.offered_weight.unwrap() - 30.0).abs() < 0.1, "{a:?}");
        assert!((a.utilisation.unwrap() - 0.60).abs() < 0.01, "{a:?}");

        // Halve the capacity and the same work is over the acting line.
        let h = assess_day(&ds, "s390x", 25.0);
        assert!((h.arches[0].utilisation.unwrap() - 1.20).abs() < 0.01);
        let w = h
            .warnings
            .iter()
            .find(|w| w.metric == "utilisation")
            .expect("a utilisation warning");
        assert!(w.text.contains("0.7 line"), "{}", w.text);
        assert!(w.text.contains("act"), "{}", w.text);
    }

    #[test]
    fn filler_work_does_not_count_as_pressure() {
        // The March 2026 ppc64le shape: a fleet full of koschei canaries,
        // which run at priority 50 and give way to everything. Counting
        // them put naive utilisation at 1.39 while nobody waited at all.
        let mut ds = Dataset::new();
        for i in 1..=40 {
            let mut b = build(i, "koschei/koschei-backend01.rdu3.example.org", true);
            b.target = Some("rawhide".into());
            ds.builds.insert(b.key(), b);
            let mut t = task(i + 100, i, "ppc64le", 1.0, 6.0 * 3600.0, 2);
            t.weight = Some(6.0);
            ds.tasks.insert(t.key(), t);
        }
        let h = assess_day(&ds, "ppc64le", 20.0);
        let a = &h.arches[0];
        // The hours are still reported — the work did happen.
        assert!(a.builder_hours > 200.0, "{a:?}");
        // But none of it is offered load, so there is nothing to warn about.
        assert_eq!(a.offered_weight, Some(0.0));
        assert_eq!(a.utilisation, Some(0.0));
        assert!(
            h.warnings.iter().all(|w| w.metric != "utilisation"),
            "{:?}",
            h.warnings
        );

        // The same volume from maintainers is pressure, and does warn.
        let mut ds = Dataset::new();
        for i in 1..=40 {
            let b = build(i, "alice", false);
            ds.builds.insert(b.key(), b);
            let mut t = task(i + 100, i, "ppc64le", 1.0, 6.0 * 3600.0, 2);
            t.weight = Some(6.0);
            ds.tasks.insert(t.key(), t);
        }
        let h = assess_day(&ds, "ppc64le", 20.0);
        assert!(h.arches[0].utilisation.unwrap() > 1.0);
        assert!(h.warnings.iter().any(|w| w.metric == "utilisation"));
    }

    #[test]
    fn a_task_weight_of_none_counts_as_one_and_a_missing_capacity_as_unknown() {
        // Weight is absent on older rows; treating it as zero would report
        // an idle fleet, and treating the arch as absent would hide it.
        let mut ds = Dataset::new();
        for i in 1..=24 {
            let b = build(i, "alice", false);
            ds.builds.insert(b.key(), b);
            let t = task(i + 100, i, "s390x", 1.0, 3600.0, 2);
            ds.tasks.insert(t.key(), t);
        }
        let selected: Vec<&TaskRecord> = ds.tasks.values().collect();
        let h = assess(&ds, &selected, Some((0.0, 86_400.0)), true);
        let a = &h.arches[0];
        // 24 tasks x 1 weight x 1h over a 24h window = 1.0 in use.
        assert!((a.offered_weight.unwrap() - 1.0).abs() < 0.01, "{a:?}");
        // No capacity known for this arch, so no ratio is invented.
        assert_eq!(a.capacity, None);
        assert_eq!(a.utilisation, None);
        assert!(!h.warnings.iter().any(|w| w.metric == "utilisation"));
    }

    #[test]
    fn a_population_too_thin_to_judge_produces_no_warning() {
        // The same 10-hour cancelled build, but on a day with three tasks in
        // total: 99% wasted and 99% tail, and neither is evidence of
        // anything. A share over three tasks is arithmetic, and one bad
        // build on a quiet day must not send somebody hunting.
        let mut ds = Dataset::new();
        for i in 1..=2 {
            let b = build(i, "alice", false);
            ds.builds.insert(b.key(), b);
            let t = task(i + 100, i, "s390x", 1.0, 60.0, 2);
            ds.tasks.insert(t.key(), t);
        }
        let b = build(3, "alice", false);
        ds.builds.insert(b.key(), b);
        let t = task(103, 3, "s390x", 1.0, 36_000.0, TASK_CANCELED);
        ds.tasks.insert(t.key(), t);

        let h = assess_all(&ds);
        // The numbers are still reported — they are just not shouted about.
        assert!(h.arches[0].wasted_pct > 90.0);
        assert!(h.warnings.is_empty(), "{:?}", h.warnings);
    }

    #[test]
    fn a_healthy_architecture_warns_about_nothing() {
        let ds = population(&[("alice", 30, 30.0), ("bob", 30, 30.0)]);
        let h = assess_all(&ds);
        assert!(h.warnings.is_empty(), "{:?}", h.warnings);
        assert_eq!(h.arches[0].tail_tasks, 0);
    }

    #[test]
    fn the_cohort_split_finds_what_a_median_hides() {
        // The July 2026 shape: a few heavy submitters waiting hours, a long
        // tail of occasional ones waiting seconds. The population median is
        // fine; the top cohort is not.
        let mut specs: Vec<(&str, usize, f64)> =
            vec![("heavy-a", 60, 7200.0), ("heavy-b", 60, 7200.0)];
        for name in ["c1", "c2", "c3", "c4", "c5", "c6", "c7", "c8"] {
            specs.push((name, 5, 30.0));
        }
        let ds = population(&specs);
        let h = assess_all(&ds);

        // Ten submitters, so the first two bands hold five each and the
        // later ones are empty; the p90 threshold still fires on its own.
        let top = h.cohorts.iter().find(|c| c.cohort == "top-5").unwrap();
        assert_eq!(top.people, 5);
        assert!(top.queue_wait.as_ref().unwrap().p90 > 20.0 * 60.0);
        let keys: Vec<&str> = h.warnings.iter().map(|w| w.metric.as_str()).collect();
        assert!(keys.contains(&"cohort-p90"), "{keys:?}");
    }

    #[test]
    fn divergence_is_measured_against_the_next_band() {
        // Sixty submitters, every band populated. The busiest five wait two
        // hours and everybody else a minute -- the real shape, where the
        // cliff falls inside what used to be one ten-person band.
        let mut specs: Vec<(&str, usize, f64)> = Vec::new();
        let heavy: Vec<String> = (0..5).map(|i| format!("heavy{i}")).collect();
        let light: Vec<String> = (0..55).map(|i| format!("light{i}")).collect();
        for n in &heavy {
            specs.push((n.as_str(), 40, 7200.0));
        }
        for n in &light {
            specs.push((n.as_str(), 3, 60.0));
        }
        let ds = population(&specs);
        let h = assess_all(&ds);

        let by = |c: &str| {
            h.cohorts
                .iter()
                .find(|x| x.cohort == c)
                .unwrap_or_else(|| panic!("no {c}"))
        };
        assert_eq!(by("top-5").people, 5);
        assert_eq!(by("6-10").people, 5);
        assert_eq!(by("11-20").people, 10);
        assert_eq!(by("21-50").people, 30);
        assert_eq!(by("rest").people, 10);
        // Ten people carry 400 of 550 tasks.
        assert!(by("top-5").share_of_tasks > 30.0, "{:?}", by("top-5"));
        assert_eq!(by("top-5").over_hour_pct, 100.0);
        assert_eq!(by("21-50").over_hour_pct, 0.0);
        let keys: Vec<&str> = h.warnings.iter().map(|w| w.metric.as_str()).collect();
        assert!(keys.contains(&"cohort-divergence"), "{keys:?}");
    }

    #[test]
    fn classes_are_reported_apart_and_never_summed() {
        // The mistake this prevents: a rebuild's four-hour wait averaged
        // with a maintainer's one minute.
        let mut ds = Dataset::new();
        let mut rebuild = build(1, "releng", false);
        rebuild.target = Some("f45-rebuild".into());
        ds.builds.insert(rebuild.key(), rebuild);
        ds.tasks.insert(
            "fedora:101".into(),
            task(101, 1, "s390x", 14_400.0, 60.0, 2),
        );
        let mine = build(2, "alice", false);
        ds.builds.insert(mine.key(), mine);
        ds.tasks
            .insert("fedora:102".into(), task(102, 2, "s390x", 60.0, 60.0, 2));

        let h = assess_all(&ds);
        let get = |slug: &str| {
            h.classes
                .iter()
                .find(|c| c.class == slug)
                .and_then(|c| c.queue_wait.as_ref())
                .map(|d| d.median)
        };
        assert_eq!(get("mass-rebuild"), Some(14_400.0));
        assert_eq!(get("official"), Some(60.0));
        // And only the human build is a candidate for a cohort.
        assert_eq!(h.cohorts.iter().map(|c| c.people).sum::<usize>(), 1);
    }
}
