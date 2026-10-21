// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Compare architectures on the *same* package, split by build length.
//!
//! An aggregate median per architecture compares package mixes, not
//! speed. Fedora's architectures do not build the same set — s390x
//! built 2,527 packages in 2026-09 where x86_64 built 3,427 — so a
//! row saying s390x's median is 3.2m against x86_64's 1.5m says as
//! much about what each was asked to build as about how fast it did
//! it.
//!
//! Pairing on the package removes that. Splitting the pairs by how
//! long the build takes removes the second trap: a single ratio
//! averages opposite effects into "about parity". Measured on
//! 2026-09, s390x was *faster* than ppc64le on builds under five
//! minutes (0.83) and twice as slow on builds of thirty to sixty
//! (1.94), and the mean across all packages was 1.06 — a number true
//! of no build at all.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use crate::dataset::TaskRecord;
use crate::stats::{in_build_time_population, median};

/// Left out of the comparison entirely.
///
/// A noarch package is built once and shipped everywhere, so the
/// noarch task and the reference's task for the same package are not
/// the same work. Pairing them produced ratios of 0.01x, which reads
/// as an architecture a hundred times faster than x86_64 rather than
/// as the category error it is.
const NOT_AN_ARCH: &str = "noarch";

/// How long the reference architecture took, as a band.
///
/// Banded on the *reference*, not on each architecture in turn, so
/// every row of a band describes the same packages.
const BANDS: &[(&str, f64, f64)] = &[
    ("<5m", 0.0, 300.0),
    ("5-30m", 300.0, 1800.0),
    ("30-60m", 1800.0, 3600.0),
    (">1h", 3600.0, f64::INFINITY),
];

/// One architecture against the reference, band by band.
#[derive(Debug, PartialEq, Serialize, JsonSchema)]
pub struct ArchComparison {
    pub arch: String,
    pub bands: Vec<BandRatio>,
}

/// The ratio in one band: how long this architecture took for each
/// package, over what the reference took, as a median across the
/// packages paired in that band.
#[derive(Debug, PartialEq, Serialize, JsonSchema)]
pub struct BandRatio {
    pub band: &'static str,
    /// Packages built by both architectures and landing in this band.
    pub packages: usize,
    /// Median of the per-package ratios; `None` only when nothing
    /// paired in this band. A band too thin to quote is withheld by
    /// the renderer, under the same `--min-samples` as every other
    /// table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ratio: Option<f64>,
}

/// Every pairing a window holds: for each `(package, arch)`, one
/// `(ratio, reference seconds)` per build that produced both.
///
/// The **build** is the pairing unit, not the package name. A
/// build's per-arch children are the same source at the same
/// version, submitted together and compiled from the same SRPM --
/// the same NEVR differing only in architecture, which is the only
/// comparison that isolates the architecture. Pairing on the name
/// across a window does not give that: a package built twice in a
/// month can pair one architecture's build of one version against
/// another's build of the next, and a figure that moves with what
/// landed between them is not a property of either machine.
fn paired(
    tasks: &[&TaskRecord],
    reference: &str,
    include_failed: bool,
) -> BTreeMap<(String, String), Vec<(f64, f64)>> {
    let mut builds: BTreeMap<String, Vec<&TaskRecord>> = BTreeMap::new();
    for task in tasks {
        if !in_build_time_population(task, include_failed) {
            continue;
        }
        if let Some(parent) = task.parent {
            builds
                .entry(format!("{}:{parent}", task.instance))
                .or_default()
                .push(task);
        }
    }
    let mut out: BTreeMap<(String, String), Vec<(f64, f64)>> = BTreeMap::new();
    for children in builds.values() {
        // A median over the arch's tasks in this one build, which is
        // normally a single task; more than one means a retry.
        let mut by_arch: BTreeMap<&str, Vec<f64>> = BTreeMap::new();
        let mut package = None;
        for task in children {
            if let Some(secs) = task.build_time() {
                by_arch.entry(task.arch.as_str()).or_default().push(secs);
                package = package.or(task.package.as_deref());
            }
        }
        let Some(package) = package else { continue };
        let per_arch: BTreeMap<&str, f64> = by_arch
            .into_iter()
            .filter_map(|(a, mut v)| {
                v.sort_by(f64::total_cmp);
                median(&v).map(|m| (a, m))
            })
            .collect();
        let Some(&base) = per_arch.get(reference) else {
            continue;
        };
        if base <= 0.0 {
            continue;
        }
        for (arch, secs) in &per_arch {
            if *arch == reference || *arch == NOT_AN_ARCH {
                continue;
            }
            out.entry((package.to_string(), (*arch).to_string()))
                .or_default()
                .push((secs / base, base));
        }
    }
    out
}

