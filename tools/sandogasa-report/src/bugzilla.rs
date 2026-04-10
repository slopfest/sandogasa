// SPDX-License-Identifier: MPL-2.0

//! Bugzilla activity reporting — review requests, package reviews,
//! CVE fixes, and general bug activity.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use sandogasa_bugzilla::BzClient;

/// Bugzilla activity report.
#[derive(Debug, Serialize)]
pub struct BugzillaReport {
    /// Review requests filed by the user.
    pub reviews_created: Vec<BugEntry>,
    /// Reviews completed by the user (fedora-review+ flag set).
    pub reviews_done: Vec<BugEntry>,
    /// CVE / security bugs the user is involved in.
    pub cve_bugs: Vec<BugEntry>,
    /// Other bugs filed by the user.
    pub other_bugs_filed: Vec<BugEntry>,
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
            // Prefer rhbzemail (Red Hat Bugzilla email) if set.
            if let Some(ref bz_email) = fas_user.rhbzemail {
                if verbose {
                    eprintln!("[bugzilla] FASJSON rhbzemail for {user}: {bz_email}");
                }
                return Ok(bz_email.clone());
            }
            // Fall back to first email in the list.
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

/// Run Bugzilla activity reporting.
pub async fn bugzilla_report(
    email: &str,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BugzillaReport, String> {
    let bz = BzClient::new("https://bugzilla.redhat.com");

    let since_str = since.format("%Y-%m-%d").to_string();
    let until_str = until
        .succ_opt()
        .unwrap_or(until)
        .format("%Y-%m-%d")
        .to_string();

    // Reviews created by the user.
    if verbose {
        eprintln!("[bugzilla] searching review requests created by {email}");
    }
    let reviews_created = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &creator={email}\
                 &creation_time={since_str}\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // Reviews done by the user (assigned_to on Package Review bugs
    // that were closed during the period).
    if verbose {
        eprintln!("[bugzilla] searching reviews completed by {email}");
    }
    let reviews_done = bz
        .search(
            &format!(
                "product=Fedora&component=Package Review\
                 &assigned_to={email}\
                 &chfield=bug_status&chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // CVE / security bugs where user is creator or assignee.
    if verbose {
        eprintln!("[bugzilla] searching CVE/security bugs for {email}");
    }
    let cve_bugs = bz
        .search(
            &format!(
                "product=Fedora&product=Fedora EPEL\
                 &keywords=Security&keywords_type=anywords\
                 &creator={email}\
                 &chfieldfrom={since_str}&chfieldto={until_str}"
            ),
            0,
        )
        .await
        .map_err(|e| format!("Bugzilla search failed: {e}"))?;

    // Other bugs filed by the user (not Package Review, not Security).
    if verbose {
        eprintln!("[bugzilla] searching other bugs filed by {email}");
    }
    let other_filed = bz
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

    // Filter out Package Review and Security bugs from "other".
    let other_bugs_filed: Vec<BugEntry> = other_filed
        .iter()
        .filter(|b| {
            !b.component.iter().any(|c| c == "Package Review")
                && !b.keywords.iter().any(|k| k == "Security")
        })
        .map(BugEntry::from)
        .collect();

    Ok(BugzillaReport {
        reviews_created: reviews_created.iter().map(BugEntry::from).collect(),
        reviews_done: reviews_done.iter().map(BugEntry::from).collect(),
        cve_bugs: cve_bugs.iter().map(BugEntry::from).collect(),
        other_bugs_filed,
    })
}

/// Format the Bugzilla report as Markdown.
pub fn format_markdown(report: &BugzillaReport, detailed: bool) -> String {
    let mut out = String::new();

    out.push_str("## Bugzilla\n\n");

    out.push_str(&format!(
        "- **{}** review request(s) created\n",
        report.reviews_created.len()
    ));
    out.push_str(&format!(
        "- **{}** review(s) completed\n",
        report.reviews_done.len()
    ));
    out.push_str(&format!(
        "- **{}** CVE/security bug(s)\n",
        report.cve_bugs.len()
    ));
    out.push_str(&format!(
        "- **{}** other bug(s) filed\n",
        report.other_bugs_filed.len()
    ));
    out.push('\n');

    if !detailed {
        return out;
    }

    let format_bugs = |out: &mut String, heading: &str, bugs: &[BugEntry]| {
        if bugs.is_empty() {
            return;
        }
        out.push_str(&format!("### {heading}\n\n"));
        for b in bugs {
            let status = if b.resolution.is_empty() {
                b.status.clone()
            } else {
                format!("{} {}", b.status, b.resolution)
            };
            out.push_str(&format!(
                "- [#{}](https://bugzilla.redhat.com/show_bug.cgi?id={}) {} ({})\n",
                b.id, b.id, b.summary, status
            ));
        }
        out.push('\n');
    };

    format_bugs(&mut out, "Review requests created", &report.reviews_created);
    format_bugs(&mut out, "Reviews completed", &report.reviews_done);
    format_bugs(&mut out, "CVE / Security bugs", &report.cve_bugs);
    format_bugs(&mut out, "Other bugs filed", &report.other_bugs_filed);

    out
}
