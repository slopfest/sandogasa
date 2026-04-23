// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `file-issue` subcommand.
//!
//! Given a CentOS Stream MR URL, file a tracking issue in the
//! corresponding `CentOS/proposed_updates/rpms/<pkg>` project on
//! gitlab.com. The issue body is a standardized markdown block
//! that `status` and `sync-issues` can parse back later.

use std::process::ExitCode;

use crate::gitlab;
use crate::jira;

/// GitLab group where tracking issues are filed.
const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";

/// Label applied to tracking issues so `sync-issues` and
/// `status` can identify them.
const TRACKING_LABEL: &str = "cpu-sig-tracker";

/// Issue-type labels already defined in the proposed_updates
/// GitLab group. Passed through `--type`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum IssueType {
    Enhancement,
    Bugfix,
    #[value(name = "arch-enablement")]
    ArchEnablement,
    Security,
}

impl IssueType {
    fn label(self) -> &'static str {
        match self {
            IssueType::Enhancement => "enhancement",
            IssueType::Bugfix => "bugfix",
            IssueType::ArchEnablement => "arch-enablement",
            IssueType::Security => "security",
        }
    }
}

#[derive(clap::Args)]
pub struct FileIssueArgs {
    /// Full Merge Request URL to track, e.g.
    /// `https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42`.
    pub mr_url: String,

    /// Affected (currently-tagged) NVR. If omitted, left blank in
    /// the issue body; can be filled in later.
    #[arg(long)]
    pub affected: Option<String>,

    /// Expected fix NVR once the MR lands and is built.
    #[arg(long = "expected-fix")]
    pub expected_fix: Option<String>,

    /// Override the auto-extracted JIRA key (e.g. "RHEL-12345").
    /// Useful when the MR description doesn't mention it verbatim.
    #[arg(long)]
    pub jira: Option<String>,

    /// Override the release auto-derived from the MR target
    /// branch (e.g. `c10s`).
    #[arg(long)]
    pub release: Option<String>,

    /// Apply one of the proposed_updates type labels:
    /// enhancement, bugfix, arch-enablement, security.
    #[arg(long = "type", value_enum)]
    pub issue_type: Option<IssueType>,

    /// Free-form context note prepended to the issue body as
    /// a lead paragraph before the structured metadata.
    #[arg(long)]
    pub note: Option<String>,

    /// Print the issue that would be filed and exit without
    /// making any GitLab API calls.
    #[arg(long)]
    pub dry_run: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

pub fn run(args: &FileIssueArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &FileIssueArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (base_url, mr_project, iid) = gitlab::parse_mr_url(&args.mr_url)?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] fetching MR {iid} from {mr_project}");
    }

    let package = package_from_project(&mr_project)
        .ok_or_else(|| format!("could not extract package name from '{mr_project}'"))?;

    let mr_client = gitlab::Client::new(&base_url, &mr_project)?;
    let mr = mr_client.merge_request(iid)?;

    let release = args
        .release
        .clone()
        .unwrap_or_else(|| mr.target_branch.clone());

    let jira_key = args
        .jira
        .clone()
        .or_else(|| extract_jira_key(&mr.title, mr.description.as_deref()));

    let jira_info = match jira_key.as_deref() {
        Some(key) => fetch_jira_info(key, args.verbose),
        None => None,
    };
    let (jira_status, jira_resolution) = match jira_info {
        Some((s, r)) => (Some(s), r),
        None => (None, None),
    };

    let body = format_body(&BodyFields {
        release: &release,
        mr_url: &mr.web_url,
        mr_title: &mr.title,
        mr_state: Some(&mr.state),
        jira_key: jira_key.as_deref(),
        jira_status: jira_status.as_deref(),
        jira_resolution: jira_resolution.as_deref(),
        affected: args.affected.as_deref(),
        expected_fix: args.expected_fix.as_deref(),
        note: args.note.as_deref(),
    });

    let title = format_title(
        package,
        args.affected.as_deref(),
        args.expected_fix.as_deref(),
        &mr.title,
    );

    let labels = build_labels(&release, args.issue_type);

    if args.dry_run {
        println!("Would file in {PROPOSED_UPDATES_GROUP}/{package}:");
        println!("---");
        println!("title: {title}");
        println!("labels: {labels}");
        println!("---");
        println!("{body}");
        return Ok(());
    }

    let tracking_project = format!("{PROPOSED_UPDATES_GROUP}/{package}");
    if args.verbose {
        eprintln!("[cpu-sig-tracker] creating issue in {tracking_project}");
    }
    let tracking_client = gitlab::Client::new(&base_url, &tracking_project)?;
    let issue = tracking_client.create_issue(&title, Some(&body), Some(&labels))?;
    eprintln!("Filed #{} {}", issue.iid, issue.web_url);

    // The issue represents tracking work against an MR that already
    // exists, so the "To do" default isn't accurate — transition to
    // "In progress" immediately.
    if let Err(e) = tracking_client.set_work_item_status(issue.iid, "In progress") {
        eprintln!("warning: could not set status to In progress: {e}");
    }

    Ok(())
}

