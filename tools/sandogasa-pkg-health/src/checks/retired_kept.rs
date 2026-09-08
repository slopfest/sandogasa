// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A package retired in rawhide yet still kept as essential.
//!
//! Retirement is the end of a package's life in rawhide, but an older
//! release may still need it, and the workspace has a place for that:
//! the `retired` inventories, kept knowingly and never walked. A
//! retired package that sits in a *walked* essential inventory instead
//! is a keep nobody revisited — the walk will not find it, and the
//! entry says nothing true any more.

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

/// The verdict from the two facts: retired in rawhide, and where the
/// workspace keeps it.
pub fn verdict(retired: bool, essential: bool, in_retired_inventory: bool) -> serde_json::Value {
    serde_json::json!({
        "retired_in_rawhide": retired,
        "essential": essential,
        "in_retired_inventory": in_retired_inventory,
        "misplaced": retired && essential && !in_retired_inventory,
    })
}

pub fn format(data: &serde_json::Value) -> String {
    let b = |k: &str| data[k].as_bool().unwrap_or(false);
    match (
        b("retired_in_rawhide"),
        b("in_retired_inventory"),
        b("essential"),
    ) {
        (false, _, _) => "active in rawhide".to_string(),
        (true, true, _) => "retired in rawhide, kept knowingly (retired inventory)".to_string(),
        (true, false, true) => {
            "retired in rawhide but kept as essential — move it to a retired inventory, or drop it"
                .to_string()
        }
        (true, false, false) => "retired in rawhide, and in no essential inventory".to_string(),
    }
}

pub struct RetiredKept;

impl HealthCheck for RetiredKept {
    fn id(&self) -> &'static str {
        "retired_kept"
    }

    fn description(&self) -> &'static str {
        "Retired in rawhide yet kept as essential outside a retired inventory (needs -w)"
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
        let retired = ctx
            .block_on(ctx.distgit.is_retired(package, "rawhide"))
            .map_err(|e| format!("dist-git retirement lookup failed: {e}"))?;
        Ok(CheckResult {
            data: verdict(
                retired,
                ws.essential.contains(package),
                ws.retired_kept.contains(package),
            ),
        })
    }

    fn format_result(&self, data: &serde_json::Value) -> String {
        format(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_retired_package_kept_outside_the_retired_inventory_is_misplaced() {
        assert!(!verdict(false, true, false)["misplaced"].as_bool().unwrap());
        assert!(!verdict(true, true, true)["misplaced"].as_bool().unwrap());
        assert!(verdict(true, true, false)["misplaced"].as_bool().unwrap());
        assert!(format(&verdict(true, true, false)).contains("move it to a retired inventory"));
        assert_eq!(format(&verdict(false, false, false)), "active in rawhide");
    }
}
