// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Count open bugs by category for a package, per Fedora release.
//!
//! Queries Bugzilla once for all open bugs filed against the
//! package's component (resolution=---), then classifies each bug
//! against the FTBFS/FTI trackers for the specific release variant.
//! Returns one result per variant so each release can be aged /
//! re-run independently.

use std::collections::BTreeMap;

use sandogasa_bugclass::bugzilla::classify;

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

pub struct BugCount;

impl HealthCheck for BugCount {
    fn id(&self) -> &'static str {
        "bug_count"
    }

    fn description(&self) -> &'static str {
        "Count of open bugs by category (security, ftbfs, update request, etc.)"
    }

    fn cost_tier(&self) -> CostTier {
        CostTier::Medium
    }

    fn variants(&self, ctx: &Context) -> Vec<Option<String>> {
        // Always include rawhide; add each explicitly requested Fedora
        // and EPEL version. Users update each independently via
        // --fedora-version / --epel-version + --max-age.
        let mut vs: Vec<Option<String>> = vec![Some("rawhide".to_string())];
        for ver in &ctx.fedora_versions {
            vs.push(Some(format!("f{ver}")));
        }
        for ver in &ctx.epel_versions {
            vs.push(Some(format!("epel{ver}")));
        }
        vs
    }

    fn run(
        &self,
        package: &str,
        variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String> {
        let variant = variant.ok_or("bug_count requires a variant")?;
        let trackers = ctx
            .trackers
            .get(variant)
            .ok_or_else(|| format!("no trackers loaded for variant '{variant}'"))?;

        let query =
            format!("product=Fedora&product=Fedora EPEL&component={package}&resolution=---");
        let bugs = ctx
            .block_on(ctx.bz.search(&query, 0))
            .map_err(|e| format!("Bugzilla search failed: {e}"))?;

        let mut by_kind: BTreeMap<&str, u64> = BTreeMap::new();
        for bug in &bugs {
            let kind = classify(bug, trackers);
            *by_kind.entry(kind.as_str()).or_insert(0) += 1;
        }

        Ok(CheckResult {
            data: serde_json::json!({
                "open": bugs.len(),
                "by_kind": by_kind,
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use sandogasa_bugclass::BugKind;
    use sandogasa_bugclass::bugzilla::TrackerIds;
    use sandogasa_bugzilla::BzClient;
    use sandogasa_distgit::DistGitClient;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    #[test]
    fn id_and_tier() {
        let check = BugCount;
        assert_eq!(check.id(), "bug_count");
        assert_eq!(check.cost_tier(), CostTier::Medium);
    }

    #[test]
    fn kind_string_ids_match() {
        // Regression guard — the report uses these strings as keys.
        assert_eq!(BugKind::Security.as_str(), "security");
        assert_eq!(BugKind::Ftbfs.as_str(), "ftbfs");
        assert_eq!(BugKind::Fti.as_str(), "fti");
        assert_eq!(BugKind::Update.as_str(), "update");
        assert_eq!(BugKind::Branch.as_str(), "branch");
        assert_eq!(BugKind::Other.as_str(), "other");
        assert_eq!(BugKind::Review.as_str(), "review");
    }

    /// Build a Bugzilla bug JSON with the commonly-needed fields.
    fn bug_json(
        id: u64,
        summary: &str,
        component: &str,
        keywords: &[&str],
        blocks: &[u64],
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": [component],
            "severity": "",
            "priority": "",
            "assigned_to": "",
            "creator": "",
            "creation_time": "2026-01-01T00:00:00Z",
            "last_change_time": "2026-01-01T00:00:00Z",
            "keywords": keywords,
            "alias": [],
            "depends_on": [],
            "blocks": blocks,
            "see_also": [],
            "cc": [],
            "flags": [],
            "version": [],
            "cf_fixed_in": ""
        })
    }

    fn make_ctx(server_uri: &str, rawhide_trackers: TrackerIds) -> Context {
        let mut trackers = BTreeMap::new();
        trackers.insert("rawhide".to_string(), Arc::new(rawhide_trackers));
        Context::for_test(
            Arc::new(BzClient::new(server_uri)),
            Arc::new(DistGitClient::with_base_url(server_uri)),
            trackers,
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn counts_and_classifies_bugs() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [
                    bug_json(1, "CVE-2026-1234 foo: overflow", "foo", &[], &[]),
                    bug_json(2, "foo: typo in help text", "foo", &[], &[]),
                    bug_json(3, "foo FTBFS in rawhide", "foo", &[], &[999]),
                ],
                "total_matches": 3
            })))
            .mount(&server)
            .await;

        let trackers = TrackerIds {
            ftbfs: HashSet::from([999]),
            fti: HashSet::new(),
        };
        let ctx = make_ctx(&server.uri(), trackers);
        let result = BugCount.run("foo", Some("rawhide"), &ctx).unwrap();

        assert_eq!(result.data["open"], 3);
        assert_eq!(result.data["by_kind"]["security"], 1);
        assert_eq!(result.data["by_kind"]["ftbfs"], 1);
        assert_eq!(result.data["by_kind"]["other"], 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_search_zero_open() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": [],
                "total_matches": 0
            })))
            .mount(&server)
            .await;

        let ctx = make_ctx(&server.uri(), TrackerIds::default());
        let result = BugCount.run("foo", Some("rawhide"), &ctx).unwrap();
        assert_eq!(result.data["open"], 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_variant_is_error() {
        let server = MockServer::start().await;
        let ctx = make_ctx(&server.uri(), TrackerIds::default());
        let err = BugCount.run("foo", Some("f44"), &ctx).unwrap_err();
        assert!(err.contains("no trackers loaded for variant 'f44'"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_variant_is_error() {
        let server = MockServer::start().await;
        let ctx = make_ctx(&server.uri(), TrackerIds::default());
        let err = BugCount.run("foo", None, &ctx).unwrap_err();
        assert!(err.contains("requires a variant"));
    }

    #[test]
    fn variants_include_rawhide_and_requested() {
        let ctx = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let mut c = Context::for_test(
                    Arc::new(BzClient::new("http://unused")),
                    Arc::new(DistGitClient::with_base_url("http://unused")),
                    BTreeMap::new(),
                );
                c.fedora_versions = vec![44, 45];
                c.epel_versions = vec![10];
                c
            });
        let vs = BugCount.variants(&ctx);
        let names: Vec<&str> = vs.iter().filter_map(|v| v.as_deref()).collect();
        assert_eq!(names, vec!["rawhide", "f44", "f45", "epel10"]);
    }
}
