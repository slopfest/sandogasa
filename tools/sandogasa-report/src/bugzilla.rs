// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla activity reporting — review requests, CVE fixes,
//! update requests, branch requests, and general bugs.

use std::collections::HashSet;

use chrono::NaiveDate;
use sandogasa_bugclass::BugKind;
use sandogasa_bugclass::bugzilla::{classify, lookup_trackers};
use sandogasa_bugzilla::BzClient;
use serde::Serialize;

/// Bugzilla activity report.
#[derive(Debug, Serialize)]
pub struct BugzillaReport {
    /// Package reviews section.
    pub reviews: ReviewSection,
    /// Security / CVE bugs.
    pub security: BugCategory,
    /// Package update requests.
    pub updates: BugCategory,
    /// Branch requests.
    pub branches: BugCategory,
    /// FTBFS (Fails To Build From Source) bugs.
    pub ftbfs: BugCategory,
    /// FTI (Fails To Install) bugs.
    pub fti: BugCategory,
    /// Other bugs.
    pub other: BugCategory,
}

/// Package review activity.
#[derive(Debug, Default, Serialize)]
pub struct ReviewSection {
    /// Review requests submitted by the user.
    pub submitted: Vec<BugEntry>,
    /// Subset of submitted that reached CLOSED/RELEASE_PENDING in period.
    pub completed: Vec<BugEntry>,
    /// Reviews done for others (assigned to user).
    pub done_for_others: Vec<BugEntry>,
    /// Subset of done_for_others that reached CLOSED/RELEASE_PENDING.
    pub done_completed: Vec<BugEntry>,
}

/// A bug category with filed/closed breakdown.
#[derive(Debug, Default, Serialize)]
pub struct BugCategory {
    /// Bugs filed during the period.
    pub filed: Vec<BugEntry>,
    /// Bugs closed during the period.
    pub closed: Vec<BugEntry>,
}

impl BugCategory {
    fn is_empty(&self) -> bool {
        self.filed.is_empty() && self.closed.is_empty()
    }
}

impl BugzillaReport {
    /// Get the category for a non-review bug kind.
    /// Returns None for Review (handled separately).
    fn category_mut(&mut self, kind: BugKind) -> Option<&mut BugCategory> {
        match kind {
            BugKind::Review => None,
            BugKind::Security => Some(&mut self.security),
            BugKind::Update => Some(&mut self.updates),
            BugKind::Branch => Some(&mut self.branches),
            BugKind::Ftbfs => Some(&mut self.ftbfs),
            BugKind::Fti => Some(&mut self.fti),
            BugKind::Other => Some(&mut self.other),
        }
    }
}

/// A single bug entry for the report.
#[derive(Debug, Clone, Serialize)]
pub struct BugEntry {
    pub id: u64,
    pub summary: String,
    pub status: String,
    pub resolution: String,
    pub component: String,
}

impl From<&sandogasa_bugzilla::models::Bug> for BugEntry {
    fn from(bug: &sandogasa_bugzilla::models::Bug) -> Self {
        BugEntry {
            id: bug.id,
            summary: bug.summary.clone(),
            status: bug.status.clone(),
            resolution: bug.resolution.clone(),
            component: bug.component.first().cloned().unwrap_or_default(),
        }
    }
}

/// Resolve the Bugzilla email for a FAS user.
///
/// Checks (in order):
/// 1. The profile's `bugzilla_email` override, if set
/// 2. FASJSON `rhbzemail` field (requires Kerberos)
/// 3. First email from FASJSON `emails` list
pub fn resolve_email(
    user: &str,
    profile_email: Option<&str>,
    verbose: bool,
) -> Result<String, String> {
    if let Some(email) = profile_email {
        if verbose {
            eprintln!("[bugzilla] using configured email for {user}: {email}");
        }
        return Ok(email.to_string());
    }

    if verbose {
        eprintln!("[bugzilla] looking up email for {user} via FASJSON");
    }
    let client = sandogasa_fasjson::FasjsonClient::new();
    match client.user(user) {
        Ok(fas_user) => {
            if let Some(ref bz_email) = fas_user.rhbzemail {
                if verbose {
                    eprintln!("[bugzilla] FASJSON rhbzemail for {user}: {bz_email}");
                }
                return Ok(bz_email.clone());
            }
            if let Some(email) = fas_user.emails.first() {
                if verbose {
                    eprintln!("[bugzilla] FASJSON email for {user}: {email}");
                }
                return Ok(email.clone());
            }
            Err(format!(
                "FASJSON returned no emails for {user}. \
                 Set `bugzilla_email` on the user profile in the config."
            ))
        }
        Err(e) => Err(format!(
            "could not look up {user} via FASJSON: {e}. \
             Set `bugzilla_email` on the user profile in the config."
        )),
    }
}

