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
pub trait HealthCheck: Send + Sync {
    /// Stable identifier used in CLI flags and stored results.
    fn id(&self) -> &'static str;

    /// Human-readable description.
    fn description(&self) -> &'static str;

    /// Cost tier — controls whether --cheap/--medium/--expensive
    /// runs this check.
    fn cost_tier(&self) -> CostTier;

    /// Run the check for a single package. Returning `Err` is a
    /// check failure (network error, parse error, etc.); returning
    /// `Ok` with an empty value is a valid "nothing to report" result.
    fn run(&self, package: &str, ctx: &Context) -> Result<CheckResult, String>;
}
