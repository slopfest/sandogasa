// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Count open bugs by category for a package, per Fedora release.
//!
//! Queries Bugzilla once for all open bugs filed against the
//! package's component (resolution=---), then classifies each bug
//! against the FTBFS/FTI trackers for the specific release variant.
//! Returns one result per variant so each release can be aged /
//! re-run independently.

use std::collections::BTreeMap;

use sandogasa_bugclass::bugzilla::classify;

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

pub struct BugCount;

impl HealthCheck for BugCount {
    fn id(&self) -> &'static str {
        "bug_count"
    }

    fn description(&self) -> &'static str {
        "Count of open bugs by category (security, ftbfs, update request, etc.)"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Medium
    }

    fn variants(&self, ctx: &Context) -> Vec<Option<String>> {
        // Always include rawhide; add each explicitly requested Fedora
        // and EPEL version. Users update each independently via
        // --fedora-version / --epel-version + --max-age.
        let mut vs: Vec<Option<String>> = vec![Some("rawhide".to_string())];
        for ver in &ctx.fedora_versions {
            vs.push(Some(format!("f{ver}")));
        }
        for ver in &ctx.epel_versions {
            vs.push(Some(format!("epel{ver}")));
        }
        vs
    }

    fn run(
        &self,
        package: &str,
        variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String> {
        let variant = variant.ok_or("bug_count requires a variant")?;
        let trackers = ctx
            .trackers
            .get(variant)
            .ok_or_else(|| format!("no trackers loaded for variant '{variant}'"))?;

        let query =
            format!("product=Fedora&product=Fedora EPEL&component={package}&resolution=---");
        let bugs = ctx
            .block_on(ctx.bz.search(&query, 0))
            .map_err(|e| format!("Bugzilla search failed: {e}"))?;

        let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
        for bug in &bugs {
            let kind = classify(bug, trackers);
            *by_kind.entry(kind.as_str()).or_insert(0) += 1;
        }

        Ok(CheckResult {
            data: serde_json::json!({
                "open": bugs.len(),
                "by_kind": by_kind,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_bugclass::BugKind;

    #[test]
    fn id_and_tier() {
        let check = BugCount;
        assert_eq!(check.id(), "bug_count");
        assert_eq!(check.cost_tier(), CostTier::Medium);
    }

    #[test]
    fn kind_string_ids_match() {
        // Regression guard — the report uses these strings as keys.
        assert_eq!(BugKind::Security.as_str(), "security");
        assert_eq!(BugKind::Ftbfs.as_str(), "ftbfs");
        assert_eq!(BugKind::Fti.as_str(), "fti");
        assert_eq!(BugKind::Update.as_str(), "update");
        assert_eq!(BugKind::Branch.as_str(), "branch");
        assert_eq!(BugKind::Other.as_str(), "other");
        assert_eq!(BugKind::Review.as_str(), "review");
    }
}
