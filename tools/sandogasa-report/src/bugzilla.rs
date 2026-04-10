// SPDX-License-Identifier: MPL-2.0

//! Bugzilla activity reporting — review requests, CVE fixes,
//! update requests, branch requests, and general bugs.

use std::collections::{BTreeMap, HashSet};

use chrono::NaiveDate;
use serde::Serialize;

use sandogasa_bugzilla::BzClient;

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

/// Bug kind for classification (non-review bugs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BugKind {
    Review,
    Security,
    Update,
    Branch,
    Ftbfs,
    Fti,
    Other,
}

/// Tracker bug IDs for FTBFS and FTI classification.
struct TrackerIds {
    ftbfs: HashSet<u64>,
    fti: HashSet<u64>,
}

/// Classify a bug. Returns `Review` for Package Review bugs
/// (handled separately by the caller).
fn classify(bug: &sandogasa_bugzilla::models::Bug, trackers: &TrackerIds) -> BugKind {
    if bug.component.iter().any(|c| c == "Package Review") {
        return BugKind::Review;
    }

    // FTBFS / FTI: check if the bug blocks a known tracker.
    if bug.blocks.iter().any(|id| trackers.ftbfs.contains(id)) {
        return BugKind::Ftbfs;
    }
    if bug.blocks.iter().any(|id| trackers.fti.contains(id)) {
        return BugKind::Fti;
    }

    // Security: summary starts with CVE- or SecurityTracking keyword.
    if bug.summary.starts_with("CVE-")
        || bug
            .keywords
            .iter()
            .any(|k| k == "SecurityTracking" || k == "Security")
    {
        return BugKind::Security;
    }

    // Update request: FutureFeature keyword + "is available" summary.
    if bug.keywords.iter().any(|k| k == "FutureFeature") {
        let component = bug.component.first().map(|s| s.as_str()).unwrap_or("");
        if !component.is_empty()
            && bug.summary.starts_with(component)
            && bug.summary.contains("is available")
        {
            return BugKind::Update;
        }
    }

    // Branch request.
    if bug.summary.to_lowercase().contains("branch") {
        return BugKind::Branch;
    }

    BugKind::Other
}

/// Resolve the Bugzilla email for a FAS user.
///
/// Checks (in order):
/// 1. Config file `[users]` mapping
/// 2. FASJSON `rhbzemail` field (requires Kerberos)
/// 3. First email from FASJSON `emails` list
pub fn resolve_email(
    user: &str,
    users: &BTreeMap<String, String>,
    verbose: bool,
) -> Result<String, String> {
    if let Some(email) = users.get(user) {
        if verbose {
            eprintln!("[bugzilla] using configured email for {user}: {email}");
        }
        return Ok(email.clone());
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
                 Add the mapping to [users] in the config file."
            ))
        }
        Err(e) => Err(format!(
            "could not look up {user} via FASJSON: {e}. \
             Add the mapping to [users] in the config file."
        )),
    }
}

/// Look up FTBFS and FTI tracker bug IDs for specified Fedora versions.
async fn lookup_trackers(bz: &BzClient, versions: &[u32], verbose: bool) -> TrackerIds {
    let mut ftbfs = HashSet::new();
    let mut fti = HashSet::new();

    if versions.is_empty() {
        return TrackerIds { ftbfs, fti };
    }

    // Build alias queries for the configured Fedora releases
    // plus the permanent Rawhide trackers.
    let mut aliases = vec![
        "RAWHIDEFTBFS".to_string(),
        "RAWHIDEFailsToInstall".to_string(),
    ];
    for ver in versions {
        aliases.push(format!("F{ver}FTBFS"));
        aliases.push(format!("F{ver}FailsToInstall"));
    }

    if verbose {
        eprintln!("[bugzilla] looking up FTBFS/FTI tracker bugs");
    }

    // Batch lookup by alias.
    let alias_params: Vec<String> = aliases.iter().map(|a| format!("alias={a}")).collect();
    let query = alias_params.join("&");
    if let Ok(bugs) = bz.search(&query, 0).await {
        for bug in &bugs {
            for alias in &bug.alias {
                if alias.ends_with("FTBFS") {
                    ftbfs.insert(bug.id);
                } else if alias.ends_with("FailsToInstall") {
                    fti.insert(bug.id);
                }
            }
        }
    }

    if verbose {
        eprintln!(
            "[bugzilla] found {} FTBFS and {} FTI tracker(s)",
            ftbfs.len(),
            fti.len()
        );
    }

    TrackerIds { ftbfs, fti }
}

/// Run Bugzilla activity reporting.
pub async fn bugzilla_report(
    email: &str,
    fedora_versions: &[u32],
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BugzillaReport, String> {
    let bz = BzClient::new("https://bugzilla.redhat.com");

    // Look up FTBFS and FTI tracker bug IDs.
    let trackers = lookup_trackers(&bz, fedora_versions, verbose).await;

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
