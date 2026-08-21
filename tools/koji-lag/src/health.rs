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

/// Cohort boundaries, by rank when submitters are ordered by volume.
const TOP: usize = 10;
const NEXT: usize = 50;

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

/// Package-name prefix that picks out the control population.
///
/// Rust packages are built by a toolchain whose cost is dominated by
/// rustc rather than by the C and C++ compilers, and the workspace has
/// thousands of them on every architecture — enough to be a population
/// rather than a sample, and uniform enough that a change in what they
/// cost is a change in the *platform* rather than in a compiler flag.
///
/// A name prefix rather than a `BuildRequires` scan on purpose: it keeps
/// this self-contained in the store, and a control population does not
/// need to be exact to be a control.
pub const CONTROL_PREFIX: &str = "rust-";

/// How long one population of packages took to build, within one window.
///
/// Deliberately two plain numbers and **never a ratio between the two
/// populations**, because within a single window that ratio is package mix
/// and not cost. On s390x in July 2026 the control population averaged
/// 3.8 minutes against 8.9 for everything else — a 2.3x gap that says only
/// that Rust crates are small, since the two *medians* were within seconds
/// of each other.
///
/// The comparison this exists to feed is across windows, where each
/// population is compared with its own earlier self and the mix cancels.
/// See [`crate::trend`].
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Population {
    /// `control` for [`CONTROL_PREFIX`] packages, `rest` for the others.
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
    pub capacity: Option<f64>,
    /// Mean weight in use: task weight integrated over the window and
    /// divided by its length, so it compares with `capacity` directly.
    pub offered_weight: Option<f64>,
    /// `offered_weight / capacity`, counting only work that competes.
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
    /// Build time split by [`CONTROL_PREFIX`]. An ingredient for
    /// [`crate::trend`]; not interpretable within one window on its own.
    pub service: Vec<Population>,
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
    // (arch, is-control) -> build times, for the cross-window comparison.
    let mut per_pop: BTreeMap<(&str, bool), Vec<f64>> = BTreeMap::new();
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
            if task.method != BUILD_ARCH {
                continue;
            }
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
            {
                per_pop
                    .entry((&task.arch, pkg.starts_with(CONTROL_PREFIX)))
                    .or_default()
                    .push(secs);
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

    let over_hour = |xs: &[f64]| match xs.len() {
        0 => 0.0,
        n => 100.0 * xs.iter().filter(|w| **w > 3600.0).count() as f64 / n as f64,
    };

    let arches: Vec<ArchHealth> = per_arch
        .iter()
        .map(
            |(arch, (total, wasted, tail, tail_n, tasks, weight_secs))| {
                let capacity = dataset.capacity.get(*arch).copied();
                let offered = window.and_then(|(from, to)| {
                    let span = to - from;
                    (span > 0.0).then_some(weight_secs / span)
                });
                ArchHealth {
                    arch: (*arch).to_string(),
                    capacity,
                    offered_weight: offered,
                    utilisation: match (offered, capacity) {
                        (Some(o), Some(c)) if c > 0.0 => Some(o / c),
                        _ => None,
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
                    service: [(false, "rest"), (true, "control")]
                        .into_iter()
                        .filter_map(|(is_control, name)| {
                            let mut xs = per_pop.get(&(*arch, is_control))?.clone();
                            xs.sort_by(f64::total_cmp);
                            Some(Population {
                                name: name.to_string(),
                                tasks: xs.len(),
                                p50: median(&xs),
                                p90: percentile(&xs, 0.9),
                            })
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
        let mut ranked: Vec<(&&str, &Vec<f64>)> = people.iter().collect();
        // Busiest first, and by name within a tie so the bands are stable
        // between runs over the same data.
        ranked.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(b.0)));
        let total: usize = ranked.iter().map(|(_, w)| w.len()).sum();
        for (name, band) in [
            ("top-10", &ranked[..ranked.len().min(TOP)]),
            (
                "next-40",
                &ranked[ranked.len().min(TOP)..ranked.len().min(NEXT)],
            ),
            ("rest", &ranked[ranked.len().min(NEXT)..]),
        ] {
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

    let warnings = warn(&arches, &cohorts, &stragglers);
    Health {
        arches,
        stragglers,
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
    for c in cohorts.iter().filter(|c| c.cohort == "top-10") {
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
                    "{}: the ten busiest submitters have a {:.0}m p90 and carry {:.0}% \
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
            .find(|o| o.arch == c.arch && o.cohort == "next-40")
            .and_then(|o| o.queue_wait.as_ref())
            && next.p90 > 0.0
            && top.p90 > 5.0 * next.p90
        {
            out.push(Warning {
                metric: "cohort-divergence".into(),
                subject: format!("{} top-10", c.arch),
                text: format!(
                    "{}: the busiest ten wait {:.0}x the next forty at p90 ({:.0}m against \
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
        assert_eq!(assess_all(&with).cohorts.len(), 3);

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
    fn srpm_work_counts_as_load_but_not_as_an_architectures_compile_cost() {
        let mut ds = Dataset::new();
        for id in 1..=30i64 {
            // One compile at ten minutes, one SRPM checkout at ten seconds,
            // both landing on s390x hosts.
            let (b, mut tasks) = multiarch(id, &[("s390x", 600.0)]);
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
        let a = &assess_all(&ds).arches[0];
        // The compile cost is the compiles: thirty of them, ten minutes
        // each. Averaging in a ten-second checkout would report 5m.
        let rest = a.service.iter().find(|p| p.name == "rest").expect("rest");
        assert_eq!((rest.tasks, rest.p50), (30, Some(600.0)));
        // The checkout still occupied a builder, so it is still load.
        assert_eq!(a.tasks, 60);
        assert_eq!(a.builder_hours, 30.0 * 610.0 / 3600.0);
    }

    #[test]
    fn build_time_splits_on_the_control_prefix() {
        let mut ds = Dataset::new();
        for id in 1..=30 {
            let control = id % 2 == 0;
            let (mut b, mut tasks) =
                multiarch(id, &[("s390x", if control { 60.0 } else { 600.0 })]);
            let pkg = if control {
                format!("{CONTROL_PREFIX}serde")
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
        let s390x = &health.arches[0].service;
        let get = |name: &str| s390x.iter().find(|p| p.name == name).expect(name);
        assert_eq!((get("control").tasks, get("control").p50), (15, Some(60.0)));
        assert_eq!((get("rest").tasks, get("rest").p50), (15, Some(600.0)));
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

        let top = h.cohorts.iter().find(|c| c.cohort == "top-10").unwrap();
        // Only ten submitters exist, so all of them land in the top band and
        // there is no next-40 to diverge from — the median still hides it,
        // which is why the p90 threshold exists on its own.
        assert_eq!(top.people, 10);
        assert!(top.queue_wait.as_ref().unwrap().p90 > 20.0 * 60.0);
        let keys: Vec<&str> = h.warnings.iter().map(|w| w.metric.as_str()).collect();
        assert!(keys.contains(&"cohort-p90"), "{keys:?}");
    }

    #[test]
    fn divergence_is_measured_against_the_next_band() {
        // Sixty submitters, so all three bands exist. The busiest ten wait
        // two hours and everybody else a minute.
        let mut specs: Vec<(&str, usize, f64)> = Vec::new();
        let heavy: Vec<String> = (0..10).map(|i| format!("heavy{i}")).collect();
        let light: Vec<String> = (0..50).map(|i| format!("light{i}")).collect();
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
        assert_eq!(by("top-10").people, 10);
        assert_eq!(by("next-40").people, 40);
        assert_eq!(by("rest").people, 10);
        // Ten people carry 400 of 550 tasks.
        assert!(by("top-10").share_of_tasks > 70.0, "{:?}", by("top-10"));
        assert_eq!(by("top-10").over_hour_pct, 100.0);
        assert_eq!(by("next-40").over_hour_pct, 0.0);
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
