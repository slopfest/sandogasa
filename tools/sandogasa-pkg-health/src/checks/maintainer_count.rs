// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Count of direct maintainers and group co-maintainers.
//!
//! Queries dist-git ACLs for the package, then expands groups via
//! the Pagure group members API. The "effective count" is the
//! number of unique usernames who can commit to the package
//! (directly or via a group).

use std::collections::BTreeSet;

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
        // Cheap-tier despite N+1 API calls (one per group) because
        // groups are typically few and the queries are fast.
        CostTier::Cheap
    }

    fn run(
        &self,
        package: &str,
        _variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String> {
        let acls = ctx
            .block_on(ctx.distgit.get_acls(package))
            .map_err(|e| format!("dist-git ACL lookup failed: {e}"))?;

        // Direct users with commit-level or higher access.
        let mut direct: BTreeSet<String> = BTreeSet::new();
        direct.extend(acls.access_users.owner.iter().cloned());
        direct.extend(acls.access_users.admin.iter().cloned());
        direct.extend(acls.access_users.commit.iter().cloned());

        // Groups with commit-level or higher access.
        let mut groups: BTreeSet<String> = BTreeSet::new();
        groups.extend(acls.access_groups.admin.iter().cloned());
        groups.extend(acls.access_groups.commit.iter().cloned());

        // Expand groups to members.
        let mut effective: BTreeSet<String> = direct.clone();
        for group in &groups {
            match ctx.block_on(ctx.distgit.get_group_members(group)) {
                Ok(members) => effective.extend(members),
                Err(e) => {
                    // Skip this group but keep going — better to
                    // under-count than fail the whole check.
                    eprintln!("warning: {package}: failed to expand group '{group}': {e}");
                }
            }
        }

        Ok(CheckResult {
            data: serde_json::json!({
                "direct": direct,
                "groups": groups,
                "effective_count": effective.len(),
            }),
        })
    }
}
