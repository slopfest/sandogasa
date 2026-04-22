// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HealthCheck trait and result types.

use serde::{Deserialize, Serialize};

use crate::context::Context;

/// Cost tier classifies how expensive a check is to run. Users
/// pick a tier (or explicit check IDs) to control runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CostTier {
    /// Local data or one cheap query — run often (hourly/daily).
    Cheap,
    /// One API call per package — run regularly (daily/weekly).
    Medium,
    /// Transitive or multi-query — run rarely (weekly/monthly).
    Expensive,
}

/// The result of a single check for a single package.
///
/// The inner `data` is serialized as-is to the report TOML, so each
/// check owns its own result schema. Reports can be deserialized
/// and inspected without the crate knowing each check's schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
    /// Check-specific structured data.
    pub data: serde_json::Value,
}

/// A single health check that can be run against a package.
///
/// Checks may be *variant-aware* — producing separate results for
/// e.g. different Fedora releases. Each variant gets its own stored
/// entry with its own timestamp, so the staleness/selective-update
/// logic can treat `bug_count:f44` independently from `bug_count:f45`.
pub trait HealthCheck: Send + Sync {
    /// Stable identifier used in CLI flags and stored results.
    fn id(&self) -> &'static str;

    /// Human-readable description.
    fn description(&self) -> &'static str;

    /// Cost tier — controls whether --cheap/--medium/--expensive
    /// runs this check.
    fn cost_tier(&self) -> CostTier;

    /// Return the list of variants to run for this check given the
    /// current context. Default: a single `None` variant (check is
    /// not parametrized). Variant-aware checks return e.g.
    /// `vec![Some("rawhide".into()), Some("f44".into())]`.
    fn variants(&self, _ctx: &Context) -> Vec<Option<String>> {
        vec![None]
    }

    /// Run the check for a single package and variant. `variant` is
    /// one of the strings returned by `variants(ctx)` (or `None` if
    /// the check has no variants). Returning `Err` is a check failure
    /// (network error, parse error, etc.); returning `Ok` with an
    /// empty value is a valid "nothing to report" result.
    fn run(
        &self,
        package: &str,
        variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String>;
}

/// Build the storage key for a (check, variant) pair.
///
/// Used by the report and the main loop. `None` → just the check id;
/// `Some("f44")` → `check_id:f44`.
pub fn entry_key(check_id: &str, variant: Option<&str>) -> String {
    match variant {
        Some(v) => format!("{check_id}:{v}"),
        None => check_id.to_string(),
    }
}
