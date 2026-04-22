// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Count of direct maintainers and group co-maintainers.
//!
//! Stub implementation — full FASJSON integration comes later.

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

pub struct MaintainerCount;

impl HealthCheck for MaintainerCount {
    fn id(&self) -> &'static str {
        "maintainer_count"
    }

    fn description(&self) -> &'static str {
        "Count of direct maintainers and group co-maintainers"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Cheap
    }

    fn run(&self, _package: &str, _ctx: &Context) -> Result<CheckResult, String> {
        // TODO: implement via sandogasa-distgit ACL lookup + FASJSON
        // group expansion. Stub returns placeholder data so the
        // framework is testable end-to-end.
        Ok(CheckResult {
            data: serde_json::json!({
                "direct": [],
                "groups": [],
                "effective_count": 0,
            }),
        })
    }
}
