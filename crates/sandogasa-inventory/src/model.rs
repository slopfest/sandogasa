// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Data model for package-of-interest inventories.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A Bugzilla priority level. Variants are ordered from
/// least- to most-important so a `max(...)` across several
/// candidates picks the highest priority.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    /// Bugzilla's `unspecified` — the default a release-monitoring
    /// bug arrives at. Treated as "don't manage" when resolving
    /// from inventory; valid as an explicit override that opts
    /// a package out of a workload-level default.
    Unspecified,
    Low,
    Medium,
    High,
    Urgent,
}

impl Priority {
    /// The matching Bugzilla API string (`"unspecified"`, `"low"`,
    /// …). Used when constructing PUT bodies and when comparing
    /// against the `priority` field of a fetched bug.
    pub fn as_bugzilla_str(&self) -> &'static str {
        match self {
            Priority::Unspecified => "unspecified",
            Priority::Low => "low",
            Priority::Medium => "medium",
            Priority::High => "high",
            Priority::Urgent => "urgent",
        }
    }
}

/// Top-level inventory document.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Inventory {
    /// Inventory metadata.
    pub inventory: InventoryMeta,
    /// Packages in the inventory.
    #[serde(default)]
    pub package: Vec<Package>,
}

/// Inventory metadata.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct InventoryMeta {
    /// Inventory name (e.g. "hyperscale-packages").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Maintainer (person or team).
    pub maintainer: String,
    /// Labels/tags for the inventory (e.g. "eln-extras").
    #[serde(default)]
    pub labels: Vec<String>,
    /// Workload definitions. Keys are workload identifiers; values
    /// carry per-workload metadata for content-resolver export.
    /// Packages without explicit workloads inherit all keys.
    #[serde(default)]
    pub workloads: BTreeMap<String, WorkloadMeta>,
    /// Field names to strip from all packages on export.
    #[serde(default)]
    pub private_fields: Vec<String>,
}

/// Per-workload metadata for content-resolver export.
///
/// All fields except `packages` are optional — omitted fields
/// fall back to the inventory-level values.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct WorkloadMeta {
    /// Workload name in content-resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Human-readable description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Maintainer override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintainer: Option<String>,
    /// Content-resolver labels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    /// Default Bugzilla priority for packages in this workload
    /// when they don't carry an explicit `priority`. The
    /// resolved priority is the max across all workloads listing
    /// the package, so a package in both a "best-effort" and a
    /// "security-sensitive" workload picks up the latter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_priority: Option<Priority>,
    /// Source RPM names belonging to this workload.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<String>,
}

/// A package of interest (source RPM).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Package {
    /// Source RPM name (required).
    pub name: String,

    // --- Metadata fields (may be private) ---
    /// Point of contact ("Name <email>").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poc: Option<String>,
    /// Reason for tracking this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Team responsible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    /// Internal task/ticket reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,

    // --- Content-resolver fields ---
    /// Binary RPM subpackages to track. If omitted, all are assumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpms: Option<Vec<String>>,
    /// Architecture-specific RPMs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch_rpms: Option<BTreeMap<String, Vec<String>>>,

    // --- hs-relmon fields ---
    /// Which branch/repository to track (upstream, fedora-rawhide, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub track: Option<String>,
    /// Name in Repology if different from RPM name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repology_name: Option<String>,
    /// Comma-separated distribution list for hs-relmon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distros: Option<String>,
    /// Whether to file GitLab issues for version updates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_issue: Option<bool>,

    // --- Bugzilla bug-priority management ---
    /// Bugzilla priority to apply to release-monitoring bugs for
    /// this package. Overrides any workload-level
    /// `default_priority`. Set to `unspecified` to explicitly
    /// opt out of a workload default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,

    // --- dist-git state ---
    /// Dist-git branches where this package is retired (a
    /// `dead.package` marker is present), as recorded by
    /// `poi-tracker triage-retired --mark`. Consumers skip checks
    /// that can't succeed for a retired branch (e.g. auditing
    /// rawhide update requests). Refreshed — in both directions —
    /// each time `triage-retired --mark` checks the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_on: Option<Vec<String>>,

    /// Set when the package is no longer shipped on any active
    /// branch — the dist-git project is gone, it has no branch on
    /// an active release, or it is retired everywhere — as
    /// recorded by `poi-tracker prune-retired`. The value records
    /// why. Most operations skip such packages; `triage-retired`
    /// still processes them so remaining bugs get closed, and the
    /// sync commands' `--prune` preserves them (a fresh sync
    /// would otherwise re-add retired packages, whose ACLs
    /// remain). Refreshed — in both directions — each time
    /// `prune-retired` checks the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unshipped: Option<String>,

    /// Set when the package's upstream repo is archived but it
    /// still has builds tagged into CBS release tags — recorded by
    /// `poi-tracker sync-gitlab --mark-unshipped`. The value records
    /// why. Unlike [`Self::unshipped`] the package still ships, so
    /// it is NOT skipped by triage/audit; instead it is a build
    /// cleanup candidate for `hs-relmon` to untag. Refreshed — in
    /// both directions — each `--mark-unshipped` run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_builds: Option<String>,
}

