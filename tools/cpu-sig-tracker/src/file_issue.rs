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
use crate::utils::{format_jira_lines, format_mr_line};

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
    /// `gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42`
    /// (with the https:// scheme).
    pub mr_url: String,

    /// Affected (currently-tagged) NVR. If omitted, left blank in
    /// the issue body; can be filled in later.
    #[arg(long, help_heading = "Issue")]
    pub affected: Option<String>,

    /// Expected fix NVR once the MR lands and is built.
    #[arg(long = "expected-fix", help_heading = "Issue")]
    pub expected_fix: Option<String>,

    /// The RHEL issue(s) the MR resolves, overriding those read from
    /// its Resolves: lines (repeatable, or comma-separated)
    #[arg(
        long,
        value_name = "KEY",
        value_delimiter = ',',
        help_heading = "Issue"
    )]
    pub jira: Vec<String>,

    /// Free-form context note prepended to the issue body as
    /// a lead paragraph before the structured metadata.
    #[arg(long, help_heading = "Issue")]
    pub note: Option<String>,

    /// Override the release auto-derived from the MR target
    /// branch (e.g. `c10s`).
    #[arg(long, help_heading = "Issue")]
    pub release: Option<String>,

    /// Apply one of the proposed_updates type labels:
    /// enhancement, bugfix, arch-enablement, security.
    #[arg(long = "type", value_enum, help_heading = "Issue")]
    pub issue_type: Option<IssueType>,

    /// Print the issue that would be filed and exit without
    /// making any GitLab API calls.
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Print progress to stderr.
    #[arg(short, long, help_heading = "Output")]
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

/// How to create the SIG's repo for `package` when it has none yet: a
/// pull mirror of the CentOS Stream repo, with the proposed change's
/// branch pushed to it as `<release>-proposed-<RHEL key>`, from
/// `source_branch` (the merge request's) when known.
pub fn missing_sig_repo_help(
    package: &str,
    release: &str,
    source_branch: Option<&str>,
    jira_key: Option<&str>,
) -> String {
    let group = sandogasa_cbs::Sig::ProposedUpdates.gitlab_group();
    let topic = format!("{release}-proposed-{}", jira_key.unwrap_or("<RHEL key>"));
    format!(
        "the SIG has no repo for {package} yet ({group}/{package}). Create it as a pull \
         mirror of the CentOS Stream repo:\n\
         \x20 1. On GitLab: New project → Import project → Repository by URL, in {group}\n\
         \x20    URL: https://gitlab.com/redhat/centos-stream/rpms/{package}.git\n\
         \x20    name {package}, public, with \"Mirror repository\" ticked; then in Settings →\n\
         \x20    Repository → Mirroring repositories, leave \"Overwrite diverged branches\" off\n\
         \x20 2. Push the change's branch to it, from the dist-git checkout:\n\
         \x20    git push git+ssh://git@gitlab.com/{group}/{package}.git \
         {}:refs/heads/{topic}",
        source_branch.unwrap_or("<branch>")
    )
}

/// Whether the SIG has a repo for `package`, where its tracking issues
/// are filed.
pub fn sig_repo_exists(package: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let path = format!(
        "{}/{package}",
        sandogasa_cbs::Sig::ProposedUpdates.gitlab_group()
    );
    Ok(gitlab::reader(&crate::utils::gitlab_base(), &path)?
        .project()?
        .is_some())
}