/// Compare every architecture against `reference`, band by band.
///
/// Every pairing found is reported, with its count; withholding a
/// thin band is the renderer's job, as it is for the per-architecture
/// tables, so one `--min-samples` governs both.
pub fn against(
    tasks: &[&TaskRecord],
    reference: &str,
    include_failed: bool,
) -> Vec<ArchComparison> {
    let per = paired(tasks, reference, include_failed);
    let arches: std::collections::BTreeSet<&str> = per.keys().map(|(_, a)| a.as_str()).collect();

    let mut out = Vec::new();
    for arch in arches {
        // Ratios collected per band, keyed by the band's index so the
        // output order is the band order rather than alphabetical.
        let mut by_band: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
        for ((_, a), pairs) in &per {
            if a != arch {
                continue;
            }
            // One vote per package, however often it was built: the
            // pairing is per build so the comparison stays
            // like-for-like, but a package rebuilt nightly must not
            // outweigh one built once.
            let mut ratios: Vec<f64> = pairs.iter().map(|(r, _)| *r).collect();
            let mut bases: Vec<f64> = pairs.iter().map(|(_, b)| *b).collect();
            ratios.sort_by(f64::total_cmp);
            bases.sort_by(f64::total_cmp);
            let (Some(ratio), Some(base)) = (median(&ratios), median(&bases)) else {
                continue;
            };
            if let Some(at) = BANDS
                .iter()
                .position(|(_, lo, hi)| base >= *lo && base < *hi)
            {
                by_band.entry(at).or_default().push(ratio);
            }
        }
        let bands = BANDS
            .iter()
            .enumerate()
            .map(|(at, (label, _, _))| {
                let mut ratios = by_band.remove(&at).unwrap_or_default();
                ratios.sort_by(f64::total_cmp);
                BandRatio {
                    band: label,
                    packages: ratios.len(),
                    // The median of the ratios, not the ratio of the
                    // medians: one slow outlier should move the
                    // figure by one package's worth, not by its size.
                    ratio: median(&ratios),
                }
            })
            .collect();
        // An architecture that paired with nothing has no row at
        // all, as distinct from one whose rows the renderer then
        // withholds as thin.
        let bands: Vec<BandRatio> = bands;
        if bands.iter().any(|b| b.packages > 0) {
            out.push(ArchComparison {
                arch: arch.to_string(),
                bands,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::BUILD_ARCH;

    /// One `buildArch` child of a build, taking `secs` on `arch`.
    ///
    /// `build` is the parent task, which is what the comparison
    /// pairs on -- so two packages must never share one, and two
    /// builds of the same package must not share one either. Koji
    /// gives each build its own task; a fixture that did otherwise
    /// would be testing something that cannot happen.
    fn task(id: i64, build: i64, pkg: &str, arch: &str, secs: f64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(build),
            method: BUILD_ARCH.into(),
            arch: arch.into(),
            package: Some(pkg.into()),
            state: 2,
            create_ts: 0.0,
            start_ts: Some(0.0),
            completion_ts: Some(secs),
            host_id: None,
            channel_id: None,
            weight: None,
        }
    }

    fn compare(rows: &[TaskRecord]) -> Vec<ArchComparison> {
        let refs: Vec<&TaskRecord> = rows.iter().collect();
        against(&refs, "x86_64", false)
    }

    #[test]
    fn a_band_is_decided_by_the_reference_not_by_each_architecture() {
        // `slow` takes 40m on x86_64 and 80m on s390x. It belongs in
        // the 30-60m band because that is where *x86_64* put it; if
        // each architecture banded itself the row would compare
        // different packages in each cell.
        let rows = vec![
            task(1, 10, "slow", "x86_64", 2400.0),
            task(2, 10, "slow", "s390x", 4800.0),
        ];
        let out = compare(&rows);
        assert_eq!(out.len(), 1);
        let bands = &out[0].bands;
        assert_eq!(bands[2].band, "30-60m");
        assert_eq!(bands[2].packages, 1);
        assert_eq!(bands[2].ratio, Some(2.0));
        // And nowhere else.
        assert_eq!(bands[3].packages, 0);
    }

    #[test]
    fn a_package_the_reference_did_not_build_is_not_compared() {
        // Nothing to divide by, so it is dropped rather than
        // contributing a one-sided figure.
        let rows = vec![task(1, 10, "only-s390x", "s390x", 60.0)];
        // Nothing pairs, so nothing is reported at all.
        assert!(compare(&rows).is_empty());
    }

    #[test]
    fn noarch_is_not_an_architecture_to_compare() {
        // A noarch package builds once; pairing it with the
        // reference's build of the same package compares different
        // work and produced ratios near 0.01x on real data.
        let rows = vec![
            task(1, 10, "doc", "x86_64", 600.0),
            task(2, 10, "doc", "noarch", 6.0),
            task(3, 10, "doc", "s390x", 900.0),
        ];
        let out = compare(&rows);
        let arches: Vec<&str> = out.iter().map(|a| a.arch.as_str()).collect();
        assert_eq!(arches, ["s390x"]);
    }

    #[test]
    fn every_pairing_is_reported_with_its_count() {
        // Two packages is not an answer; printing one invites it
        // being quoted.
        let rows = vec![
            task(1, 10, "a", "x86_64", 60.0),
            task(2, 10, "a", "s390x", 120.0),
            task(3, 11, "b", "x86_64", 60.0),
            task(4, 11, "b", "s390x", 120.0),
        ];
        // Both pairings are reported with their count; it is the
        // renderer that decides two is too few to show.
        let out = compare(&rows);
        assert_eq!(out[0].bands[0].packages, 2);
        assert_eq!(out[0].bands[0].ratio, Some(2.0));
    }

    #[test]
    fn a_package_built_twice_pairs_inside_each_build() {
        // The same package, two builds, with the architectures
        // moving apart between them. Pairing on the name across the
        // window mixes one build's x86_64 with the other's s390x and
        // reports a ratio no build ran; pairing inside each build
        // gives 2.0 and 1.0, and the package still votes once.
        // Three builds, so the answer does not turn on how an
        // even-length median is defined. Ratios 2.0, 1.0, 3.0.
        let rows = vec![
            task(1, 10, "p", "x86_64", 60.0),
            task(2, 10, "p", "s390x", 120.0),
            task(3, 11, "p", "x86_64", 600.0),
            task(4, 11, "p", "s390x", 600.0),
            task(5, 12, "p", "x86_64", 100.0),
            task(6, 12, "p", "s390x", 300.0),
        ];
        let out = compare(&rows);
        let counted: usize = out[0].bands.iter().map(|b| b.packages).sum();
        assert_eq!(counted, 1, "one package is one vote, however often built");
        let ratio = out[0].bands.iter().find_map(|b| b.ratio);
        // Pairing on the name would take the median x86_64 (100s)
        // against the median s390x (300s) and report 3.0.
        assert_eq!(ratio, Some(2.0), "the median of 2.0, 1.0 and 3.0");
    }

    #[test]
    fn the_median_ratio_is_taken_not_the_ratio_of_medians() {
        // One package ten times slower should move the figure by one
        // package's worth, not by its size. Ratios are 2, 2, 10;
        // median 2. The ratio of summed times would be 14/3 = 4.67.
        let mut rows = Vec::new();
        for (i, (pkg, s390x)) in [("a", 120.0), ("b", 120.0), ("c", 600.0)]
            .iter()
            .enumerate()
        {
            let id = i as i64 * 2;
            let build = 100 + i as i64;
            rows.push(task(id + 1, build, pkg, "x86_64", 60.0));
            rows.push(task(id + 2, build, pkg, "s390x", *s390x));
        }
        let out = compare(&rows);
        assert_eq!(out[0].bands[0].ratio, Some(2.0));
    }
}
