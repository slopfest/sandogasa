// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `retire` subcommand.
//!
//! Closes a tracking issue classified as `retire-issue` by
//! `status` — i.e. one where JIRA has been resolved and the
//! package is no longer tagged in the proposed_updates
//! `-release` Koji tag, so the tracking issue is just leftover
//! bookkeeping.
//!
//! Safety-first flow: fetch the issue, verify both conditions,
//! prompt the user, leave an audit-trail comment, then close.
//! `--force` skips the condition checks, `--yes` skips the
//! prompt.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use sandogasa_koji::{list_tagged_nvrs, parse_nvr_name};

use crate::dump_inventory::proposed_updates_tag;
use crate::{gitlab, jira};

const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct RetireArgs {
    /// Full tracking issue URL (either `/-/issues/<n>` or
    /// `/-/work_items/<n>` form).
    pub issue_url: String,

    /// Skip the interactive confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,

    /// Skip the retire-issue precondition checks (JIRA
    /// resolved, build untagged). Use when the tool can't
    /// reach JIRA/Koji or when you're sure the conditions hold.
    #[arg(long)]
    pub force: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

pub fn run(args: &RetireArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

pub(crate) fn run_inner(args: &RetireArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (_parsed_base, project_path, iid) = gitlab::parse_issue_url(&args.issue_url)?;
    // parse_issue_url extracts the host from the user-supplied
    // URL, but we route API calls through `gitlab_base()` so
    // tests can override via `CPU_SIG_TRACKER_GITLAB_BASE`.
    // In production both are gitlab.com so there's no
    // visible change.
    if args.verbose {
        eprintln!("[cpu-sig-tracker] fetching issue {project_path}!{iid}");
    }
    let client = gitlab::Client::new(&crate::utils::gitlab_base(), &project_path)?;
    let issue = client.issue(iid)?;

    if issue.state == "closed" {
        eprintln!("issue already closed: {}", issue.web_url);
        return Ok(());
    }

    let body = issue.description.as_deref().unwrap_or("");
    let release = parse_release_from_body(body)
        .ok_or("could not parse `- **Release**:` line from issue body")?;
    let package = gitlab::package_from_issue_url(&issue.web_url)
        .ok_or("could not derive package name from issue URL")?;
    let jira_key = parse_jira_key_from_body(body);

    // Precondition 1: JIRA resolved.
    let jira_check = check_jira_resolved(jira_key.as_deref(), args.verbose);

    // Precondition 2: no pu build tagged.
    let build_check = check_package_untagged(&release, package, args.verbose);

    report_precondition("JIRA resolved", &jira_check.check);
    report_precondition("no pu build tagged", &build_check);

    let preconditions_ok =
        matches!(&jira_check.check, Check::Pass(_)) && matches!(&build_check, Check::Pass(_));
    if !preconditions_ok && !args.force {
        return Err(
            "retire preconditions not met; re-run with --force to override or fix the \
             underlying state (e.g. run `untag` first if the build is still tagged)"
                .into(),
        );
    }

    println!();
    println!("Will close {}", issue.web_url);
    println!("  title:   {}", issue.title);
    println!("  package: {package}");
    println!("  release: {release}");
    let start_date = derive_start_date(package, &release, &issue, args.verbose);
    if let Some((date, source)) = &start_date {
        println!("  start_date: {date} (from {source})");
    }
    if let Some(date) = jira_check.resolution_date {
        println!("  due_date: {date} (from JIRA resolutiondate)");
    }
    if !args.yes && !confirm("Proceed?")? {
        eprintln!("aborted.");
        return Ok(());
    }

    let note = compose_audit_note(
        jira_key.as_deref(),
        &jira_check.check,
        &build_check,
        args.force,
    );
    if args.verbose {
        eprintln!("[cpu-sig-tracker] posting audit note");
    }
    client.add_note(iid, &note)?;

    // Flip the work-item status so browsers of the GitLab UI
    // see a terminal state, not just a closed issue. Mirrors
    // the JIRA resolution: "Done" for actual fixes, "Won't do"
    // for Won't Do / Obsolete / Cannot Reproduce / …
    let terminal_status =
        crate::status::gitlab_status_for_resolution(jira_check.resolution_name.as_deref());
    if args.verbose {
        eprintln!("[cpu-sig-tracker] setting work-item status to {terminal_status}");
    }
    if let Err(e) = client.set_work_item_status(iid, terminal_status) {
        eprintln!("warning: could not set work-item status to {terminal_status}: {e}");
    }

    // Stamp start_date / due_date via GraphQL — REST
    // PUT /issues ignores these for work items.
    let formatted_start = start_date
        .as_ref()
        .map(|(d, _)| d.format("%Y-%m-%d").to_string());
    let formatted_due = jira_check
        .resolution_date
        .map(|d| d.format("%Y-%m-%d").to_string());
    if formatted_start.is_some() || formatted_due.is_some() {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] setting start_date/due_date via GraphQL");
        }
        if let Err(e) =
            client.set_work_item_dates(iid, formatted_start.as_deref(), formatted_due.as_deref())
        {
            eprintln!("warning: could not set start/due dates: {e}");
        }
    }

    if args.verbose {
        eprintln!("[cpu-sig-tracker] closing issue");
    }
    let update = gitlab::IssueUpdate {
        state_event: Some("close".to_string()),
        ..Default::default()
    };
    client.edit_issue(iid, &update)?;

    eprintln!("closed {}", issue.web_url);
    Ok(())
}

