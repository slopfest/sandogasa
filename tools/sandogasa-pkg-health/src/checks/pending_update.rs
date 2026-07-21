// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pending upstream update and its semver impact.
//!
//! The persisted, aged counterpart of poi-tracker's `semver-audit`:
//! finds the package's open release-monitoring bug ("X is
//! available", filed by Anitya / the-new-hotness), compares the
//! advertised version against the rawhide dist-git spec, and
//! classifies the bump via `sandogasa_bugclass::semver` (Cargo's
//! compatibility rule).
//!
//! A spec that already matches the advertised version is only
//! reported as a stale bug after verifying (via the Context's Koji
//! lookup, when available) that rawhide's tag chain actually
//! carries a build with it — a version committed and built only
//! into a side tag is "committed, awaiting release", not stale.

use sandogasa_bugclass::bugzilla::extract_new_version;
use sandogasa_bugclass::semver::{
    Bump, classify_with_status, numeric_components, version_at_least,
};
use sandogasa_bugzilla::models::Bug;
use sandogasa_distgit::spec::parse_version;

use crate::check::{CheckResult, CostTier, HealthCheck};
use crate::context::Context;

/// Reporter address of Fedora's release-monitoring bot.
const RELEASE_MONITORING_REPORTER: &str = "upstream-release-monitoring@fedoraproject.org";

/// The dist-git branch whose spec gives the "current" version;
/// also the Koji alias tag that resolves to the current `fNN`.
const CURRENT_BRANCH: &str = "rawhide";

pub struct PendingUpdate;

impl HealthCheck for PendingUpdate {
    fn id(&self) -> &'static str {
        "pending_update"
    }

    fn description(&self) -> &'static str {
        "Pending upstream update and its semver impact (breaking / non-breaking)"
    }

    fn cost_tier(&self) -> CostTier {
        // Bugzilla search + dist-git spec fetch (+ a koji query
        // for the stale case) per package.
        CostTier::Medium
    }

    fn run(
        &self,
        package: &str,
        _variant: Option<&str>,
        ctx: &Context,
    ) -> Result<CheckResult, String> {
        let query = format!(
            "product=Fedora&product=Fedora EPEL&component={package}\
             &reporter={RELEASE_MONITORING_REPORTER}&bug_status=__open__"
        );
        let bugs = ctx
            .block_on(ctx.bz.search(&query, 0))
            .map_err(|e| format!("Bugzilla search failed: {e}"))?;

        let Some((bug_id, new)) = pick_latest(&bugs, package) else {
            return Ok(CheckResult {
                data: serde_json::json!({ "pending": false }),
            });
        };

        // Current rawhide version from the spec; when unreadable,
        // distinguish a retired package from a genuine unknown.
        let current = match ctx.block_on(ctx.distgit.fetch_spec(package, CURRENT_BRANCH)) {
            Ok(spec) => parse_version(&spec),
            Err(_) => None,
        };
        let retired = current.is_none()
            && ctx
                .block_on(ctx.distgit.is_retired(package, CURRENT_BRANCH))
                .unwrap_or(false);

        let mut bump = classify_with_status(current.as_deref(), &new, retired);
        if current.as_deref() == Some(new.as_str()) {
            bump = Bump::UpToDate;
        }
        // "Spec equals available" only means stale when rawhide
        // actually carries the build; only in a side tag / gating
        // means the update is in flight. Without a Koji lookup the
        // spec-only verdict stands (warned once at startup).
        if bump == Bump::UpToDate
            && let Some(koji) = &ctx.koji
        {
            match koji(CURRENT_BRANCH, package) {
                Ok(nvr) => {
                    let shipped = nvr
                        .as_deref()
                        .and_then(sandogasa_koji::parse_nvr)
                        .is_some_and(|(name, v, _)| name == package && version_at_least(v, &new));
                    if !shipped {
                        bump = Bump::PendingRelease;
                    }
                }
                Err(e) => {
                    eprintln!(
                        "warning: {package}: Koji {CURRENT_BRANCH} query failed: {e} \
                         (cannot verify staleness)"
                    );
                }
            }
        }

        Ok(CheckResult {
            data: serde_json::json!({
                "pending": true,
                "current": current,
                "new": new,
                "bump": bump,
                "bug_id": bug_id,
            }),
        })
    }

    fn format_result(&self, data: &serde_json::Value) -> String {
        if !data["pending"].as_bool().unwrap_or(false) {
            return "none pending".to_string();
        }
        let current = data["current"].as_str().unwrap_or("?");
        let new = data["new"].as_str().unwrap_or("?");
        let bump = data["bump"].as_str().unwrap_or("?");
        let bug_id = data["bug_id"].as_u64().unwrap_or(0);
        match bump {
            "up-to-date" => format!("up to date; stale bug rhbz#{bug_id}"),
            "pending-release" => {
                format!("{new} committed, awaiting release (rhbz#{bug_id})")
            }
            _ => format!("{current} -> {new} [{bump}] (rhbz#{bug_id})"),
        }
    }
}

