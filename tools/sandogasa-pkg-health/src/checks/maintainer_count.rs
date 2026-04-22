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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use sandogasa_bugzilla::BzClient;
    use sandogasa_distgit::DistGitClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn make_ctx(server_uri: &str) -> Context {
        Context::for_test(
            Arc::new(BzClient::new(server_uri)),
            Arc::new(DistGitClient::with_base_url(server_uri)),
            BTreeMap::new(),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn direct_maintainers_only() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/foo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_users": {
                    "owner": ["alice"],
                    "admin": ["bob"],
                    "commit": ["carol"],
                    "collaborator": [],
                    "ticket": []
                },
                "access_groups": {
                    "admin": [],
                    "commit": [],
                    "collaborator": [],
                    "ticket": []
                },
                "name": "foo",
                "namespace": "rpms"
            })))
            .mount(&server)
            .await;

        let ctx = make_ctx(&server.uri());
        let result = MaintainerCount.run("foo", None, &ctx).unwrap();
        let data = &result.data;
        assert_eq!(data["direct"].as_array().unwrap().len(), 3);
        assert_eq!(data["groups"].as_array().unwrap().len(), 0);
        assert_eq!(data["effective_count"], 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expands_group_members() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/bar"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_users": {
                    "owner": ["alice"],
                    "admin": [], "commit": [], "collaborator": [], "ticket": []
                },
                "access_groups": {
                    "admin": [], "commit": ["rust-sig"],
                    "collaborator": [], "ticket": []
                },
                "name": "bar",
                "namespace": "rpms"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/0/group/rust-sig"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "display_name": "Rust SIG",
                "description": "",
                "creator": {"name": "alice"},
                "date_created": "0",
                "group_type": "user",
                "members": ["alice", "bob", "carol"],
                "name": "rust-sig"
            })))
            .mount(&server)
            .await;

        let ctx = make_ctx(&server.uri());
        let result = MaintainerCount.run("bar", None, &ctx).unwrap();
        let data = &result.data;
        assert_eq!(data["direct"].as_array().unwrap().len(), 1);
        assert_eq!(data["groups"].as_array().unwrap().len(), 1);
        // alice is both direct and in rust-sig, deduped.
        assert_eq!(data["effective_count"], 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_group_falls_through() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/baz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_users": {
                    "owner": ["alice"],
                    "admin": [], "commit": [], "collaborator": [], "ticket": []
                },
                "access_groups": {
                    "admin": [], "commit": ["ghost-sig"],
                    "collaborator": [], "ticket": []
                },
                "name": "baz",
                "namespace": "rpms"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/0/group/ghost-sig"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let ctx = make_ctx(&server.uri());
        // Should not error — missing group is a warning, not fatal.
        let result = MaintainerCount.run("baz", None, &ctx).unwrap();
        // Only direct maintainer counted.
        assert_eq!(result.data["effective_count"], 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn acl_lookup_failure_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/0/rpms/missing"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let ctx = make_ctx(&server.uri());
        let result = MaintainerCount.run("missing", None, &ctx);
        assert!(result.is_err());
    }
}
