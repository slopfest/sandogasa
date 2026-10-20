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

/// Median build time per `(package, arch)`, which is the unit a
/// comparison pairs on.
fn per_package(tasks: &[&TaskRecord], include_failed: bool) -> BTreeMap<(String, String), f64> {
    let mut gathered: BTreeMap<(String, String), Vec<f64>> = BTreeMap::new();
    for task in tasks {
        if !in_build_time_population(task, include_failed) {
            continue;
        }
        if let (Some(pkg), Some(secs)) = (task.package.as_deref(), task.build_time()) {
            gathered
                .entry((pkg.to_string(), task.arch.clone()))
                .or_default()
                .push(secs);
        }
    }
    gathered
        .into_iter()
        .filter_map(|(k, mut v)| {
            v.sort_by(f64::total_cmp);
            median(&v).map(|m| (k, m))
        })
        .collect()
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
    let per = per_package(tasks, include_failed);
    let arches: std::collections::BTreeSet<&str> = per
        .keys()
        .map(|(_, a)| a.as_str())
        .filter(|a| *a != reference && *a != NOT_AN_ARCH)
        .collect();

    let mut out = Vec::new();
    for arch in arches {
        // Ratios collected per band, keyed by the band's index so the
        // output order is the band order rather than alphabetical.
        let mut by_band: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
        for ((pkg, a), secs) in &per {
            if a != arch {
                continue;
            }
            let Some(base) = per.get(&(pkg.clone(), reference.to_string())) else {
                continue;
            };
            if *base <= 0.0 {
                continue;
            }
            if let Some(at) = BANDS
                .iter()
                .position(|(_, lo, hi)| *base >= *lo && *base < *hi)
            {
                by_band.entry(at).or_default().push(secs / base);
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

    /// A completed build of `pkg` on `arch` taking `secs`.
    fn task(id: i64, pkg: &str, arch: &str, secs: f64) -> TaskRecord {
        TaskRecord {
            instance: "fedora".into(),
            task_id: id,
            parent: Some(1),
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
            task(1, "slow", "x86_64", 2400.0),
            task(2, "slow", "s390x", 4800.0),
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
        let rows = vec![task(1, "only-s390x", "s390x", 60.0)];
        // Nothing pairs, so nothing is reported at all.
        assert!(compare(&rows).is_empty());
    }

    #[test]
    fn noarch_is_not_an_architecture_to_compare() {
        // A noarch package builds once; pairing it with the
        // reference's build of the same package compares different
        // work and produced ratios near 0.01x on real data.
        let rows = vec![
            task(1, "doc", "x86_64", 600.0),
            task(2, "doc", "noarch", 6.0),
            task(3, "doc", "s390x", 900.0),
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
            task(1, "a", "x86_64", 60.0),
            task(2, "a", "s390x", 120.0),
            task(3, "b", "x86_64", 60.0),
            task(4, "b", "s390x", 120.0),
        ];
        // Both pairings are reported with their count; it is the
        // renderer that decides two is too few to show.
        let out = compare(&rows);
        assert_eq!(out[0].bands[0].packages, 2);
        assert_eq!(out[0].bands[0].ratio, Some(2.0));
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
            rows.push(task(id + 1, pkg, "x86_64", 60.0));
            rows.push(task(id + 2, pkg, "s390x", *s390x));
        }
        let out = compare(&rows);
        assert_eq!(out[0].bands[0].ratio, Some(2.0));
    }
}