/// Choose the bug advertising the highest new version among a
/// package's open release-monitoring bugs. Bugs whose version
/// can't be parsed sort below numeric ones; if none parse, the
/// first bug with an extractable version string wins.
fn pick_latest(bugs: &[Bug], component: &str) -> Option<(u64, String)> {
    let mut best: Option<(u64, String, Option<Vec<u64>>)> = None;
    for bug in bugs {
        let Some(version) = extract_new_version(&bug.summary, component) else {
            continue;
        };
        let parsed = numeric_components(&version);
        let better = match &best {
            None => true,
            Some((_, _, best_parsed)) => match (&parsed, best_parsed) {
                (Some(a), Some(b)) => a > b,
                (Some(_), None) => true,
                _ => false,
            },
        };
        if better {
            best = Some((bug.id, version, parsed));
        }
    }
    best.map(|(id, version, _)| (id, version))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use sandogasa_bugzilla::BzClient;
    use sandogasa_distgit::DistGitClient;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::context::KojiLookup;

    fn make_ctx(server_uri: &str, koji: Option<Arc<KojiLookup>>) -> Context {
        Context::for_test(
            Arc::new(BzClient::new(server_uri)),
            Arc::new(DistGitClient::with_base_url(server_uri)),
            BTreeMap::new(),
            koji,
        )
    }

    fn bug_json(id: u64, summary: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": ["foo"],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "nobody@fedoraproject.org",
            "creator": RELEASE_MONITORING_REPORTER,
            "creation_time": "2026-05-01T00:00:00Z",
            "last_change_time": "2026-05-01T00:00:00Z",
        })
    }

    async fn mount_bugs(server: &MockServer, bugs: Vec<serde_json::Value>) {
        let total = bugs.len();
        Mock::given(method("GET"))
            .and(path("/rest/bug"))
            .and(query_param("component", "foo"))
            .and(query_param("reporter", RELEASE_MONITORING_REPORTER))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "bugs": bugs,
                "total_matches": total
            })))
            .mount(server)
            .await;
    }

    async fn mount_spec(server: &MockServer, version: &str) {
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/foo.spec"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!(
                "Name: foo\nVersion: {version}\nRelease: 1%{{?dist}}\n"
            )))
            .mount(server)
            .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_open_bug_means_none_pending() {
        let server = MockServer::start().await;
        mount_bugs(&server, vec![]).await;
        let ctx = make_ctx(&server.uri(), None);
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["pending"], false);
        assert_eq!(PendingUpdate.format_result(&result.data), "none pending");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn classifies_breaking_bump_and_picks_highest() {
        let server = MockServer::start().await;
        mount_bugs(
            &server,
            vec![
                bug_json(1, "foo-1.5.0 is available"),
                bug_json(2, "foo-2.0.0 is available"),
            ],
        )
        .await;
        mount_spec(&server, "1.4.0").await;
        let ctx = make_ctx(&server.uri(), None);
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["pending"], true);
        assert_eq!(result.data["bump"], "breaking");
        assert_eq!(result.data["bug_id"], 2);
        let line = PendingUpdate.format_result(&result.data);
        assert_eq!(line, "1.4.0 -> 2.0.0 [breaking] (rhbz#2)");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stale_bug_requires_koji_confirmation() {
        // Spec matches the advertised version. With a Koji lookup
        // that carries the build -> up to date; with one that only
        // has the old version (side tag case) -> pending release.
        let server = MockServer::start().await;
        mount_bugs(&server, vec![bug_json(7, "foo-1.2.0 is available")]).await;
        mount_spec(&server, "1.2.0").await;

        let shipped: Arc<KojiLookup> = Arc::new(|tag: &str, pkg: &str| {
            assert_eq!((tag, pkg), ("rawhide", "foo"));
            Ok(Some("foo-1.2.0-1.fc45".to_string()))
        });
        let ctx = make_ctx(&server.uri(), Some(shipped));
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["bump"], "up-to-date");
        assert_eq!(
            PendingUpdate.format_result(&result.data),
            "up to date; stale bug rhbz#7"
        );

        let side_tag_only: Arc<KojiLookup> =
            Arc::new(|_: &str, _: &str| Ok(Some("foo-1.1.0-1.fc45".to_string())));
        let ctx = make_ctx(&server.uri(), Some(side_tag_only));
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["bump"], "pending-release");
        assert_eq!(
            PendingUpdate.format_result(&result.data),
            "1.2.0 committed, awaiting release (rhbz#7)"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_koji_keeps_spec_verdict_and_error_warns_through() {
        let server = MockServer::start().await;
        mount_bugs(&server, vec![bug_json(7, "foo-1.2.0 is available")]).await;
        mount_spec(&server, "1.2.0").await;

        // koji CLI unavailable -> spec-only verdict.
        let ctx = make_ctx(&server.uri(), None);
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["bump"], "up-to-date");

        // Transient koji failure -> warn, keep the verdict.
        let failing: Arc<KojiLookup> = Arc::new(|_: &str, _: &str| Err("koji down".to_string()));
        let ctx = make_ctx(&server.uri(), Some(failing));
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["bump"], "up-to-date");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retired_package_reports_retired() {
        let server = MockServer::start().await;
        mount_bugs(&server, vec![bug_json(7, "foo-1.2.0 is available")]).await;
        // No spec mock -> fetch fails; dead.package present.
        Mock::given(method("GET"))
            .and(path("/rpms/foo/raw/rawhide/f/dead.package"))
            .respond_with(ResponseTemplate::new(200).set_body_string("retired\n"))
            .mount(&server)
            .await;
        let ctx = make_ctx(&server.uri(), None);
        let result = PendingUpdate.run("foo", None, &ctx).unwrap();
        assert_eq!(result.data["bump"], "retired");
    }
}