/// Fields we substitute into the standardized issue body.
struct BodyFields<'a> {
    release: &'a str,
    mr_url: &'a str,
    mr_title: &'a str,
    mr_state: Option<&'a str>,
    jira_key: Option<&'a str>,
    jira_status: Option<&'a str>,
    jira_resolution: Option<&'a str>,
    affected: Option<&'a str>,
    expected_fix: Option<&'a str>,
    note: Option<&'a str>,
}

/// Extract the final path segment of a GitLab project path —
/// by convention the package name for `rpms/<pkg>`-style
/// projects.
fn package_from_project(project: &str) -> Option<&str> {
    project.rsplit('/').next().filter(|s| !s.is_empty())
}

/// Build the comma-separated label list: tracking label +
/// release label (only if the release is a valid `c<N>s`
/// identifier) + type label (if given).
fn build_labels(release: &str, issue_type: Option<IssueType>) -> String {
    let mut labels: Vec<&str> = vec![TRACKING_LABEL];
    if let Some(r) = release_label(release) {
        labels.push(r);
    }
    if let Some(t) = issue_type {
        labels.push(t.label());
    }
    labels.join(",")
}

/// Return `Some(release)` if it looks like a CentOS Stream
/// release identifier (`c9s`, `c10s`, …). Returns `None` for
/// anything else (e.g. `main`, feature branches) so we don't
/// invent bogus project labels.
fn release_label(release: &str) -> Option<&str> {
    let digits = release.strip_prefix('c')?.strip_suffix('s')?;
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        Some(release)
    } else {
        None
    }
}

/// Scan text for the first `RHEL-\d+` occurrence. Avoids a
/// regex dep for a single trivial pattern.
fn extract_jira_key(title: &str, description: Option<&str>) -> Option<String> {
    for text in [Some(title), description].into_iter().flatten() {
        if let Some(key) = scan_rhel_key(text) {
            return Some(key);
        }
    }
    None
}

/// Scan `text` for the first RHEL-\d+ occurrence that isn't
/// embedded inside a longer word. Shared with `status` so
/// legacy manually-filed issues (whose bodies don't match the
/// tool's standardized structure) still resolve a JIRA key.
pub(crate) fn scan_rhel_key(text: &str) -> Option<String> {
    const PREFIX: &str = "RHEL-";
    let mut rest = text;
    while let Some(idx) = rest.find(PREFIX) {
        let after = &rest[idx + PREFIX.len()..];
        // Make sure the char before PREFIX (if any) isn't
        // alphanumeric — avoid matching inside longer words.
        let before_ok = idx == 0
            || !rest[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if before_ok && !digits.is_empty() {
            return Some(format!("{PREFIX}{digits}"));
        }
        rest = &rest[idx + PREFIX.len()..];
    }
    None
}

/// Fetch the JIRA issue's status and resolution for inclusion
/// in the rendered body. Returns `(status, resolution)` where
/// resolution is None when the issue is still open.
fn fetch_jira_info(key: &str, verbose: bool) -> Option<(String, Option<String>)> {
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("warning: could not start tokio runtime for JIRA lookup: {e}");
            return None;
        }
    };
    let client = jira::client();
    match runtime.block_on(client.issue(key)) {
        Ok(Some(issue)) => Some((
            issue.status().to_string(),
            issue.resolution().map(|s| s.to_string()),
        )),
        Ok(None) => {
            eprintln!("warning: JIRA {key} not found or not visible");
            None
        }
        Err(e) => {
            eprintln!("warning: JIRA {key} lookup failed: {e}");
            None
        }
    }
}

fn format_title(
    package: &str,
    affected: Option<&str>,
    expected_fix: Option<&str>,
    mr_title: &str,
) -> String {
    match (affected, expected_fix) {
        (Some(a), Some(e)) => format!("{package}: {a} → {e}"),
        _ => format!("{package}: {mr_title}"),
    }
}

/// `- **MR**: [title](url) — state` (state suffix omitted when
/// unknown).
fn format_mr_line(mr_url: &str, mr_title: &str, mr_state: Option<&str>) -> String {
    match mr_state {
        Some(state) => format!("- **MR**: [{mr_title}]({mr_url}) — {state}"),
        None => format!("- **MR**: [{mr_title}]({mr_url})"),
    }
}

