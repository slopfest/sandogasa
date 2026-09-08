// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The health of what a package depends on.
//!
//! A package can be in good shape itself while a library three edges
//! down is orphaned with a year-old CVE bug, and that is the package's
//! problem too. This check reads the dependency graph a `poi-tracker
//! deps --graph` walk saved (`--graph`, or the graphs a workspace file
//! names with `-w`), gathers the other checks' facts about each
//! dependency, and reports them for the package — separately from the
//! package's own results, never blended into one number, so the report
//! can say "the package is fine, its dependencies are not".
//!
//! The aggregation is by worst offender with attribution, not by
//! average: one orphaned dependency among fifty healthy ones is the
//! finding, and a mean hides it. So a package's dependency reading is
//! the worst state among its dependencies with the package that has it
//! named, counts above a threshold, and the age of security bugs
//! pooled over the bugs themselves across the set — median and p90,
//! with the n. Direct dependencies are read in full; the transitive
//! closure gets one line naming what needs attention, since a
//! dependency's own dependency problems belong in its row and repeating
//! them up every path would count them twice.
//!
//! It is not a check that runs per package on its own: `run` computes
//! it after the others, from their stored results.

use std::collections::BTreeMap;

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;
use crate::report::HealthReport;

/// What the other checks know about one dependency.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DepFacts {
    pub orphaned: bool,
    /// Effective maintainer count, when `maintainer_count` has run.
    pub maintainers: Option<u64>,
    /// Open security bugs on rawhide, when `bug_count` has run.
    pub security_bugs: u64,
    /// Ages in days of those bugs.
    pub security_age_days: Vec<u64>,
    /// The branch no longer has a package of this name: the graph is
    /// older than the retirement, or of another branch. Reported, not
    /// counted.
    pub gone: bool,
}

impl DepFacts {
    /// The stored `maintainer_count` and `bug_count:rawhide` results
    /// for `pkg`, or `None` when neither has run.
    pub fn from_report(report: &HealthReport, pkg: &str) -> Option<DepFacts> {
        let entry = report.package.get(pkg)?;
        let maint = entry.checks.get("maintainer_count").map(|e| &e.data);
        let bugs = entry.checks.get("bug_count:rawhide").map(|e| &e.data);
        if maint.is_none() && bugs.is_none() {
            return None;
        }
        Some(DepFacts {
            orphaned: maint.is_some_and(|d| d["orphaned"].as_bool().unwrap_or(false)),
            maintainers: maint.and_then(|d| d["effective_count"].as_u64()),
            security_bugs: bugs
                .and_then(|d| d["by_kind"]["security"].as_u64())
                .unwrap_or(0),
            security_age_days: bugs
                .and_then(|d| d["security_age_days"].as_array())
                .map(|a| a.iter().filter_map(|v| v.as_u64()).collect())
                .unwrap_or_default(),
            gone: false,
        })
    }

    /// Why this dependency needs attention, worst first; empty when it
    /// does not.
    pub fn reasons(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.orphaned {
            out.push("orphaned".to_string());
        }
        if self.security_bugs > 0 {
            let oldest = self.security_age_days.iter().max().copied().unwrap_or(0);
            out.push(format!(
                "{} open security bug{}, oldest {oldest} days",
                self.security_bugs,
                if self.security_bugs == 1 { "" } else { "s" }
            ));
        }
        if self.maintainers == Some(1) {
            out.push("a single maintainer".to_string());
        }
        out
    }

    /// How badly this dependency needs attention: orphaned outranks
    /// open security bugs, which outrank a single maintainer; among
    /// equals, the oldest security bug decides.
    fn severity(&self) -> (u8, u64) {
        let rank = if self.orphaned {
            3
        } else if self.security_bugs > 0 {
            2
        } else if self.maintainers == Some(1) {
            1
        } else {
            0
        };
        (
            rank,
            self.security_age_days.iter().max().copied().unwrap_or(0),
        )
    }
}