/// Outcome of a single precondition check.
enum Check {
    Pass(String),
    Fail(String),
    Skipped(String),
}

impl Check {
    fn label(&self) -> &'static str {
        match self {
            Check::Pass(_) => "ok",
            Check::Fail(_) => "FAIL",
            Check::Skipped(_) => "skipped",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Check::Pass(d) | Check::Fail(d) | Check::Skipped(d) => d,
        }
    }
}

/// Outcome of the JIRA-resolved check, plus details extracted
/// from the fetch for use downstream (resolution name → GitLab
/// status, resolution date → due_date).
struct JiraCheck {
    check: Check,
    resolution_name: Option<String>,
    resolution_date: Option<chrono::NaiveDate>,
}

fn check_jira_resolved(jira_key: Option<&str>, verbose: bool) -> JiraCheck {
    let Some(key) = jira_key else {
        return JiraCheck {
            check: Check::Skipped("no JIRA key found in issue body".to_string()),
            resolution_name: None,
            resolution_date: None,
        };
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            return JiraCheck {
                check: Check::Skipped(format!("tokio runtime init failed: {e}")),
                resolution_name: None,
                resolution_date: None,
            };
        }
    };
    let jira_client = jira::client();
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    match runtime.block_on(jira_client.issue(key)) {
        Ok(Some(issue)) if issue.is_resolved() => JiraCheck {
            check: Check::Pass(format!("{} — {}", key, describe_jira(&issue))),
            resolution_name: issue.resolution().map(|s| s.to_string()),
            resolution_date: issue.resolution_date(),
        },
        Ok(Some(issue)) => JiraCheck {
            check: Check::Fail(format!("{} is {} (not resolved)", key, issue.status())),
            resolution_name: None,
            resolution_date: None,
        },
        Ok(None) => JiraCheck {
            check: Check::Skipped(format!("JIRA {key} not visible")),
            resolution_name: None,
            resolution_date: None,
        },
        Err(e) => JiraCheck {
            check: Check::Skipped(format!("JIRA {key} fetch failed: {e}")),
            resolution_name: None,
            resolution_date: None,
        },
    }
}

fn describe_jira(issue: &sandogasa_jira::Issue) -> String {
    match issue.resolution() {
        Some(res) => format!("{} ({})", issue.status(), res),
        None => issue.status().to_string(),
    }
}

fn check_package_untagged(release: &str, package: &str, verbose: bool) -> Check {
    let tag = match proposed_updates_tag(release) {
        Ok(t) => t,
        Err(e) => return Check::Skipped(e),
    };
    if verbose {
        eprintln!("[cpu-sig-tracker] listing tagged NVRs in {tag}");
    }
    let nvrs = match list_tagged_nvrs(&tag, Some(KOJI_PROFILE)) {
        Ok(v) => v,
        Err(e) => return Check::Skipped(format!("koji list-tagged {tag} failed: {e}")),
    };
    match nvrs.iter().find(|nvr| parse_nvr_name(nvr) == Some(package)) {
        Some(nvr) => Check::Fail(format!("package still tagged as {nvr} — run `untag` first")),
        None => Check::Pass(format!("no {package} build tagged in {tag}")),
    }
}

fn report_precondition(name: &str, check: &Check) {
    println!("check {name}: {} — {}", check.label(), check.detail());
}

fn compose_audit_note(
    jira_key: Option<&str>,
    jira_check: &Check,
    build_check: &Check,
    forced: bool,
) -> String {
    let jira_part = match jira_key {
        Some(k) => format!(" JIRA {k}: {}", jira_check.detail()),
        None => String::new(),
    };
    let build_part = format!(" Build: {}", build_check.detail());
    let forced_part = if forced { " (--force)" } else { "" };
    format!("Closing via `cpu-sig-tracker retire`{forced_part}.{jira_part}{build_part}")
}

/// Best-effort start_date for the tracking issue we're about
/// to close.
///
/// Tries Koji's `-release` / `-testing` tags first (matching
/// `file-issue`'s logic). When the build is no longer tagged
/// — the common case at retire-time, since retirement usually
/// follows untagging — falls back to the issue's own
/// `created_at` timestamp, which is a reasonable approximation
/// of when the SIG started tracking the package.
fn derive_start_date(
    package: &str,
    release: &str,
    issue: &gitlab::Issue,
    verbose: bool,
) -> Option<(chrono::NaiveDate, &'static str)> {
    if let Some(date) = crate::file_issue::find_build_start_date(package, release, verbose) {
        return Some((date, "Koji build creation time"));
    }
    issue
        .created_at
        .as_deref()
        .and_then(crate::utils::parse_iso_date)
        .map(|d| (d, "GitLab issue created_at"))
}