/// `- **JIRA**: [KEY](url) — status (resolution)` with
/// graceful degradation when fields are missing.
fn format_jira_line(
    jira_key: Option<&str>,
    jira_status: Option<&str>,
    jira_resolution: Option<&str>,
) -> String {
    let Some(key) = jira_key else {
        return "- **JIRA**: _(not found in MR; set with `--jira`)_".to_string();
    };
    let url = format!("https://issues.redhat.com/browse/{key}");
    let suffix = match (jira_status, jira_resolution) {
        (Some(s), Some(r)) => format!(" — {s} ({r})"),
        (Some(s), None) => format!(" — {s}"),
        (None, _) => String::new(),
    };
    format!("- **JIRA**: [{key}]({url}){suffix}")
}

fn format_body(f: &BodyFields<'_>) -> String {
    let affected = f.affected.unwrap_or("_(unknown)_");
    let expected_fix = f.expected_fix.unwrap_or("_(unknown)_");
    let mr_line = format_mr_line(f.mr_url, f.mr_title, f.mr_state);
    let jira_line = format_jira_line(f.jira_key, f.jira_status, f.jira_resolution);

    let metadata = format!(
        "{mr_line}\n\
         {jira_line}\n\
         - **Release**: {release}\n\
         - **Affected build**: {affected}\n\
         - **Expected fix**: {expected_fix}\n",
        release = f.release,
    );

    match f.note {
        Some(note) if !note.trim().is_empty() => format!("{}\n\n{metadata}", note.trim()),
        _ => metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_from_project_basic() {
        assert_eq!(
            package_from_project("redhat/centos-stream/rpms/xz"),
            Some("xz")
        );
    }

    #[test]
    fn package_from_project_trailing_slash_stripped() {
        // parse_mr_url already strips trailing slashes; no need to
        // handle here. Empty final segment returns None.
        assert_eq!(package_from_project("redhat/centos-stream/rpms/"), None);
    }

    #[test]
    fn package_from_project_flat() {
        assert_eq!(package_from_project("xz"), Some("xz"));
    }

    #[test]
    fn scan_rhel_key_in_title() {
        assert_eq!(
            extract_jira_key("Fix RHEL-12345 backport", None),
            Some("RHEL-12345".to_string())
        );
    }

    #[test]
    fn scan_rhel_key_in_description_when_title_empty() {
        assert_eq!(
            extract_jira_key("no key here", Some("see RHEL-42 for details")),
            Some("RHEL-42".to_string())
        );
    }

    #[test]
    fn scan_rhel_key_title_wins_over_description() {
        assert_eq!(
            extract_jira_key("RHEL-1 summary", Some("actually RHEL-999")),
            Some("RHEL-1".to_string())
        );
    }

    #[test]
    fn scan_rhel_key_none_when_absent() {
        assert_eq!(extract_jira_key("no JIRA here", Some("nor here")), None);
    }

    #[test]
    fn scan_rhel_key_ignores_embedded_in_word() {
        // "NOTRHEL-9" should not match — previous char is alnum.
        assert_eq!(extract_jira_key("NOTRHEL-9 thing", None), None);
    }

    #[test]
    fn scan_rhel_key_requires_digits() {
        assert_eq!(extract_jira_key("bare RHEL- prefix", None), None);
    }

    #[test]
    fn scan_rhel_key_handles_url_context() {
        assert_eq!(
            extract_jira_key("", Some("https://issues.redhat.com/browse/RHEL-7 please"),),
            Some("RHEL-7".to_string())
        );
    }

    #[test]
    fn format_title_with_nvrs() {
        assert_eq!(
            format_title("xz", Some("xz-5.4-1"), Some("xz-5.6-1"), "whatever"),
            "xz: xz-5.4-1 → xz-5.6-1"
        );
    }

    #[test]
    fn format_title_without_nvrs_uses_mr_title() {
        assert_eq!(
            format_title("xz", None, None, "Fix CVE-2026-0001"),
            "xz: Fix CVE-2026-0001"
        );
    }

    #[test]
    fn format_title_missing_one_nvr_uses_mr_title() {
        assert_eq!(
            format_title("xz", Some("xz-5.4-1"), None, "MR subject"),
            "xz: MR subject"
        );
    }

    #[test]
    fn format_body_full() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42",
            mr_title: "Fix CVE-2026-0001",
            mr_state: Some("opened"),
            jira_key: Some("RHEL-12345"),
            jira_status: Some("In Progress"),
            jira_resolution: None,
            affected: Some("xz-5.4-1.el10"),
            expected_fix: Some("xz-5.6-1.el10"),
            note: None,
        });
        assert!(
            !body.contains("##"),
            "body should not contain heading: {body}"
        );
        assert!(body.contains(
            "- **MR**: [Fix CVE-2026-0001](https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42) — opened"
        ));
        assert!(body.contains(
            "- **JIRA**: [RHEL-12345](https://issues.redhat.com/browse/RHEL-12345) — In Progress"
        ));
        assert!(body.contains("- **Release**: c10s"));
        assert!(body.contains("- **Affected build**: xz-5.4-1.el10"));
        assert!(body.contains("- **Expected fix**: xz-5.6-1.el10"));
        // Separate Status line is no longer emitted — state
        // lives on the MR line, JIRA status on the JIRA line.
        assert!(
            !body.contains("- **Status**:"),
            "body should not contain a standalone Status line: {body}",
        );
    }

    #[test]
    fn format_body_without_jira() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            mr_state: None,
            jira_key: None,
            jira_status: None,
            jira_resolution: None,
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **Affected build**: _(unknown)_"));
        assert!(body.contains("- **Expected fix**: _(unknown)_"));
        assert!(body.contains("- **JIRA**: _(not found in MR; set with `--jira`)_"));
        // MR state suffix is omitted when unknown.
        assert!(body.contains("- **MR**: [t](https://example/mr)\n"));
    }

    #[test]
    fn format_body_with_resolved_jira_shows_resolution() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            mr_state: Some("merged"),
            jira_key: Some("RHEL-1"),
            jira_status: Some("Closed"),
            jira_resolution: Some("Done"),
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **MR**: [t](https://example/mr) — merged"));
        assert!(body.contains(
            "- **JIRA**: [RHEL-1](https://issues.redhat.com/browse/RHEL-1) — Closed (Done)"
        ));
    }

    #[test]
    fn release_label_accepts_c10s() {
        assert_eq!(release_label("c10s"), Some("c10s"));
    }

    #[test]
    fn release_label_accepts_c9s() {
        assert_eq!(release_label("c9s"), Some("c9s"));
    }

    #[test]
    fn release_label_rejects_main() {
        assert_eq!(release_label("main"), None);
    }

    #[test]
    fn release_label_rejects_prefix_only() {
        assert_eq!(release_label("cs"), None);
    }

    #[test]
    fn release_label_rejects_non_digit_body() {
        assert_eq!(release_label("cfoos"), None);
    }

    #[test]
    fn build_labels_all_three() {
        assert_eq!(
            build_labels("c10s", Some(IssueType::Security)),
            "cpu-sig-tracker,c10s,security"
        );
    }

    #[test]
    fn build_labels_tracking_only_when_release_invalid_and_no_type() {
        assert_eq!(build_labels("main", None), "cpu-sig-tracker");
    }

    #[test]
    fn build_labels_skips_invalid_release_but_keeps_type() {
        assert_eq!(
            build_labels("main", Some(IssueType::Bugfix)),
            "cpu-sig-tracker,bugfix"
        );
    }

    #[test]
    fn build_labels_release_only() {
        assert_eq!(build_labels("c9s", None), "cpu-sig-tracker,c9s");
    }

    #[test]
    fn issue_type_labels_match_gitlab_project_labels() {
        assert_eq!(IssueType::Enhancement.label(), "enhancement");
        assert_eq!(IssueType::Bugfix.label(), "bugfix");
        assert_eq!(IssueType::ArchEnablement.label(), "arch-enablement");
        assert_eq!(IssueType::Security.label(), "security");
    }

    #[test]
    fn format_body_with_jira_key_but_no_summary() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira_key: Some("RHEL-1"),
            mr_state: None,
            jira_status: None,
            jira_resolution: None,
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **JIRA**: [RHEL-1](https://issues.redhat.com/browse/RHEL-1)\n"));
    }

    #[test]
    fn format_body_prepends_note() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira_key: None,
            mr_state: None,
            jira_status: None,
            jira_resolution: None,
            affected: None,
            expected_fix: None,
            note: Some("Tracking for SIG decision 2026-04-22."),
        });
        assert!(body.starts_with("Tracking for SIG decision 2026-04-22.\n\n- **MR**:"));
    }

    #[test]
    fn format_body_ignores_empty_note() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira_key: None,
            mr_state: None,
            jira_status: None,
            jira_resolution: None,
            affected: None,
            expected_fix: None,
            note: Some("   \n\t"),
        });
        assert!(body.starts_with("- **MR**:"));
    }

    #[test]
    fn format_body_trims_note_whitespace() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira_key: None,
            mr_state: None,
            jira_status: None,
            jira_resolution: None,
            affected: None,
            expected_fix: None,
            note: Some("  leading and trailing  \n\n"),
        });
        assert!(body.starts_with("leading and trailing\n\n- **MR**:"));
    }
}