pub(crate) fn run_inner(args: &FileIssueArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (_parsed_base, mr_project, iid) = gitlab::parse_mr_url(&args.mr_url)?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] fetching MR {iid} from {mr_project}");
    }

    let package = package_from_project(&mr_project)
        .ok_or_else(|| format!("could not extract package name from '{mr_project}'"))?;

    let mr_client = gitlab::client(&crate::utils::gitlab_base(), &mr_project)?;
    let mr = mr_client.merge_request(iid)?;

    let release = args
        .release
        .clone()
        .unwrap_or_else(|| mr.target_branch.clone());

    let jira_keys = if args.jira.is_empty() {
        extract_jira_keys(&mr.title, mr.description.as_deref())
    } else {
        args.jira.clone()
    };
    // Before Jira is asked anything: whether there is a repo to file
    // in. A dry run says so and still shows the issue.
    if !sig_repo_exists(package)? {
        let help = missing_sig_repo_help(
            package,
            &release,
            Some(&mr.source_branch),
            jira_keys.first().map(String::as_str),
        );
        if !args.dry_run {
            return Err(format!("{help}\nthen run this again.").into());
        }
        eprintln!("warning: {help}\n");
    }
    let group = sandogasa_cbs::Sig::ProposedUpdates.gitlab_group();
    let jira: Vec<(String, Option<String>, Option<String>)> = jira_keys
        .iter()
        .map(|key| match fetch_jira_info(key, args.verbose) {
            Some((s, r)) => (key.clone(), Some(s), r),
            None => (key.clone(), None, None),
        })
        .collect();

    let body = format_body(&BodyFields {
        release: &release,
        mr_url: &mr.web_url,
        mr_title: &mr.title,
        mr_state: Some(&mr.state),
        jira: &jira,
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
        println!("Would file in {group}/{package}:");
        println!("---");
        println!("title: {title}");
        println!("labels: {labels}");
        println!("---");
        println!("{body}");
        return Ok(());
    }

    let tracking_project = format!("{group}/{package}");
    if args.verbose {
        eprintln!("[cpu-sig-tracker] creating issue in {tracking_project}");
    }
    let tracking_client = gitlab::client(&crate::utils::gitlab_base(), &tracking_project)?;
    let issue = tracking_client.create_issue(&title, Some(&body), Some(&labels))?;
    eprintln!("Filed #{} {}", issue.iid, issue.web_url);

    // The issue represents tracking work against an MR that already
    // exists, so the "To do" default isn't accurate — transition to
    // "In progress" immediately.
    if let Err(e) = tracking_client.set_work_item_status(issue.iid, "In progress") {
        eprintln!("warning: could not set status to In progress: {e}");
    }

    // Set the issue start_date to when the Koji build entered
    // the SIG's tag line (approximated by the build's creation
    // time — close enough for the "how long did we need this"
    // retrospective the SIG cares about). Goes via the GraphQL
    // workItemUpdate mutation; REST PUT /issues ignores
    // start_date on work items.
    if let Some(date) = find_build_start_date(package, &release, args.verbose) {
        let formatted = date.format("%Y-%m-%d").to_string();
        if let Err(e) = tracking_client.set_work_item_dates(issue.iid, Some(&formatted), None) {
            eprintln!("warning: could not set start_date: {e}");
        }
    } else if args.verbose {
        eprintln!(
            "[cpu-sig-tracker] no Koji build found for {package} in c{release} \
             -release or -testing; leaving start_date blank",
        );
    }

    Ok(())
}

/// Look up the package's latest build across the release's
/// `-release` and `-testing` Koji tags and return its creation
/// date. `-release` wins when both are populated; `-testing`
/// is the fallback for packages whose first build hasn't been
/// promoted yet.
pub(crate) fn find_build_start_date(
    package: &str,
    release: &str,
    verbose: bool,
) -> Option<chrono::NaiveDate> {
    for tag_fn in [
        crate::dump_inventory::proposed_updates_tag as fn(&str) -> Result<String, String>,
        crate::dump_inventory::proposed_updates_testing_tag as fn(&str) -> Result<String, String>,
    ] {
        let tag = match tag_fn(release) {
            Ok(t) => t,
            Err(_) => continue,
        };
        if verbose {
            eprintln!("[cpu-sig-tracker] checking {tag} for {package}");
        }
        let nvrs = match sandogasa_koji::list_tagged_nvrs(&tag, Some(KOJI_PROFILE)) {
            Ok(v) => v,
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] koji list-tagged {tag} failed: {e}");
                }
                continue;
            }
        };
        let Some(nvr) = nvrs
            .iter()
            .find(|n| sandogasa_koji::parse_nvr_name(n) == Some(package))
        else {
            continue;
        };
        match sandogasa_koji::build_creation_date(nvr, Some(KOJI_PROFILE)) {
            Ok(Some(date)) => return Some(date),
            Ok(None) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] koji buildinfo {nvr} produced no creation time",);
                }
            }
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] koji buildinfo {nvr} failed: {e}");
                }
            }
        }
    }
    None
}

const KOJI_PROFILE: &str = "cbs";

/// Fields we substitute into the standardized issue body.
struct BodyFields<'a> {
    release: &'a str,
    mr_url: &'a str,
    mr_title: &'a str,
    mr_state: Option<&'a str>,
    /// `(key, status, resolution)` of each RHEL issue the MR resolves.
    jira: &'a [(String, Option<String>, Option<String>)],
    affected: Option<&'a str>,
    expected_fix: Option<&'a str>,
    note: Option<&'a str>,
}

