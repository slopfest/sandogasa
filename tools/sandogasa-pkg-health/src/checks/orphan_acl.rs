// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Orphaned, but you still hold an ACL.
//!
//! The python-usort shape from the kondo close-out: a package orphaned
//! — its owner handed to the `orphan` sentinel — while the workspace's
//! user still has commit or admin access on it. Nothing is wrong with
//! the package's data, but the lingering ACL keeps it on your lists
//! (dist-git counts you as a maintainer, `sync-distgit` keeps pulling
//! it into the owned inventory) for a package you gave up. Either
//! adopt it back (`poi-tracker adopt`) or drop the ACL.

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

/// Dist-git user that owns orphaned packages.
const ORPHAN_USER: &str = "orphan";

/// The verdict: who owns it, and whether `user` holds an ACL there.
pub fn verdict(
    owner: &[String],
    admin: &[String],
    commit: &[String],
    user: Option<&str>,
) -> serde_json::Value {
    let orphaned = owner.iter().any(|u| u == ORPHAN_USER);
    let holds = user.is_some_and(|u| owner.iter().chain(admin).chain(commit).any(|x| x == u));
    serde_json::json!({
        "orphaned": orphaned,
        "user": user,
        "holds_acl": holds,
        "lingering": orphaned && holds,
    })
}

pub fn format(data: &serde_json::Value) -> String {
    let b = |k: &str| data[k].as_bool().unwrap_or(false);
    match (b("orphaned"), b("holds_acl")) {
        (true, true) => format!(
            "orphaned, but {} still holds an ACL — adopt it (poi-tracker adopt) or drop the ACL",
            data["user"].as_str().unwrap_or("you")
        ),
        (true, false) => "orphaned".to_string(),
        (false, _) => "not orphaned".to_string(),
    }
}

pub struct OrphanAcl;

impl HealthCheck for OrphanAcl {
    fn id(&self) -> &'static str {
        "orphan_acl"
    }

    fn description(&self) -> &'static str {
        "Orphaned while the workspace's user still holds an ACL on it (needs -w)"
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
        let acls = ctx
            .block_on(ctx.distgit.get_acls(package))
            .map_err(|e| format!("dist-git ACL lookup failed: {e}"))?;
        Ok(CheckResult {
            data: verdict(
                &acls.access_users.owner,
                &acls.access_users.admin,
                &acls.access_users.commit,
                ws.user.as_deref(),
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

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_lingering_acl_is_the_orphaned_package_you_still_have_access_to() {
        let d = verdict(&v(&["orphan"]), &[], &v(&["salimma"]), Some("salimma"));
        assert!(d["lingering"].as_bool().unwrap());
        assert!(format(&d).contains("salimma still holds an ACL"));
        // Orphaned without you: nothing lingers.
        let d = verdict(&v(&["orphan"]), &[], &v(&["someone"]), Some("salimma"));
        assert!(!d["lingering"].as_bool().unwrap());
        assert_eq!(format(&d), "orphaned");
        // Owned by you: not orphaned, whatever the ACL.
        let d = verdict(&v(&["salimma"]), &[], &[], Some("salimma"));
        assert_eq!(format(&d), "not orphaned");
        // No user in the workspace: no lingering can be told.
        assert!(
            !verdict(&v(&["orphan"]), &[], &v(&["salimma"]), None)["lingering"]
                .as_bool()
                .unwrap()
        );
    }
}