/// The p-th percentile of `sorted` (ascending), nearest-rank.
fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    Some(sorted[rank.clamp(1, sorted.len()) - 1])
}

/// One direct dependency: its source name and whether a binary of the
/// package requires it at run time (`false`: needed only to build).
pub type Direct = (String, bool);

/// The dependency reading for one package: `direct` are its direct
/// dependencies with their kind, `transitive` the deeper ones that were
/// walked, `facts` what is known about each (a dependency absent from
/// `facts` is counted as not yet checked).
///
/// The worst offender is chosen by severity, then by kind — a run-time
/// dependency ships with the package, a build-only one does not — then
/// by the age of its oldest security bug.
pub fn aggregate(
    direct: &[Direct],
    transitive: &[String],
    facts: &BTreeMap<String, DepFacts>,
) -> serde_json::Value {
    // (name, runtime?, direct?) over the whole set.
    let all = direct
        .iter()
        .map(|(n, rt)| (n, *rt, true))
        .chain(transitive.iter().map(|n| (n, true, false)));
    let mut worst: Option<(&String, &DepFacts, bool, bool)> = None;
    let (mut orphaned, mut with_security, mut single, mut unknown) = (0u64, 0u64, 0u64, 0u64);
    let mut ages: Vec<u64> = Vec::new();
    let mut transitive_attention: Vec<String> = Vec::new();
    let mut gone: Vec<String> = Vec::new();
    for (name, runtime, is_direct) in all {
        let Some(f) = facts.get(name) else {
            unknown += 1;
            continue;
        };
        if f.gone {
            gone.push(name.clone());
            continue;
        }
        orphaned += u64::from(f.orphaned);
        with_security += u64::from(f.security_bugs > 0);
        single += u64::from(f.maintainers == Some(1));
        ages.extend(&f.security_age_days);
        if f.severity().0 > 0 {
            if !is_direct {
                transitive_attention.push(name.clone());
            }
            let key = |f: &DepFacts, rt: bool| (f.severity().0, rt, f.severity().1);
            if worst.is_none_or(|(_, w, w_rt, _)| key(f, runtime) > key(w, w_rt)) {
                worst = Some((name, f, runtime, is_direct));
            }
        }
    }
    ages.sort_unstable();
    // TOML has no null, so what is absent is left out rather than
    // written as one: no worst offender, no security bugs to date.
    let mut data = serde_json::json!({
        "total": direct.len() + transitive.len(),
        "direct": direct.len(),
        "transitive": transitive.len(),
        "orphaned": orphaned,
        "with_security_bugs": with_security,
        "single_maintainer": single,
        "unknown": unknown,
        "security_bugs": ages.len(),
        "transitive_attention": transitive_attention,
        "gone": gone,
    });
    if let Some((name, f, runtime, is_direct)) = worst {
        data["worst"] = serde_json::json!({
            "name": name,
            "reasons": f.reasons(),
            "direct": is_direct,
            "runtime": runtime,
        });
    }
    if let (Some(p50), Some(p90)) = (percentile(&ages, 50.0), percentile(&ages, 90.0)) {
        data["security_age_p50_days"] = p50.into();
        data["security_age_p90_days"] = p90.into();
    }
    data
}