/// Find the `c<N>s` release label in `- **Release**: c10s`.
fn parse_release_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **Release**:") {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Find the RHEL-\d+ key in `- **JIRA**: [KEY](url)...`.
fn parse_jira_key_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(end) = rest.find(']')
        {
            return Some(rest[..end].to_string());
        }
    }
    None
}

fn confirm(prompt: &str) -> Result<bool, Box<dyn std::error::Error>> {
    eprint!("{prompt} [y/N]: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_release_from_standard_body() {
        let body = "- **Release**: c10s\n- **Other**: x\n";
        assert_eq!(parse_release_from_body(body).as_deref(), Some("c10s"));
    }

    #[test]
    fn parse_release_returns_none_when_missing() {
        assert_eq!(parse_release_from_body("no release line"), None);
    }

    #[test]
    fn parse_jira_key_from_standard_body() {
        let body = "- **JIRA**: [RHEL-1](https://example/) — New\n";
        assert_eq!(parse_jira_key_from_body(body).as_deref(), Some("RHEL-1"));
    }

    #[test]
    fn parse_jira_key_returns_none_when_missing() {
        assert_eq!(parse_jira_key_from_body("no jira"), None);
    }

    #[test]
    fn audit_note_includes_jira_and_build() {
        let note = compose_audit_note(
            Some("RHEL-12345"),
            &Check::Pass("RHEL-12345 — Closed (Done)".to_string()),
            &Check::Pass("no xz build tagged in proposed_updates10s-…".to_string()),
            false,
        );
        assert!(note.contains("cpu-sig-tracker retire"));
        assert!(note.contains("JIRA RHEL-12345"));
        assert!(note.contains("no xz build"));
        assert!(!note.contains("--force"));
    }

    #[test]
    fn audit_note_marks_force() {
        let note = compose_audit_note(
            None,
            &Check::Skipped("no JIRA key found".to_string()),
            &Check::Fail("still tagged".to_string()),
            true,
        );
        assert!(note.contains("--force"));
        assert!(note.contains("still tagged"));
    }

    // ---- end-to-end wiremock + fake-binary test ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{body_partial_json, method, path as wiremock_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const RETIRE_ISSUE_BODY: &str = "\
- **MR**: [Fix](https://gitlab.example/foo/bar/-/merge_requests/10) — merged\n\
- **JIRA**: [RHEL-1](https://jira.example/browse/RHEL-1) — Closed (Done)\n\
- **Release**: c10s\n";

    fn koji_empty_list_tagged() -> String {
        "Build  Tag  Built by\n-------  -----  --------\n".to_string()
    }

    #[test]
    #[serial_test::serial]
    fn retire_closes_issue_with_preconditions_passing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // Fetch the tracking issue.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1,
                    "title": "xz retire test",
                    "description": RETIRE_ISSUE_BODY,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1",
                    "assignees": [],
                })))
                .mount(&server)
                .await;

            // JIRA lookup — resolved as Done.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "Closed" },
                        "resolution": { "name": "Done" },
                        "resolutiondate": "2026-04-20T12:00:00.000+0000"
                    }
                })))
                .mount(&server)
                .await;

            // Audit note POST.
            Mock::given(method("POST"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1/notes",
                ))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": 1})))
                .expect(1)
                .mount(&server)
                .await;

            // GraphQL endpoint handles both workItemUpdate
            // mutations (status + dates) and the get_work_item_id /
            // resolve_status_id queries that set_work_item_status
            // performs. One catch-all responder covers them all.
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "project": {
                            "workItems": {
                                "nodes": [{
                                    "id": "gid://gitlab/WorkItem/99",
                                    "widgets": [
                                        { "type": "STATUS", "status": { "name": "Done" } }
                                    ],
                                    "namespace": {
                                        "workItemTypes": {
                                            "nodes": [{
                                                "name": "Issue",
                                                "widgetDefinitions": [{
                                                    "type": "STATUS",
                                                    "allowedStatuses": [{
                                                        "id": "gid://gitlab/status/1",
                                                        "name": "Done"
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

            // Final PUT that closes the issue.
            Mock::given(method("PUT"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/1",
                ))
                .and(body_partial_json(json!({ "state_event": "close" })))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 1, "title": "t", "state": "closed",
                    "web_url": "https://gitlab.example/…", "assignees": []
                })))
                .expect(1)
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                // list_tagged_nvrs for the precondition check:
                // no xz build currently tagged → passes.
                (
                    "list-tagged --quiet proposed_updates10s-packages-main-release",
                    &koji_empty_list_tagged(),
                ),
            ],
        );
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = RetireArgs {
            issue_url: "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/1"
                .to_string(),
            yes: true,
            force: false,
            verbose: false,
        };
        run_inner(&args).expect("retire succeeds");
    }
}