impl Package {
    /// Whether this package is recorded as retired on `branch`.
    pub fn is_retired_on(&self, branch: &str) -> bool {
        self.retired_on
            .as_ref()
            .is_some_and(|branches| branches.iter().any(|b| b == branch))
    }

    /// Whether this package is recorded as no longer shipped on
    /// any active branch.
    pub fn is_unshipped(&self) -> bool {
        self.unshipped.is_some()
    }

    /// Whether this package's upstream repo is archived while it
    /// still has CBS builds (a cleanup candidate for hs-relmon).
    pub fn has_archived_builds(&self) -> bool {
        self.archived_builds.is_some()
    }

    /// Merge another entry for the same package into this one,
    /// field by field: fields set in `other` win, fields unset in
    /// `other` keep this entry's values, and retirement knowledge
    /// is combined (`retired_on` is unioned; `unshipped` keeps
    /// whichever side knows). Returns a human-readable note for
    /// each field where both sides were set to different values
    /// (the `other` side won).
    pub fn merge_from(&mut self, other: &Package) -> Vec<String> {
        let mut conflicts = Vec::new();
        macro_rules! take {
            ($field:ident) => {
                if let Some(theirs) = &other.$field {
                    if let Some(ours) = &self.$field
                        && ours != theirs
                    {
                        conflicts.push(format!(
                            "{}: conflicting {} ({ours:?} vs {theirs:?}; later file wins)",
                            self.name,
                            stringify!($field),
                        ));
                    }
                    self.$field = Some(theirs.clone());
                }
            };
        }
        take!(poc);
        take!(reason);
        take!(team);
        take!(task);
        take!(rpms);
        take!(arch_rpms);
        take!(track);
        take!(repology_name);
        take!(distros);
        take!(file_issue);
        take!(priority);
        take!(unshipped);
        take!(archived_builds);
        // Retirement branches are facts about dist-git, not
        // per-inventory preferences: union them.
        if let Some(theirs) = &other.retired_on {
            let mut branches = self.retired_on.take().unwrap_or_default();
            branches.extend(theirs.iter().cloned());
            branches.sort();
            branches.dedup();
            self.retired_on = Some(branches);
        }
        conflicts
    }
}

impl Inventory {
    /// Check if a field name is private (should be stripped on export).
    pub fn is_private(&self, field: &str) -> bool {
        self.inventory.private_fields.iter().any(|f| f == field)
    }

