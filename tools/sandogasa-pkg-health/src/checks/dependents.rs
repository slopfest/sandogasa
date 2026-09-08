// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Who depends on a package, off the branch's graph.
//!
//! The counterpart of `dependency_health`: not what the package needs
//! but what needs it. A package nothing depends on is a leaf, kept for
//! its own sake or not at all; one carried by other inventory packages
//! is there because they are; one needed only from outside the
//! inventory is someone else's dependency you happen to hold. A
//! package whose binaries are all `-devel` is a library nothing runs on
//! its own — the shape that is almost always tracked as a mere
//! dependency, the same marker `poi-tracker dependents` uses.

use std::collections::BTreeSet;

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::{Context, WorkspaceFacts};

/// The verdict for `package` from the workspace's graph facts.
pub fn verdict(ws: &WorkspaceFacts, package: &str) -> serde_json::Value {
    if !ws.graph_known.contains(package) {
        return serde_json::json!({ "known": false });
    }
    let all: BTreeSet<&String> = ws.dependents.get(package).into_iter().flatten().collect();
    let in_inventory: Vec<&String> = all
        .iter()
        .copied()
        .filter(|d| ws.inventory.contains(*d))
        .collect();
    let outside: Vec<&String> = all
        .iter()
        .copied()
        .filter(|d| !ws.inventory.contains(*d))
        .collect();
    let bins = ws.binaries.get(package).cloned().unwrap_or_default();
    let devel_only = !bins.is_empty() && bins.iter().all(|b| b.ends_with("-devel"));
    serde_json::json!({
        "known": true,
        "dependents": all.len(),
        "in_inventory": in_inventory,
        "outside": outside,
        "devel_only": devel_only,
    })
}

/// One line for the verdict.
pub fn format(data: &serde_json::Value) -> String {
    if !data["known"].as_bool().unwrap_or(false) {
        return "not in the graph (walk it again, or the package predates it)".to_string();
    }
    let names = |k: &str| -> Vec<&str> {
        data[k]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default()
    };
    let marker = if data["devel_only"].as_bool().unwrap_or(false) {
        " [devel-only]"
    } else {
        ""
    };
    let (inside, outside) = (names("in_inventory"), names("outside"));
    if inside.is_empty() && outside.is_empty() {
        return format!("leaf — nothing in the graph depends on it{marker}");
    }
    let mut parts = Vec::new();
    if !inside.is_empty() {
        parts.push(format!("needed by {}", inside.join(", ")));
    }
    if !outside.is_empty() {
        parts.push(format!(
            "needed outside the inventory by {}",
            if outside.len() > 5 {
                format!(
                    "{} packages ({}, …)",
                    outside.len(),
                    outside[..5].join(", ")
                )
            } else {
                outside.join(", ")
            }
        ));
    }
    format!("{}{marker}", parts.join("; "))
}

pub struct Dependents;

impl HealthCheck for Dependents {
    fn id(&self) -> &'static str {
        "dependents"
    }

    fn description(&self) -> &'static str {
        "Who depends on the package, off the workspace's graph (needs -w)"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Cheap
    }

    fn needs_workspace(&self) -> bool {
        true
    }

    fn run(
        &self,
        package: &str,
        _variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String> {
        let ws = ctx.workspace.as_ref().ok_or("needs a workspace (-w)")?;
        Ok(CheckResult {
            data: verdict(ws, package),
        })
    }

    fn format_result(&self, data: &serde_json::Value) -> String {
        format(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> WorkspaceFacts {
        let mut ws = WorkspaceFacts::default();
        ws.inventory.extend([
            "app".to_string(),
            "rust-quiet".to_string(),
            "rust-anyhow".to_string(),
        ]);
        ws.graph_known
            .extend(["app", "rust-quiet", "rust-anyhow", "rust-syn", "pandoc"].map(String::from));
        ws.dependents.insert(
            "rust-anyhow".into(),
            ["app".to_string(), "pandoc".to_string()].into(),
        );
        ws.dependents
            .insert("rust-syn".into(), ["pandoc".to_string()].into());
        ws.binaries
            .insert("rust-anyhow".into(), vec!["rust-anyhow-devel".into()]);
        ws.binaries
            .insert("app".into(), vec!["app".into(), "app-devel".into()]);
        ws
    }

    #[test]
    fn leaves_carried_and_external_read_differently() {
        let ws = facts();
        assert_eq!(
            format(&verdict(&ws, "app")),
            "leaf — nothing in the graph depends on it"
        );
        assert_eq!(
            format(&verdict(&ws, "rust-anyhow")),
            "needed by app; needed outside the inventory by pandoc [devel-only]"
        );
        assert_eq!(
            format(&verdict(&ws, "rust-syn")),
            "needed outside the inventory by pandoc"
        );
        assert!(format(&verdict(&ws, "never-walked")).starts_with("not in the graph"));
    }
}