/// Render the reading, one line per fact, continuation lines indented
/// so they sit under the check's line in the summary.
pub fn format(data: &serde_json::Value) -> String {
    let n = |k: &str| data[k].as_u64().unwrap_or(0);
    let mut lines = vec![format!(
        "{} ({} direct, {} transitive)",
        n("total"),
        n("direct"),
        n("transitive")
    )];
    if let Some(w) = data["worst"].as_object() {
        let reasons: Vec<&str> = w["reasons"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let place = match (
            w["direct"].as_bool().unwrap_or(false),
            w["runtime"].as_bool().unwrap_or(true),
        ) {
            (true, true) => "direct, runtime",
            (true, false) => "direct, build-only",
            (false, _) => "transitive",
        };
        lines.push(format!(
            "worst: {}  {}  [{place}]",
            w["name"].as_str().unwrap_or("?"),
            reasons.join(", ")
        ));
    } else if n("unknown") < n("total") {
        lines.push("none needs attention".to_string());
    }
    let mut counts = Vec::new();
    if n("with_security_bugs") > 0 {
        counts.push(format!(
            "{} with open security bugs",
            n("with_security_bugs")
        ));
    }
    if n("orphaned") > 0 {
        counts.push(format!("{} orphaned", n("orphaned")));
    }
    if n("single_maintainer") > 0 {
        counts.push(format!(
            "{} with a single maintainer",
            n("single_maintainer")
        ));
    }
    if n("unknown") > 0 {
        counts.push(format!("{} not yet checked", n("unknown")));
    }
    if !counts.is_empty() {
        lines.push(counts.join(", "));
    }
    if let (Some(p50), Some(p90)) = (
        data["security_age_p50_days"].as_u64(),
        data["security_age_p90_days"].as_u64(),
    ) {
        lines.push(format!(
            "age of open security bugs across deps: p50 {p50} d, p90 {p90} d (n={})",
            n("security_bugs")
        ));
    }
    if let Some(g) = data["gone"].as_array()
        && !g.is_empty()
    {
        let names: Vec<&str> = g.iter().filter_map(|v| v.as_str()).collect();
        lines.push(format!(
            "no longer on the branch, not counted (graph stale?): {}",
            names.join(", ")
        ));
    }
    if let Some(t) = data["transitive_attention"].as_array()
        && !t.is_empty()
    {
        let names: Vec<&str> = t.iter().filter_map(|v| v.as_str()).collect();
        lines.push(format!(
            "transitive: {} need{} attention ({})",
            names.len(),
            if names.len() == 1 { "s" } else { "" },
            names.join(", ")
        ));
    }
    lines.join("\n    ")
}

pub struct DependencyHealth;

impl HealthCheck for DependencyHealth {
    fn id(&self) -> &'static str {
        "dependency_health"
    }

    fn description(&self) -> &'static str {
        "Health of a package's dependencies, from the graph (run with --graph or -w)"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Cheap
    }

    fn run(
        &self,
        _package: &str,
        _variant: Option<&str>,
        _ctx: &Context,
    ) -> Result<CheckResult, String> {
        Err(
            "derived from the dependency graph after the other checks; give run --graph or -w"
                .into(),
        )
    }

    fn format_result(&self, data: &serde_json::Value) -> String {
        format(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn percentile_is_nearest_rank() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[7], 90.0), Some(7));
        assert_eq!(percentile(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 50.0), Some(5));
        assert_eq!(percentile(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10], 90.0), Some(9));
    }

    #[test]
    fn worst_offender_wins_over_the_average() {
        let mut facts = BTreeMap::new();
        for i in 0..49 {
            facts.insert(
                format!("rust-fine{i}"),
                DepFacts {
                    maintainers: Some(3),
                    ..Default::default()
                },
            );
        }
        facts.insert(
            "rust-bar".to_string(),
            DepFacts {
                orphaned: true,
                maintainers: Some(0),
                security_bugs: 3,
                security_age_days: vec![412, 96, 30],
                gone: false,
            },
        );
        facts.insert(
            "rust-baz".to_string(),
            DepFacts {
                maintainers: Some(1),
                security_bugs: 1,
                security_age_days: vec![200],
                ..Default::default()
            },
        );
        let direct: Vec<Direct> = (0..12)
            .map(|i| (format!("rust-fine{i}"), true))
            .chain([("rust-bar".to_string(), true)])
            .collect();
        let transitive: Vec<String> = (12..49)
            .map(|i| format!("rust-fine{i}"))
            .chain(v(&["rust-baz", "rust-unseen"]))
            .collect();
        let data = aggregate(&direct, &transitive, &facts);
        assert_eq!(data["total"], 52);
        assert_eq!(data["worst"]["name"], "rust-bar");
        assert_eq!(data["worst"]["direct"], true);
        assert_eq!(data["orphaned"], 1);
        assert_eq!(data["with_security_bugs"], 2);
        assert_eq!(data["single_maintainer"], 1);
        assert_eq!(data["unknown"], 1);
        assert_eq!(data["security_bugs"], 4);
        // Pooled over the bugs: 30, 96, 200, 412.
        assert_eq!(data["security_age_p50_days"], 96);
        assert_eq!(data["security_age_p90_days"], 412);
        assert_eq!(
            data["transitive_attention"],
            serde_json::json!(["rust-baz"])
        );
        let text = format(&data);
        assert!(
            text.contains("worst: rust-bar  orphaned, 3 open security bugs, oldest 412 days  [direct, runtime]"),
            "{text}"
        );
        assert!(text.contains("p50 96 d, p90 412 d (n=4)"), "{text}");
        assert!(
            text.contains("transitive: 1 needs attention (rust-baz)"),
            "{text}"
        );
        assert!(text.contains("1 not yet checked"), "{text}");
    }

    #[test]
    fn a_dependency_gone_from_the_branch_is_reported_not_counted() {
        let mut facts = BTreeMap::new();
        facts.insert(
            "rust-srpm-macros".to_string(),
            DepFacts {
                orphaned: true,
                gone: true,
                ..Default::default()
            },
        );
        facts.insert(
            "fine".to_string(),
            DepFacts {
                maintainers: Some(2),
                ..Default::default()
            },
        );
        let direct = vec![
            ("rust-srpm-macros".to_string(), true),
            ("fine".to_string(), true),
        ];
        let data = aggregate(&direct, &[], &facts);
        assert!(data["worst"].is_null());
        assert_eq!(data["orphaned"], 0);
        assert_eq!(data["gone"], serde_json::json!(["rust-srpm-macros"]));
        let text = format(&data);
        assert!(
            text.contains("no longer on the branch, not counted (graph stale?): rust-srpm-macros"),
            "{text}"
        );
    }

    #[test]
    fn a_clean_set_says_so() {
        let mut facts = BTreeMap::new();
        facts.insert(
            "a".to_string(),
            DepFacts {
                maintainers: Some(2),
                ..Default::default()
            },
        );
        let data = aggregate(&[("a".to_string(), true)], &[], &facts);
        assert!(data["worst"].is_null());
        assert!(format(&data).contains("none needs attention"));
    }

    #[test]
    fn a_runtime_dependency_outranks_a_build_only_one_of_equal_severity() {
        let mut facts = BTreeMap::new();
        for n in ["build-tool", "runtime-lib"] {
            facts.insert(
                n.to_string(),
                DepFacts {
                    orphaned: true,
                    ..Default::default()
                },
            );
        }
        let direct = vec![
            ("build-tool".to_string(), false),
            ("runtime-lib".to_string(), true),
        ];
        let data = aggregate(&direct, &[], &facts);
        assert_eq!(data["worst"]["name"], "runtime-lib");
        assert!(format(&data).contains("[direct, runtime]"));
        let only_build = vec![("build-tool".to_string(), false)];
        assert!(format(&aggregate(&only_build, &[], &facts)).contains("[direct, build-only]"));
    }

    #[test]
    fn facts_are_read_from_the_stored_results() {
        let mut report = HealthReport::new("inv");
        report.update(
            "rust-bar",
            "maintainer_count",
            serde_json::json!({"effective_count": 0, "orphaned": true}),
        );
        report.update(
            "rust-bar",
            "bug_count:rawhide",
            serde_json::json!({"open": 4, "by_kind": {"security": 2, "other": 2}, "security_age_days": [10, 400]}),
        );
        let f = DepFacts::from_report(&report, "rust-bar").unwrap();
        assert!(f.orphaned);
        assert_eq!(f.security_bugs, 2);
        assert_eq!(f.security_age_days, vec![10, 400]);
        assert!(DepFacts::from_report(&report, "rust-unseen").is_none());
    }
}
