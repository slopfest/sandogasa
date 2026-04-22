// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Persistent health report data model.
//!
//! Reports are stored as TOML. Each package has a map of check
//! results keyed by check id, each with its own timestamp. This
//! enables selective re-running of individual checks.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Top-level health report for an inventory.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct HealthReport {
    /// Report-level metadata.
    pub report: ReportMeta,
    /// Per-package results. Key is source RPM name.
    #[serde(default)]
    pub package: BTreeMap<String, PackageReport>,
}

/// Report-level metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReportMeta {
    /// Inventory name this report covers.
    pub inventory: String,
    /// Timestamp of the most recent update to any check in this report.
    pub generated: DateTime<Utc>,
}

/// Health data for a single package.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct PackageReport {
    /// Per-check results keyed by check id.
    #[serde(flatten)]
    pub checks: BTreeMap<String, CheckEntry>,
}

/// A single check's result for a single package.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CheckEntry {
    /// When this check ran.
    pub timestamp: DateTime<Utc>,
    /// Check-specific structured data.
    pub data: serde_json::Value,
}

impl HealthReport {
    /// Create an empty report for the given inventory.
    pub fn new(inventory: &str) -> Self {
        Self {
            report: ReportMeta {
                inventory: inventory.to_string(),
                generated: Utc::now(),
            },
            package: BTreeMap::new(),
        }
    }

    /// Load a report from a TOML file.
    pub fn load(path: &str) -> Result<Self, String> {
        let content =
            std::fs::read_to_string(path).map_err(|e| format!("failed to read {path}: {e}"))?;
        toml::from_str(&content).map_err(|e| format!("failed to parse {path}: {e}"))
    }

    /// Save a report to a TOML file.
    pub fn save(&self, path: &str) -> Result<(), String> {
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("TOML serialization failed: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("failed to write {path}: {e}"))
    }

    /// Update a check result for a package. Existing results for
    /// other checks on this package are preserved.
    pub fn update(&mut self, package: &str, check_id: &str, data: serde_json::Value) {
        let pkg = self.package.entry(package.to_string()).or_default();
        pkg.checks.insert(
            check_id.to_string(),
            CheckEntry {
                timestamp: Utc::now(),
                data,
            },
        );
        self.report.generated = Utc::now();
    }

    /// Check if a result for (package, check_id) is stale relative to
    /// the given max age. Returns true if the check hasn't run or its
    /// timestamp is older than `now - max_age`.
    pub fn is_stale(&self, package: &str, check_id: &str, max_age: chrono::Duration) -> bool {
        let Some(pkg) = self.package.get(package) else {
            return true;
        };
        let Some(entry) = pkg.checks.get(check_id) else {
            return true;
        };
        Utc::now().signed_duration_since(entry.timestamp) > max_age
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_report_is_empty() {
        let report = HealthReport::new("test");
        assert_eq!(report.report.inventory, "test");
        assert!(report.package.is_empty());
    }

    #[test]
    fn update_adds_package_and_check() {
        let mut report = HealthReport::new("test");
        report.update("foo", "bug_count", serde_json::json!({"open": 5}));
        assert!(report.package.contains_key("foo"));
        assert!(report.package["foo"].checks.contains_key("bug_count"));
    }

    #[test]
    fn update_preserves_other_checks() {
        let mut report = HealthReport::new("test");
        report.update("foo", "bug_count", serde_json::json!({"open": 5}));
        report.update("foo", "maintainer_count", serde_json::json!({"count": 2}));
        assert_eq!(report.package["foo"].checks.len(), 2);
    }

    #[test]
    fn update_replaces_same_check() {
        let mut report = HealthReport::new("test");
        report.update("foo", "bug_count", serde_json::json!({"open": 5}));
        report.update("foo", "bug_count", serde_json::json!({"open": 7}));
        assert_eq!(report.package["foo"].checks.len(), 1);
        assert_eq!(
            report.package["foo"].checks["bug_count"].data,
            serde_json::json!({"open": 7})
        );
    }

    #[test]
    fn is_stale_for_missing_entry() {
        let report = HealthReport::new("test");
        assert!(report.is_stale("foo", "bug_count", chrono::Duration::hours(1)));
    }

    #[test]
    fn is_stale_fresh_result() {
        let mut report = HealthReport::new("test");
        report.update("foo", "bug_count", serde_json::json!({"open": 5}));
        assert!(!report.is_stale("foo", "bug_count", chrono::Duration::hours(1)));
    }

    #[test]
    fn round_trip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.toml");
        let path_str = path.to_str().unwrap();

        let mut report = HealthReport::new("hyperscale");
        report.update("rust-arrow", "bug_count", serde_json::json!({"open": 5}));
        report.update("rust-arrow", "maintainers", serde_json::json!({"count": 2}));
        report.save(path_str).unwrap();

        let loaded = HealthReport::load(path_str).unwrap();
        assert_eq!(loaded.report.inventory, "hyperscale");
        assert_eq!(loaded.package["rust-arrow"].checks.len(), 2);
    }
}