    /// Get packages filtered by workload. Returns all if workload is None.
    ///
    /// When a workload key is given, returns only packages listed in
    /// that workload's `packages` field.
    pub fn packages_for_workload(&self, workload: Option<&str>) -> Vec<&Package> {
        match workload {
            None => self.package.iter().collect(),
            Some(w) => {
                let pkg_names: std::collections::HashSet<&str> = self
                    .inventory
                    .workloads
                    .get(w)
                    .map(|wl| wl.packages.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                self.package
                    .iter()
                    .filter(|p| pkg_names.contains(p.name.as_str()))
                    .collect()
            }
        }
    }

    /// Return the sorted list of workload identifiers.
    pub fn workload_names(&self) -> Vec<&str> {
        self.inventory
            .workloads
            .keys()
            .map(|k| k.as_str())
            .collect()
    }

    /// Return the workload keys that contain a given package.
    pub fn workloads_for_package(&self, name: &str) -> Vec<&str> {
        self.inventory
            .workloads
            .iter()
            .filter(|(_, meta)| meta.packages.iter().any(|p| p == name))
            .map(|(k, _)| k.as_str())
            .collect()
    }

    /// Add a package to a workload, creating the workload if needed.
    pub fn add_to_workload(&mut self, workload: &str, package: &str) {
        let meta = self
            .inventory
            .workloads
            .entry(workload.to_string())
            .or_default();
        if !meta.packages.iter().any(|p| p == package) {
            meta.packages.push(package.to_string());
            meta.packages.sort();
        }
    }

    /// Find a package by name.
    pub fn find_package(&self, name: &str) -> Option<&Package> {
        self.package.iter().find(|p| p.name == name)
    }

    /// Resolve the Bugzilla priority for a package. An explicit
    /// `priority` on the package wins outright (including
    /// `unspecified`, which acts as an opt-out from any
    /// workload-level default). Otherwise, walk every workload
    /// that lists this package and return the highest
    /// `default_priority` among them.
    pub fn priority_for(&self, name: &str) -> Option<Priority> {
        let pkg = self.find_package(name)?;
        if let Some(p) = pkg.priority {
            return Some(p);
        }
        let mut best: Option<Priority> = None;
        for meta in self.inventory.workloads.values() {
            if !meta.packages.iter().any(|p| p == name) {
                continue;
            }
            if let Some(p) = meta.default_priority {
                best = match best {
                    None => Some(p),
                    Some(b) => Some(b.max(p)),
                };
            }
        }
        best
    }

    /// Find a package by name (mutable).
    pub fn find_package_mut(&mut self, name: &str) -> Option<&mut Package> {
        self.package.iter_mut().find(|p| p.name == name)
    }

    /// Add a package, replacing if one with the same name exists.
    pub fn add_package(&mut self, pkg: Package) {
        if let Some(existing) = self.find_package_mut(&pkg.name) {
            *existing = pkg;
        } else {
            self.package.push(pkg);
            self.package.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }

    /// Remove a package by name. Returns true if found and removed.
    pub fn remove_package(&mut self, name: &str) -> bool {
        let len = self.package.len();
        self.package.retain(|p| p.name != name);
        self.package.len() < len
    }

    /// Merge another inventory into this one.
    ///
    /// New packages from `other` are added. A package present in
    /// both is merged field by field ([`Package::merge_from`]):
    /// `other`'s set fields win, its unset fields keep this
    /// inventory's values, and retirement knowledge is combined —
    /// so a marker recorded in one file survives the package also
    /// appearing bare in another. Metadata (name, description,
    /// workloads, etc.) is kept from the original. Returns a
    /// note per field where the two files genuinely disagreed.
    pub fn merge(&mut self, other: &Inventory) -> Vec<String> {
        let mut conflicts = Vec::new();
        for pkg in &other.package {
            match self.find_package_mut(&pkg.name) {
                Some(existing) => conflicts.extend(existing.merge_from(pkg)),
                None => self.add_package(pkg.clone()),
            }
        }
        conflicts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pkg(name: &str) -> Package {
        Package {
            name: name.to_string(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
            priority: None,
            retired_on: None,
            unshipped: None,
            archived_builds: None,
        }
    }

    fn make_inventory() -> Inventory {
        let mut inv = Inventory {
            inventory: InventoryMeta {
                name: "test".to_string(),
                description: "test".to_string(),
                maintainer: "tester".to_string(),
                labels: vec![],
                workloads: BTreeMap::from([
                    (
                        "hyperscale".to_string(),
                        WorkloadMeta {
                            packages: vec!["bar".to_string(), "foo".to_string()],
                            ..Default::default()
                        },
                    ),
                    (
                        "epel".to_string(),
                        WorkloadMeta {
                            packages: vec!["foo".to_string()],
                            ..Default::default()
                        },
                    ),
                ]),
                private_fields: vec!["poc".to_string(), "team".to_string()],
            },
            package: vec![],
        };
        let mut bar = make_pkg("bar");
        bar.poc = Some("Team <t@e.com>".to_string());
        bar.team = Some("infra".to_string());
        inv.package.push(bar);

        let mut foo = make_pkg("foo");
        foo.rpms = Some(vec!["foo".to_string(), "foo-libs".to_string()]);
        foo.track = Some("upstream".to_string());
        inv.package.push(foo);

        inv
    }

    #[test]
    fn is_private() {
        let inv = make_inventory();
        assert!(inv.is_private("poc"));
        assert!(inv.is_private("team"));
        assert!(!inv.is_private("reason"));
        assert!(!inv.is_private("name"));
    }

    #[test]
    fn packages_for_workload() {
        let inv = make_inventory();
        let hs = inv.packages_for_workload(Some("hyperscale"));
        assert_eq!(hs.len(), 2);
        let epel = inv.packages_for_workload(Some("epel"));
        assert_eq!(epel.len(), 1);
        assert_eq!(epel[0].name, "foo");
        let all = inv.packages_for_workload(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn unlisted_package_not_in_workload() {
        let mut inv = make_inventory();
        inv.add_package(make_pkg("unlisted"));
        // Not listed in any workload's packages.
        let hs = inv.packages_for_workload(Some("hyperscale"));
        assert!(!hs.iter().any(|p| p.name == "unlisted"));
        // But shows up in unfiltered list.
        let all = inv.packages_for_workload(None);
        assert!(all.iter().any(|p| p.name == "unlisted"));
    }

    #[test]
    fn workload_names() {
        let inv = make_inventory();
        assert_eq!(inv.workload_names(), vec!["epel", "hyperscale"]);
    }

    #[test]
    fn workloads_for_package() {
        let inv = make_inventory();
        assert_eq!(inv.workloads_for_package("bar"), vec!["hyperscale"]);
        assert_eq!(inv.workloads_for_package("foo"), vec!["epel", "hyperscale"]);
        assert!(inv.workloads_for_package("nonexistent").is_empty());
    }

    #[test]
    fn add_to_workload() {
        let mut inv = make_inventory();
        inv.add_to_workload("hyperscale", "newpkg");
        assert!(
            inv.inventory.workloads["hyperscale"]
                .packages
                .contains(&"newpkg".to_string())
        );
        // Duplicate add is a no-op.
        inv.add_to_workload("hyperscale", "newpkg");
        assert_eq!(
            inv.inventory.workloads["hyperscale"]
                .packages
                .iter()
                .filter(|p| *p == "newpkg")
                .count(),
            1
        );
    }

    #[test]
    fn add_to_workload_creates_workload() {
        let mut inv = make_inventory();
        inv.add_to_workload("newwl", "pkg");
        assert!(inv.inventory.workloads.contains_key("newwl"));
        assert_eq!(inv.inventory.workloads["newwl"].packages, vec!["pkg"]);
    }

    #[test]
    fn find_package() {
        let inv = make_inventory();
        assert!(inv.find_package("foo").is_some());
        assert!(inv.find_package("nonexistent").is_none());
    }

    #[test]
    fn add_package_new() {
        let mut inv = make_inventory();
        inv.add_package(make_pkg("aaa"));
        assert_eq!(inv.package.len(), 3);
        // Should be sorted: aaa, bar, foo.
        assert_eq!(inv.package[0].name, "aaa");
    }

    #[test]
    fn add_package_replace() {
        let mut inv = make_inventory();
        let mut pkg = inv.find_package("foo").unwrap().clone();
        pkg.reason = Some("updated reason".to_string());
        inv.add_package(pkg);
        assert_eq!(inv.package.len(), 2);
        assert_eq!(
            inv.find_package("foo").unwrap().reason.as_deref(),
            Some("updated reason")
        );
    }

    #[test]
    fn remove_package() {
        let mut inv = make_inventory();
        assert!(inv.remove_package("foo"));
        assert_eq!(inv.package.len(), 1);
        assert!(!inv.remove_package("nonexistent"));
    }

    #[test]
    fn merge_inventories() {
        let mut inv1 = make_inventory();
        let inv2 = Inventory {
            inventory: InventoryMeta {
                name: "other".to_string(),
                description: "other".to_string(),
                maintainer: "other".to_string(),
                labels: vec![],
                workloads: BTreeMap::new(),
                private_fields: vec![],
            },
            package: vec![make_pkg("new-pkg")],
        };
        inv1.merge(&inv2);
        assert_eq!(inv1.package.len(), 3);
        assert!(inv1.find_package("new-pkg").is_some());
        // Metadata stays from inv1.
        assert_eq!(inv1.inventory.name, "test");
    }

    #[test]
    fn merge_from_set_fields_win_unset_preserved() {
        let mut earlier = make_pkg("foo");
        earlier.poc = Some("alice".to_string());
        earlier.priority = Some(Priority::High);
        let mut later = make_pkg("foo");
        later.reason = Some("rust SIG".to_string());

        let conflicts = earlier.merge_from(&later);
        assert!(conflicts.is_empty());
        // Later file's set field landed...
        assert_eq!(earlier.reason.as_deref(), Some("rust SIG"));
        // ...and its unset fields kept the earlier values.
        assert_eq!(earlier.poc.as_deref(), Some("alice"));
        assert_eq!(earlier.priority, Some(Priority::High));
    }

    #[test]
    fn merge_from_reports_conflicts_later_wins() {
        let mut earlier = make_pkg("foo");
        earlier.priority = Some(Priority::High);
        let mut later = make_pkg("foo");
        later.priority = Some(Priority::Low);

        let conflicts = earlier.merge_from(&later);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("priority"), "{}", conflicts[0]);
        assert_eq!(earlier.priority, Some(Priority::Low));
    }

    #[test]
    fn merge_from_combines_retirement_knowledge() {
        let mut earlier = make_pkg("foo");
        earlier.retired_on = Some(vec!["rawhide".to_string()]);
        earlier.unshipped = Some("dist-git project gone (404)".to_string());
        let mut later = make_pkg("foo");
        later.retired_on = Some(vec!["epel9".to_string(), "rawhide".to_string()]);

        let conflicts = earlier.merge_from(&later);
        assert!(conflicts.is_empty());
        // retired_on unioned, not replaced.
        assert_eq!(
            earlier.retired_on,
            Some(vec!["epel9".to_string(), "rawhide".to_string()])
        );
        // A bare later entry doesn't erase the unshipped marker.
        assert!(earlier.is_unshipped());
    }

    #[test]
    fn merge_preserves_markers_from_earlier_files() {
        // The original motivation: a package marked unshipped in
        // one inventory also appears bare in a later-merged one.
        let mut inv1 = make_inventory();
        inv1.find_package_mut("foo").unwrap().unshipped = Some("gone".to_string());
        let inv2 = Inventory {
            inventory: InventoryMeta {
                name: "other".to_string(),
                description: "other".to_string(),
                maintainer: "other".to_string(),
                labels: vec![],
                workloads: BTreeMap::new(),
                private_fields: vec![],
            },
            package: vec![make_pkg("foo")],
        };
        let conflicts = inv1.merge(&inv2);
        assert!(conflicts.is_empty());
        assert!(inv1.find_package("foo").unwrap().is_unshipped());
        // Still two packages — merged, not duplicated.
        assert_eq!(inv1.package.len(), 2);
    }

    #[test]
    fn priority_ordering() {
        // The ordering matters: max() over workload defaults
        // must pick the most-important one.
        assert!(Priority::Urgent > Priority::High);
        assert!(Priority::High > Priority::Medium);
        assert!(Priority::Medium > Priority::Low);
        assert!(Priority::Low > Priority::Unspecified);
    }

    #[test]
    fn priority_serializes_lowercase() {
        let toml_str = toml::to_string(&PriorityWrapper { p: Priority::High }).unwrap();
        assert!(toml_str.contains("p = \"high\""));
        let back: PriorityWrapper = toml::from_str("p = \"urgent\"").unwrap();
        assert_eq!(back.p, Priority::Urgent);
    }

    #[derive(Serialize, Deserialize)]
    struct PriorityWrapper {
        p: Priority,
    }

    #[test]
    fn priority_for_returns_none_when_unset() {
        let inv = make_inventory();
        assert_eq!(inv.priority_for("foo"), None);
    }

    #[test]
    fn priority_for_explicit_package_field_wins() {
        let mut inv = make_inventory();
        inv.find_package_mut("foo").unwrap().priority = Some(Priority::High);
        // Add a workload default that would otherwise apply.
        inv.inventory
            .workloads
            .get_mut("hyperscale")
            .unwrap()
            .default_priority = Some(Priority::Urgent);
        // Package field wins even when the workload would be
        // strictly higher.
        assert_eq!(inv.priority_for("foo"), Some(Priority::High));
    }

    #[test]
    fn priority_for_falls_back_to_workload_default() {
        let mut inv = make_inventory();
        inv.inventory
            .workloads
            .get_mut("hyperscale")
            .unwrap()
            .default_priority = Some(Priority::Medium);
        assert_eq!(inv.priority_for("bar"), Some(Priority::Medium));
    }

    #[test]
    fn priority_for_picks_max_across_workloads() {
        let mut inv = make_inventory();
        // `foo` is in both hyperscale and epel.
        inv.inventory
            .workloads
            .get_mut("hyperscale")
            .unwrap()
            .default_priority = Some(Priority::Low);
        inv.inventory
            .workloads
            .get_mut("epel")
            .unwrap()
            .default_priority = Some(Priority::High);
        assert_eq!(inv.priority_for("foo"), Some(Priority::High));
    }

    #[test]
    fn priority_for_explicit_unspecified_opts_out_of_workload_default() {
        let mut inv = make_inventory();
        inv.inventory
            .workloads
            .get_mut("hyperscale")
            .unwrap()
            .default_priority = Some(Priority::Urgent);
        inv.find_package_mut("foo").unwrap().priority = Some(Priority::Unspecified);
        assert_eq!(inv.priority_for("foo"), Some(Priority::Unspecified));
    }

    #[test]
    fn priority_for_unknown_package_is_none() {
        let inv = make_inventory();
        assert_eq!(inv.priority_for("does-not-exist"), None);
    }
}