/// Extract the final path segment of a GitLab project path —
/// by convention the package name for `rpms/<pkg>`-style
/// projects.
pub(crate) fn package_from_project(project: &str) -> Option<&str> {
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

/// The RHEL issues a merge request resolves: the keys on its
/// `Resolves:` lines, each once, in order, as CentOS Stream's template
/// lists them, one per fix it carries (#86); without any, the first
/// `RHEL-\d+` in its title or description, as a hand-written MR may
/// name its issue anywhere.
pub(crate) fn extract_jira_keys(title: &str, description: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in description.unwrap_or("").lines() {
        if let Some(rest) = line.trim().strip_prefix("Resolves:")
            && let Some(key) = scan_rhel_key(rest)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        keys.extend(
            [Some(title), description]
                .into_iter()
                .flatten()
                .find_map(scan_rhel_key),
        );
    }
    keys
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
    let issue = jira::fetch_or_warn(key, verbose)?;
    Some((
        issue.status().to_string(),
        issue.resolution().map(|s| s.to_string()),
    ))
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

fn format_body(f: &BodyFields<'_>) -> String {
    let affected = f.affected.unwrap_or("_(unknown)_");
    let expected_fix = f.expected_fix.unwrap_or("_(unknown)_");
    let mr_line = format_mr_line(f.mr_url, f.mr_title, f.mr_state);
    let issues: Vec<(&str, Option<&str>, Option<&str>)> = f
        .jira
        .iter()
        .map(|(k, s, r)| (k.as_str(), s.as_deref(), r.as_deref()))
        .collect();
    let jira_line = format_jira_lines(&issues);

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
            extract_jira_keys("Fix RHEL-12345 backport", None),
            ["RHEL-12345"]
        );
    }

    #[test]
    fn scan_rhel_key_in_description_when_title_empty() {
        assert_eq!(
            extract_jira_keys("no key here", Some("see RHEL-42 for details")),
            ["RHEL-42"]
        );
    }

    #[test]
    fn scan_rhel_key_title_wins_over_description() {
        assert_eq!(
            extract_jira_keys("RHEL-1 summary", Some("actually RHEL-999")),
            ["RHEL-1"]
        );
    }

    #[test]
    fn scan_rhel_key_none_when_absent() {
        assert!(extract_jira_keys("no JIRA here", Some("nor here")).is_empty());
    }

    #[test]
    fn scan_rhel_key_ignores_embedded_in_word() {
        // "NOTRHEL-9" should not match — previous char is alnum.
        assert!(extract_jira_keys("NOTRHEL-9 thing", None).is_empty());
    }

    #[test]
    fn scan_rhel_key_requires_digits() {
        assert!(extract_jira_keys("bare RHEL- prefix", None).is_empty());
    }

    #[test]
    fn every_resolves_line_names_an_issue() {
        // CentOS Stream's template lists one per fix the MR carries.
        let description = "## Approved Development Ticket(s)\n\
                           List tickets like \"Resolves: RHEL-76229\".\n\
                           </details>\n\n\
                           Resolves: RHEL-303720\n\
                           Resolves: RHEL-303900\n\
                           Resolves: RHEL-303720\n";
        assert_eq!(
            extract_jira_keys("rsync: two fixes", Some(description)),
            ["RHEL-303720", "RHEL-303900"]
        );
    }

    #[test]
    fn scan_rhel_key_handles_url_context() {
        assert_eq!(
            extract_jira_keys("", Some("https://issues.redhat.com/browse/RHEL-7 please"),),
            ["RHEL-7"]
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
            jira: &[(
                "RHEL-12345".to_string(),
                Some("In Progress".to_string()),
                None,
            )],
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
        let expected = format!(
            "- **JIRA**: [RHEL-12345]({}/browse/RHEL-12345) — In Progress",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
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
            jira: &[],
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
            jira: &[(
                "RHEL-1".to_string(),
                Some("Closed".to_string()),
                Some("Done".to_string()),
            )],
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **MR**: [t](https://example/mr) — merged"));
        let expected = format!(
            "- **JIRA**: [RHEL-1]({}/browse/RHEL-1) — Closed (Done)",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
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
            jira: &[("RHEL-1".to_string(), None, None)],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: None,
        });
        let expected = format!(
            "- **JIRA**: [RHEL-1]({}/browse/RHEL-1)\n",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
    }

    #[test]
    fn format_body_prepends_note() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira: &[],
            mr_state: None,
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
            jira: &[],
            mr_state: None,
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
            jira: &[],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: Some("  leading and trailing  \n\n"),
        });
        assert!(body.starts_with("leading and trailing\n\n- **MR**:"));
    }

    // ---- end-to-end wiremock + fake-koji test ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wiremock_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[serial_test::serial]
    fn file_issue_end_to_end() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // The SIG's repo the issue is filed in exists.
            Mock::given(method("GET"))
                .and(wiremock_path("/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 7,
                    "path_with_namespace": "CentOS/proposed_updates/rpms/xz",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz",
                    "ssh_url_to_repo": "git@gitlab.example:CentOS/proposed_updates/rpms/xz.git",
                    "http_url_to_repo": "https://gitlab.example/CentOS/proposed_updates/rpms/xz.git",
                })))
                .mount(&server)
                .await;
            // MR fetch from the source project.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fxz/merge_requests/42",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 42,
                    "title": "Fix RHEL-1234 in xz",
                    "description": "Linked to RHEL-1234.",
                    "state": "opened",
                    "web_url": "https://gitlab.example/redhat/centos-stream/rpms/xz/-/merge_requests/42",
                    "source_branch": "fix",
                    "target_branch": "c10s",
                })))
                .mount(&server)
                .await;

            // JIRA enrichment.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1234"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1234",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "In Progress" }
                    }
                })))
                .mount(&server)
                .await;

            // create_issue POST on the tracking project.
            Mock::given(method("POST"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues",
                ))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                    "iid": 7,
                    "title": "xz: Fix RHEL-1234 in xz",
                    "description": null,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/7",
                    "assignees": [],
                })))
                .expect(1)
                .mount(&server)
                .await;

            // GraphQL — set_work_item_status (→ In progress) and
            // set_work_item_dates (start_date) both land here.
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "project": {
                            "workItems": {
                                "nodes": [{
                                    "id": "gid://gitlab/WorkItem/7",
                                    "widgets": [{
                                        "type": "STATUS",
                                        "status": { "name": "To do" }
                                    }],
                                    "namespace": {
                                        "workItemTypes": {
                                            "nodes": [{
                                                "name": "Issue",
                                                "widgetDefinitions": [{
                                                    "type": "STATUS",
                                                    "allowedStatuses": [{
                                                        "id": "gid://gitlab/status/1",
                                                        "name": "In progress"
                                                    }]
                                                }]
                                            }]
                                        }
                                    }
                                }]
                            }
                        },
                        "workItemUpdate": { "errors": [] }
                    }
                })))
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                // Koji list for start_date discovery — package
                // is in -release, with a canned NVR.
                (
                    "list-tagged --quiet -- proposed_updates10s-packages-main-release",
                    "xz-5.6.4-1~proposed.el10\n",
                ),
                // buildinfo for the date.
                (
                    "buildinfo -- xz-5.6.4-1~proposed.el10",
                    "BUILD: xz-5.6.4-1~proposed.el10\nCreation time: 2026-04-22 10:11:12\n",
                ),
            ],
        );
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = FileIssueArgs {
            mr_url: "https://gitlab.example/redhat/centos-stream/rpms/xz/-/merge_requests/42"
                .to_string(),
            affected: Some("xz-5.6.2-3.el10".to_string()),
            expected_fix: Some("xz-5.6.4-1~proposed.el10".to_string()),
            jira: vec![],
            release: None,
            issue_type: Some(IssueType::Security),
            note: None,
            dry_run: false,
            verbose: false,
        };
        run_inner(&args).expect("file-issue succeeds");
    }

    /// rsync's first run: no SIG repo yet, so nothing is asked of Jira
    /// or posted, and the error says how to make one.
    #[test]
    #[serial_test::serial]
    fn a_missing_sig_repo_stops_file_issue_before_jira_with_how_to_make_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests/90",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 90,
                    "title": "Fix denial of service via crafted checksums (CVE-2026-119741)",
                    "description": "Resolves: RHEL-303721",
                    "state": "opened",
                    "web_url": "https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/90",
                    "source_branch": "c10s-CVE-2026-119741",
                    "target_branch": "c10s",
                })))
                .mount(&server)
                .await;
            // Anything else (the SIG's project, Jira, the issue POST)
            // must not be reached, bar the project lookup's 404.
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-303721"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        });
        let dir = tempdir().unwrap();
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
        ]);
        let args = FileIssueArgs {
            mr_url: "https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/90"
                .to_string(),
            affected: None,
            expected_fix: None,
            jira: vec![],
            release: None,
            issue_type: None,
            note: None,
            dry_run: false,
            verbose: false,
        };
        let err = run_inner(&args).unwrap_err().to_string();
        assert!(
            err.contains("the SIG has no repo for rsync yet")
                && err.contains(
                    "rpms/rsync.git c10s-CVE-2026-119741:refs/heads/c10s-proposed-RHEL-303721"
                )
                && err.ends_with("then run this again."),
            "{err}"
        );
        runtime.block_on(server.verify());
    }
}