/// Run Bugzilla activity reporting.
pub async fn bugzilla_report(
    email: &str,
    fedora_versions: &[u32],
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BugzillaReport, String> {
    bugzilla_report_with_client(
        &BzClient::new("https://bugzilla.redhat.com"),
        email,
        fedora_versions,
        since,
        until,
        verbose,
    )
    .await
}

async fn bugzilla_report_with_client(
    bz: &BzClient,
    email: &str,
    fedora_versions: &[u32],
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BugzillaReport, String> {
    // Look up FTBFS and FTI tracker bug IDs.
    let trackers = lookup_trackers(bz, fedora_versions, verbose).await;

    let since_str = since.format("%Y-%m-%d").to_string();
    let until_str = until
        .succ_opt()
        .unwrap_or(until)
        .format("%Y-%m-%d")
        .to_string();

    // Query 1: Bugs filed by the user in the period.
    if verbose {
        eprintln!("[bugzilla] searching bugs filed by {email}");
    }
    let bugs_filed = bz
        .search(
            &format!(
                "product=Fedora&product=Fedora EPEL\
                 &creator={email}\
                 &creation_time={since_str}\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // Query 2: Bugs assigned to the user, closed during the period.
    if verbose {
        eprintln!("[bugzilla] searching bugs closed (assigned to {email})");
    }
    let bugs_closed = bz
        .search(
            &format!(
                "product=Fedora&product=Fedora EPEL\
                 &assigned_to={email}\
                 &bug_status=CLOSED\
                 &chfield=bug_status&chfieldvalue=CLOSED\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // Query 3: Package Review bugs assigned to user (reviews done)
    // that had activity in the period.
    if verbose {
        eprintln!("[bugzilla] searching reviews assigned to {email}");
    }
    let reviews_assigned = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &assigned_to={email}\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // Query 4: Review requests filed by user that reached
    // CLOSED or RELEASE_PENDING during the period.
    if verbose {
        eprintln!("[bugzilla] searching completed review requests by {email}");
    }
    let reviews_completed_closed = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &creator={email}\
                 &chfield=bug_status&chfieldvalue=CLOSED\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;
    let reviews_completed_pending = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &creator={email}\
                 &chfield=bug_status&chfieldvalue=RELEASE_PENDING\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    let mut reviews_completed_ids: HashSet<u64> = HashSet::new();
    for bug in reviews_completed_closed
        .iter()
        .chain(reviews_completed_pending.iter())
    {
        reviews_completed_ids.insert(bug.id);
    }

    // Query 5: Reviews done for others that reached
    // CLOSED or RELEASE_PENDING during the period.
    if verbose {
        eprintln!("[bugzilla] searching reviews done for others by {email}");
    }
    let reviews_done_closed = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &assigned_to={email}\
                 &chfield=bug_status&chfieldvalue=CLOSED\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;
    let reviews_done_pending = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &assigned_to={email}\
                 &chfield=bug_status&chfieldvalue=RELEASE_PENDING\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    let mut reviews_done_completed_ids: HashSet<u64> = HashSet::new();
    for bug in reviews_done_closed
        .iter()
        .chain(reviews_done_pending.iter())
    {
        reviews_done_completed_ids.insert(bug.id);
    }

    // Build the report.
    let mut report = BugzillaReport {
        reviews: ReviewSection::default(),
        security: BugCategory::default(),
        updates: BugCategory::default(),
        branches: BugCategory::default(),
        ftbfs: BugCategory::default(),
        fti: BugCategory::default(),
        other: BugCategory::default(),
    };

    // Reviews: submitted by user (from bugs_filed, component=Package Review).
    // Completed = status changed to CLOSED or RELEASE_PENDING during period.
    let mut seen: HashSet<u64> = HashSet::new();
    for bug in &bugs_filed {
        if !seen.insert(bug.id) {
            continue;
        }
        if bug.component.iter().any(|c| c == "Package Review") {
            let entry = BugEntry::from(bug);
            if reviews_completed_ids.contains(&bug.id) {
                report.reviews.completed.push(entry.clone());
            }
            report.reviews.submitted.push(entry);
        }
    }

    // Reviews done for others (assigned to user).
    for bug in &reviews_assigned {
        if seen.insert(bug.id) {
            let entry = BugEntry::from(bug);
            if reviews_done_completed_ids.contains(&bug.id) {
                report.reviews.done_completed.push(entry.clone());
            }
            report.reviews.done_for_others.push(entry);
        }
    }

    // Non-review filed bugs.
    for bug in &bugs_filed {
        let kind = classify(bug, &trackers);
        if let Some(cat) = report.category_mut(kind) {
            cat.filed.push(BugEntry::from(bug));
        }
    }

    // Closed bugs (assigned to user, non-review).
    seen.clear();
    for bug in &bugs_closed {
        if !seen.insert(bug.id) {
            continue;
        }
        let kind = classify(bug, &trackers);
        if let Some(cat) = report.category_mut(kind) {
            cat.closed.push(BugEntry::from(bug));
        }
    }

    Ok(report)
}

/// Format the Bugzilla report as Markdown.
pub fn format_markdown(report: &BugzillaReport, detailed: bool) -> String {
    let mut out = String::new();

    out.push_str("## Bugzilla\n\n");

    // Reviews summary.
    out.push_str("### Package reviews\n\n");
    out.push_str(&format!(
        "- **{}** review request(s) submitted",
        report.reviews.submitted.len()
    ));
    if !report.reviews.completed.is_empty() {
        out.push_str(&format!(" ({} completed)", report.reviews.completed.len()));
    }
    out.push('\n');
    out.push_str(&format!(
        "- **{}** review(s) done for others",
        report.reviews.done_for_others.len()
    ));
    if !report.reviews.done_completed.is_empty() {
        out.push_str(&format!(
            " ({} completed)",
            report.reviews.done_completed.len()
        ));
    }
    out.push('\n');
    out.push('\n');

    // Other categories table.
    if !report.security.is_empty()
        || !report.updates.is_empty()
        || !report.branches.is_empty()
        || !report.ftbfs.is_empty()
        || !report.fti.is_empty()
        || !report.other.is_empty()
    {
        out.push_str("| Category | Filed | Closed |\n");
        out.push_str("|----------|------:|-------:|\n");

        let row = |out: &mut String, label: &str, cat: &BugCategory| {
            if !cat.is_empty() {
                out.push_str(&format!(
                    "| {} | {} | {} |\n",
                    label,
                    cat.filed.len(),
                    cat.closed.len()
                ));
            }
        };

        row(&mut out, "Security / CVE", &report.security);
        row(&mut out, "Update requests", &report.updates);
        row(&mut out, "Branch requests", &report.branches);
        row(&mut out, "FTBFS", &report.ftbfs);
        row(&mut out, "Fails to install", &report.fti);
        row(&mut out, "Other", &report.other);
        out.push('\n');
    }

    if !detailed {
        return out;
    }

    let format_bugs = |out: &mut String, bugs: &[BugEntry]| {
        for b in bugs {
            let status = if b.resolution.is_empty() {
                b.status.clone()
            } else {
                format!("{} {}", b.status, b.resolution)
            };
            out.push_str(&format!(
                "- [#{}](https://bugzilla.redhat.com/show_bug.cgi?id={}) \
                 {} ({})\n",
                b.id, b.id, b.summary, status
            ));
        }
        out.push('\n');
    };

    // Detailed review lists.
    let completed_ids: HashSet<u64> = report.reviews.completed.iter().map(|b| b.id).collect();

    if !report.reviews.submitted.is_empty() {
        out.push_str("#### Review requests submitted\n\n");
        for b in &report.reviews.submitted {
            let status = if b.resolution.is_empty() {
                b.status.clone()
            } else {
                format!("{} {}", b.status, b.resolution)
            };
            out.push_str(&format!(
                "- [#{}](https://bugzilla.redhat.com/show_bug.cgi?id={}) \
                 {} ({})\n",
                b.id, b.id, b.summary, status
            ));
            if completed_ids.contains(&b.id) {
                out.push_str("  - Completed\n");
            }
        }
        out.push('\n');
    }
    if !report.reviews.done_for_others.is_empty() {
        let done_completed_ids: HashSet<u64> =
            report.reviews.done_completed.iter().map(|b| b.id).collect();
        out.push_str("#### Reviews done for others\n\n");
        for b in &report.reviews.done_for_others {
            let status = if b.resolution.is_empty() {
                b.status.clone()
            } else {
                format!("{} {}", b.status, b.resolution)
            };
            out.push_str(&format!(
                "- [#{}](https://bugzilla.redhat.com/show_bug.cgi?id={}) \
                 {} ({})\n",
                b.id, b.id, b.summary, status
            ));
            if done_completed_ids.contains(&b.id) {
                out.push_str("  - Completed\n");
            }
        }
        out.push('\n');
    }

    // Detailed category lists.
    let format_category = |out: &mut String, heading: &str, cat: &BugCategory| {
        if cat.is_empty() {
            return;
        }
        out.push_str(&format!("### {heading}\n\n"));
        if !cat.filed.is_empty() {
            out.push_str("**Filed:**\n\n");
            format_bugs(out, &cat.filed);
        }
        if !cat.closed.is_empty() {
            out.push_str("**Closed:**\n\n");
            format_bugs(out, &cat.closed);
        }
    };

    format_category(&mut out, "Security / CVE", &report.security);
    format_category(&mut out, "Update requests", &report.updates);
    format_category(&mut out, "Branch requests", &report.branches);
    format_category(&mut out, "FTBFS", &report.ftbfs);
    format_category(&mut out, "Fails to install", &report.fti);
    format_category(&mut out, "Other bugs", &report.other);

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn format_summary_shows_table() {
        let report = BugzillaReport {
            reviews: ReviewSection {
                submitted: vec![BugEntry {
                    id: 1,
                    summary: "Review Request: rust-foo".to_string(),
                    status: "NEW".to_string(),
                    resolution: String::new(),
                    component: "Package Review".to_string(),
                }],
                completed: vec![],
                done_for_others: vec![],
                done_completed: vec![],
            },
            security: BugCategory::default(),
            updates: BugCategory::default(),
            branches: BugCategory::default(),
            ftbfs: BugCategory::default(),
            fti: BugCategory::default(),
            other: BugCategory::default(),
        };
        let md = format_markdown(&report, false);
        assert!(md.contains("**1** review request(s) submitted"));
        assert!(md.contains("## Bugzilla"));
    }

    #[test]
    fn format_detailed_shows_sections() {
        let report = BugzillaReport {
            reviews: ReviewSection {
                submitted: vec![BugEntry {
                    id: 100,
                    summary: "Review Request: rust-foo - Foo library".to_string(),
                    status: "CLOSED".to_string(),
                    resolution: "NEXTRELEASE".to_string(),
                    component: "Package Review".to_string(),
                }],
                completed: vec![BugEntry {
                    id: 100,
                    summary: "Review Request: rust-foo - Foo library".to_string(),
                    status: "CLOSED".to_string(),
                    resolution: "NEXTRELEASE".to_string(),
                    component: "Package Review".to_string(),
                }],
                done_for_others: vec![],
                done_completed: vec![],
            },
            security: BugCategory {
                filed: vec![],
                closed: vec![BugEntry {
                    id: 200,
                    summary: "CVE-2026-1234 foo: overflow".to_string(),
                    status: "CLOSED".to_string(),
                    resolution: "ERRATA".to_string(),
                    component: "foo".to_string(),
                }],
            },
            updates: BugCategory::default(),
            branches: BugCategory::default(),
            ftbfs: BugCategory {
                filed: vec![],
                closed: vec![BugEntry {
                    id: 300,
                    summary: "bar: FTBFS in F45".to_string(),
                    status: "CLOSED".to_string(),
                    resolution: "RAWHIDE".to_string(),
                    component: "bar".to_string(),
                }],
            },
            fti: BugCategory::default(),
            other: BugCategory::default(),
        };
        let md = format_markdown(&report, true);
        assert!(md.contains("(1 completed)"));
        assert!(md.contains("Completed"));
        assert!(md.contains("#### Review requests submitted"));
        assert!(md.contains("### Security / CVE"));
        assert!(md.contains("CVE-2026-1234"));
        assert!(md.contains("### FTBFS"));
        assert!(md.contains("| Security / CVE | 0 | 1 |"));
        assert!(md.contains("| FTBFS | 0 | 1 |"));
    }

    #[test]
    fn format_table_only_shows_nonempty() {
        let report = BugzillaReport {
            reviews: ReviewSection::default(),
            security: BugCategory::default(),
            updates: BugCategory::default(),
            branches: BugCategory::default(),
            ftbfs: BugCategory::default(),
            fti: BugCategory::default(),
            other: BugCategory::default(),
        };
        let md = format_markdown(&report, false);
        // No table if all categories are empty.
        assert!(!md.contains("| Category |"));
    }

    /// Helper: mock Bugzilla REST API returning a set of bugs for any search.
    fn bug_json(
        id: u64,
        summary: &str,
        component: &str,
        status: &str,
        keywords: &[&str],
        creator: &str,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "summary": summary,
            "status": status,
            "resolution": if status == "CLOSED" { "RAWHIDE" } else { "" },
            "product": "Fedora",
            "component": [component],
            "severity": "unspecified",
            "priority": "unspecified",
            "assigned_to": "test@example.com",
            "creator": creator,
            "creation_time": "2026-02-01T10:00:00Z",
            "last_change_time": "2026-02-15T10:00:00Z",
            "keywords": keywords,
            "alias": [],
            "depends_on": [],
            "blocks": [],
            "see_also": [],
            "cc": [],
            "flags": [],
            "version": ["rawhide"],
            "cf_fixed_in": ""
        })
    }

    #[tokio::test]
    async fn bugzilla_report_classifies_bugs() {
        let server = MockServer::start().await;
        let bz = BzClient::new(&server.uri());

        // All search queries return the same set of bugs for simplicity.
        let bugs = serde_json::json!({
            "bugs": [
                bug_json(1, "Review Request: rust-foo - Foo", "Package Review", "NEW", &[], "test@example.com"),
                bug_json(2, "CVE-2026-1234 bar: overflow", "bar", "CLOSED", &["SecurityTracking"], "other@example.com"),
                bug_json(3, "fish-4.0 is available", "fish", "CLOSED", &["FutureFeature"], "other@example.com"),
                bug_json(4, "Please branch rust-baz for epel10", "rust-baz", "CLOSED", &[], "test@example.com"),
                bug_json(5, "qux crashes on startup", "qux", "NEW", &[], "test@example.com"),
            ],
            "total_matches": 5
        });

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&bugs))
            .mount(&server)
            .await;

        let report = bugzilla_report_with_client(
            &bz,
            "test@example.com",
            &[],
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            false,
        )
        .await
        .unwrap();

        // Review filed by user.
        assert_eq!(report.reviews.submitted.len(), 1);
        assert_eq!(report.reviews.submitted[0].id, 1);

        // CVE closed (assigned to user).
        assert!(!report.security.closed.is_empty());

        // Update request closed.
        assert!(!report.updates.closed.is_empty());

        // Branch request.
        assert!(!report.branches.filed.is_empty() || !report.branches.closed.is_empty());
    }

    #[tokio::test]
    async fn bugzilla_report_with_ftbfs_trackers() {
        let server = MockServer::start().await;
        let bz = BzClient::new(&server.uri());

        // First call: tracker lookup returns one FTBFS tracker.
        // Subsequent calls: return a bug that blocks it.
        let tracker_response = serde_json::json!({
            "bugs": [{
                "id": 999,
                "summary": "RAWHIDE FTBFS Tracker",
                "status": "NEW",
                "resolution": "",
                "product": "Fedora",
                "component": ["Tracking"],
                "severity": "unspecified",
                "priority": "unspecified",
                "assigned_to": "nobody",
                "creator": "admin",
                "creation_time": "2025-01-01T00:00:00Z",
                "last_change_time": "2025-01-01T00:00:00Z",
                "keywords": [],
                "alias": ["RAWHIDEFTBFS"],
                "depends_on": [],
                "blocks": [],
                "see_also": [],
                "cc": [],
                "flags": [],
                "version": [],
                "cf_fixed_in": ""
            }],
            "total_matches": 1
        });

        let ftbfs_bug = serde_json::json!({
            "bugs": [{
                "id": 500,
                "summary": "foo: FTBFS in rawhide",
                "status": "CLOSED",
                "resolution": "RAWHIDE",
                "product": "Fedora",
                "component": ["foo"],
                "severity": "unspecified",
                "priority": "unspecified",
                "assigned_to": "test@example.com",
                "creator": "releng@fedoraproject.org",
                "creation_time": "2026-01-15T00:00:00Z",
                "last_change_time": "2026-02-01T00:00:00Z",
                "keywords": [],
                "alias": [],
                "depends_on": [],
                "blocks": [999],
                "see_also": [],
                "cc": [],
                "flags": [],
                "version": ["rawhide"],
                "cf_fixed_in": ""
            }],
            "total_matches": 1
        });

        // Return both the tracker and the FTBFS bug for all queries.
        // The report function will filter appropriately.
        let combined = serde_json::json!({
            "bugs": [
                tracker_response["bugs"][0],
                ftbfs_bug["bugs"][0]
            ],
            "total_matches": 2
        });

        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_json(&combined))
            .mount(&server)
            .await;

        let report = bugzilla_report_with_client(
            &bz,
            "test@example.com",
            &[],
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            false,
        )
        .await
        .unwrap();

        // The FTBFS bug should be classified as FTBFS.
        assert!(!report.ftbfs.closed.is_empty());
        assert_eq!(report.ftbfs.closed[0].id, 500);
    }
}
