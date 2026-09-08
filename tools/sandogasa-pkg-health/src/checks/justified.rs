// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether a package is kept on purpose.
//!
//! The kondo workflow's central question, asked here as a standing
//! fact rather than at cull time: is the package named by any of the
//! workspace's essential inventories — a keep, a walked closure, a
//! derived dependency inventory, a retired-but-kept list? One that is
//! not is one `poi-tracker kondo` would offer to cull, and a report
//! that says so over time shows the drift as it happens.

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

pub struct Justified;

impl HealthCheck for Justified {
    fn id(&self) -> &'static str {
        "justified"
    }

    fn description(&self) -> &'static str {
        "Whether an essential inventory of the workspace names the package (needs -w)"
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
            data: serde_json::json!({ "justified": ws.essential.contains(package) }),
        })
    }

    fn format_result(&self, data: &serde_json::Value) -> String {
        if data["justified"].as_bool().unwrap_or(false) {
            "in an essential inventory".to_string()
        } else {
            "in no essential inventory — kondo would offer to cull it".to_string()
        }
    }
}
