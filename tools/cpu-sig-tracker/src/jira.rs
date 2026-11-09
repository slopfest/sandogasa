// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_jira`] for the Red Hat JIRA
//! (`https://redhat.atlassian.net`).
//! Loads the credentials from config if present; otherwise falls
//! back to anonymous access (which works for public issues).
//!
//! Also owns the blocking wrappers every subcommand uses to look
//! up an issue from synchronous code.

use crate::utils::Check;

/// Build a JIRA client, loading optional credentials from the
/// environment (`JIRA_EMAIL` and `JIRA_TOKEN`) or the local config.
/// Base URL comes from [`crate::utils::jira_base`] so tests can
/// point it at a mock server.
pub async fn client() -> Result<sandogasa_jira::JiraClient, String> {
    let (email, token) = credentials();
    with_credentials(email, token).await
}

/// Whether [`client`] has credentials to log in with, so a search that
/// finds nothing can say whom it searched as.
pub fn logged_in() -> bool {
    credentials().1.is_some()
}

/// The email and token [`client`] logs in with: the environment's,
/// else the config's, else none.
fn credentials() -> (Option<String>, Option<String>) {
    match std::env::var("JIRA_TOKEN") {
        Ok(t) => (std::env::var("JIRA_EMAIL").ok(), Some(t)),
        Err(_) => match crate::config::load().ok().and_then(|c| c.jira) {
            Some(j) => (j.email, Some(j.access_token)),
            None => (None, None),
        },
    }
}

/// A token with the account's email is an Atlassian Cloud API token:
/// basic auth, through Atlassian's gateway for the site (the only
/// place a scoped token works), whose tenant id is looked up once per
/// run. A token alone is a self-hosted personal access token, a
/// bearer against the site; none is anonymous, against the site.
pub async fn with_credentials(
    email: Option<String>,
    token: Option<String>,
) -> Result<sandogasa_jira::JiraClient, String> {
    let site = crate::utils::jira_base();
    Ok(match (email, token) {
        (Some(e), Some(t)) => {
            sandogasa_jira::JiraClient::new(&gateway_for(&site).await?).with_api_token(e, t)?
        }
        (None, Some(t)) => sandogasa_jira::JiraClient::new(&site).with_api_key(t)?,
        (_, None) => sandogasa_jira::JiraClient::new(&site),
    })
}

/// The gateway base URL for `site`, from its tenant id, remembered
/// for the run so a `status` over many issues asks once.
async fn gateway_for(site: &str) -> Result<String, String> {
    static GATEWAY: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    GATEWAY
        .get_or_try_init(|| async {
            sandogasa_jira::cloud_id(site)
                .await
                .map(|id| sandogasa_jira::gateway_url(&id))
                .map_err(|e| format!("cannot read {site}'s tenant id: {e}"))
        })
        .await
        .cloned()
}

/// Who the credentials belong to, blocking on the shared runtime;
/// `Err` carries the failure rendered for a message, with a 401
/// named as a rejected token.
pub fn whoami(email: Option<String>, token: String) -> Result<sandogasa_jira::Myself, String> {
    let runtime = runtime()?;
    runtime.block_on(async {
        with_credentials(email, Some(token))
            .await?
            .myself()
            .await
            .map_err(|e| {
                if e.status().is_some_and(|s| s.as_u16() == 401) {
                    "JIRA rejected the credentials (401; a scoped token needs read:jira-user)"
                        .to_string()
                } else {
                    e.to_string()
                }
            })
    })
}

/// Why a [`fetch`] failed, with the underlying error rendered to
/// a string so callers can word their own messages.
pub enum FetchError {
    /// The one-shot tokio runtime could not be created.
    Runtime(String),
    /// The HTTP lookup itself failed.
    Lookup(String),
}

/// Process-wide tokio runtime for the blocking JIRA lookups.
/// Built on first use so a `status` run over many issues pays
/// for one runtime, not one per issue.
pub(crate) fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    RUNTIME
        .get_or_init(|| tokio::runtime::Runtime::new().map_err(|e| e.to_string()))
        .as_ref()
        .map_err(|e| e.clone())
}

/// Fetch a JIRA issue by key, blocking on the shared runtime.
/// `Ok(None)` means JIRA answered but the issue is not visible
/// (private or missing).
pub fn fetch(key: &str, verbose: bool) -> Result<Option<sandogasa_jira::Issue>, FetchError> {
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    let runtime = runtime().map_err(FetchError::Runtime)?;
    runtime.block_on(async {
        client()
            .await
            .map_err(FetchError::Lookup)?
            .issue(key)
            .await
            .map_err(|e| FetchError::Lookup(e.to_string()))
    })
}

/// Warn-and-continue variant of [`fetch`]: prints a warning on
/// stderr and returns `None` on any failure.
pub fn fetch_or_warn(key: &str, verbose: bool) -> Option<sandogasa_jira::Issue> {
    match fetch(key, verbose) {
        Ok(Some(issue)) => Some(issue),
        Ok(None) => {
            eprintln!("warning: JIRA {key} not found or not visible");
            None
        }
        Err(FetchError::Runtime(e)) => {
            eprintln!("warning: could not start tokio runtime for JIRA lookup: {e}");
            None
        }
        Err(FetchError::Lookup(e)) => {
            eprintln!("warning: JIRA {key} lookup failed: {e}");
            None
        }
    }
}

/// Outcome of the JIRA-resolved precondition check, plus details
/// extracted from the fetch for use downstream (resolution name
/// → GitLab status, resolution date → due_date).
pub struct JiraCheck {
    pub check: Check,
    pub resolution_name: Option<String>,
    pub resolution_date: Option<chrono::NaiveDate>,
    /// Whether Jira shows a fix ([`Verdict::Fixed`]), the one thing
    /// that says the change reached RHEL.
    pub fixed: bool,
    /// Whether Red Hat said no ([`Verdict::Declined`]).
    pub declined: bool,
    /// Whether the issue was closed with no sign of a fix
    /// ([`Verdict::Unverified`]): no answer, rather than a no.
    pub unanswered: bool,
}

/// What a RHEL issue's resolution says about the change it asks for
/// (#87). Only [`Verdict::Fixed`] means the fix shipped: an issue can
/// be closed `Done` for inactivity, with nothing fixed.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Still open, in this status.
    Open(String),
    /// A resolution meaning fixed, with the releases the fix is in.
    Fixed {
        resolution: String,
        fix_versions: Vec<String>,
    },
    /// Closed with a resolution meaning fixed, but no fix version:
    /// nothing says a fix was planned for any release, as for an issue
    /// closed because nobody worked on it.
    Unverified(String),
    /// Closed without a fix: `Won't Do`, `Not a Bug`, `Obsolete`, ….
    Declined(String),
    /// Closed as a duplicate, of the issue named when it is visible; the
    /// duplicated issue's verdict is what counts.
    Duplicate(Option<String>),
}

/// The resolutions Red Hat's Jira uses for a fix.
fn means_fixed(resolution: &str) -> bool {
    matches!(
        resolution,
        "Done"
            | "Done-Errata"
            | "Errata"
            | "Fixed"
            | "Resolved"
            | "Current Release"
            | "Next Release"
    )
}

/// An issue's [`Verdict`], from its status, resolution, fix versions
/// and links.
pub fn verdict_of(issue: &sandogasa_jira::Issue) -> Verdict {
    match issue.resolution() {
        None => Verdict::Open(issue.status().to_string()),
        Some("Duplicate") => Verdict::Duplicate(issue.duplicate_of().map(str::to_string)),
        Some(r) if means_fixed(r) => {
            let fix_versions: Vec<String> = issue
                .fix_versions()
                .into_iter()
                .map(str::to_string)
                .collect();
            // `Done-Errata` names the erratum that shipped the fix, so it
            // stands on its own (RHEL-170528, PackageKit c9s, has no fix
            // version); a plain `Done` needs one to be told apart from
            // an issue closed for inactivity.
            if fix_versions.is_empty() && r != "Done-Errata" {
                Verdict::Unverified(r.to_string())
            } else {
                Verdict::Fixed {
                    resolution: r.to_string(),
                    fix_versions,
                }
            }
        }
        Some(r) => Verdict::Declined(r.to_string()),
    }
}

/// How a check reports `key`'s verdict: passed only for a fix.
pub fn verdict_check(key: &str, verdict: &Verdict) -> Check {
    match verdict {
        Verdict::Fixed {
            resolution,
            fix_versions,
        } if fix_versions.is_empty() => Check::Pass(format!("{key} — {resolution}")),
        Verdict::Fixed {
            resolution,
            fix_versions,
        } => Check::Pass(format!(
            "{key} — {resolution}, fixed in {}",
            fix_versions.join(", ")
        )),
        Verdict::Open(status) => Check::Fail(format!("{key} is {status} (not resolved)")),
        Verdict::Unverified(resolution) => Check::Fail(format!(
            "{key} was closed {resolution} without a fix version: nothing says a fix \
             shipped (closed for inactivity?); check stock for the change"
        )),
        Verdict::Declined(resolution) => {
            Check::Fail(format!("{key} was closed {resolution}: not fixed in RHEL"))
        }
        Verdict::Duplicate(Some(of)) => {
            Check::Skipped(format!("{key} duplicates {of}, which could not be read"))
        }
        Verdict::Duplicate(None) => Check::Skipped(format!(
            "{key} is a duplicate of an issue this account cannot see"
        )),
    }
}

/// Whether every RHEL issue an update resolves is resolved (#86):
/// [`check_resolved`] for each, combined by [`combine_checks`]. No
/// keys is `Skipped`, as for one missing key.
pub fn check_all_resolved(keys: &[String], verbose: bool) -> JiraCheck {
    if keys.is_empty() {
        return check_resolved(None, verbose);
    }
    combine_checks(
        keys.iter()
            .map(|k| check_resolved(Some(k), verbose))
            .collect(),
    )
}

/// Several issues' checks as one: a failure if any is unresolved (the
/// failures named), skipped if any could not be read, passed only when
/// all are resolved. The resolution is a fix only when every one is
/// (otherwise the first that is not, so the work item reads Won't do
/// rather than Done for an update partly declined), and the date the
/// latest, when the last of them was resolved.
pub fn combine_checks(checks: Vec<JiraCheck>) -> JiraCheck {
    if checks.len() == 1 {
        return checks.into_iter().next().expect("one check");
    }
    let details = |pick: fn(&Check) -> bool| -> String {
        checks
            .iter()
            .filter(|c| pick(&c.check))
            .map(|c| c.check.detail().to_string())
            .collect::<Vec<_>>()
            .join("; ")
    };
    let check = if checks.iter().any(|c| matches!(c.check, Check::Fail(_))) {
        Check::Fail(details(|c| matches!(c, Check::Fail(_))))
    } else if checks.iter().any(|c| matches!(c.check, Check::Skipped(_))) {
        Check::Skipped(details(|c| matches!(c, Check::Skipped(_))))
    } else {
        Check::Pass(details(|_| true))
    };
    let resolved = matches!(check, Check::Pass(_));
    let is_fix = |r: &str| matches!(r, "Done" | "Fixed" | "Resolved");
    let resolution_name = resolved
        .then(|| {
            checks
                .iter()
                .filter_map(|c| c.resolution_name.clone())
                .find(|r| !is_fix(r))
                .or_else(|| checks.iter().find_map(|c| c.resolution_name.clone()))
        })
        .flatten();
    let resolution_date = resolved
        .then(|| checks.iter().filter_map(|c| c.resolution_date).max())
        .flatten();
    JiraCheck {
        check,
        resolution_name,
        resolution_date,
        fixed: checks.iter().all(|c| c.fixed),
        declined: checks.iter().all(|c| c.declined),
        unanswered: checks.iter().all(|c| c.unanswered),
    }
}

/// Check whether the JIRA issue behind `jira_key` is resolved.
/// A missing key or a failed fetch is `Skipped`; an unresolved
/// issue is `Fail`.
pub fn check_resolved(jira_key: Option<&str>, verbose: bool) -> JiraCheck {
    fn skipped(detail: String) -> JiraCheck {
        JiraCheck {
            check: Check::Skipped(detail),
            resolution_name: None,
            resolution_date: None,
            fixed: false,
            declined: false,
            unanswered: false,
        }
    }
    let Some(key) = jira_key else {
        return skipped("no JIRA key found in issue body".to_string());
    };
    let issue = match fetch(key, verbose) {
        Ok(Some(issue)) => issue,
        Ok(None) => return skipped(format!("JIRA {key} not visible")),
        Err(FetchError::Runtime(e)) => return skipped(format!("tokio runtime init failed: {e}")),
        Err(FetchError::Lookup(e)) => return skipped(format!("JIRA {key} fetch failed: {e}")),
    };
    // A duplicate stands or falls with the issue it duplicates, one
    // link deep, when that one is visible.
    let (judged, verdict) = match verdict_of(&issue) {
        Verdict::Duplicate(Some(of)) => match fetch(&of, verbose) {
            Ok(Some(original)) => {
                let v = verdict_of(&original);
                let check = verdict_check(&of, &v);
                let check = match check {
                    Check::Pass(d) => Check::Pass(format!("{key} duplicates {d}")),
                    Check::Fail(d) => Check::Fail(format!("{key} duplicates {d}")),
                    Check::Skipped(d) => Check::Skipped(format!("{key} duplicates {d}")),
                };
                return JiraCheck {
                    fixed: matches!(v, Verdict::Fixed { .. }),
                    declined: matches!(v, Verdict::Declined(_)),
                    unanswered: matches!(v, Verdict::Unverified(_)),
                    resolution_name: original.resolution().map(str::to_string),
                    resolution_date: original.resolution_date(),
                    check,
                };
            }
            _ => (&issue, Verdict::Duplicate(Some(of))),
        },
        v => (&issue, v),
    };
    JiraCheck {
        check: verdict_check(key, &verdict),
        fixed: matches!(verdict, Verdict::Fixed { .. }),
        declined: matches!(verdict, Verdict::Declined(_)),
        unanswered: matches!(verdict, Verdict::Unverified(_)),
        resolution_name: judged.resolution().map(str::to_string),
        resolution_date: judged.resolution_date(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(check: Check, resolution: Option<&str>, day: Option<u32>) -> JiraCheck {
        JiraCheck {
            fixed: matches!(check, Check::Pass(_)),
            declined: false,
            unanswered: false,
            check,
            resolution_name: resolution.map(str::to_string),
            resolution_date: day.and_then(|d| chrono::NaiveDate::from_ymd_opt(2026, 12, d)),
        }
    }

    /// An issue as Jira answers for it.
    fn issue(
        resolution: Option<&str>,
        fix_versions: &[&str],
        links: serde_json::Value,
    ) -> sandogasa_jira::Issue {
        serde_json::from_value(serde_json::json!({
            "key": "RHEL-1",
            "fields": {
                "summary": "s",
                "status": {"name": if resolution.is_some() { "Closed" } else { "New" }},
                "resolution": resolution.map(|r| serde_json::json!({"name": r})),
                "fixVersions": fix_versions.iter().map(|v| serde_json::json!({"name": v})).collect::<Vec<_>>(),
                "issuelinks": links,
            }
        }))
        .unwrap()
    }

    #[test]
    fn only_a_resolution_with_a_fix_version_is_a_fix() {
        let none = serde_json::json!([]);
        // RHEL-186589: worked, Done in rhel-10.3.
        assert_eq!(
            verdict_of(&issue(Some("Done"), &["rhel-10.3"], none.clone())),
            Verdict::Fixed {
                resolution: "Done".into(),
                fix_versions: vec!["rhel-10.3".into()],
            }
        );
        // RHEL-123605/123606 (blktrace): New -> Closed Done, no version.
        let blktrace = verdict_of(&issue(Some("Done"), &[], none.clone()));
        assert_eq!(blktrace, Verdict::Unverified("Done".into()));
        assert!(matches!(
            verdict_check("RHEL-123606", &blktrace),
            Check::Fail(d) if d.contains("without a fix version")
        ));
        // RHEL-170528 (PackageKit c9s): Done-Errata names the erratum.
        assert_eq!(
            verdict_of(&issue(Some("Done-Errata"), &[], none.clone())),
            Verdict::Fixed {
                resolution: "Done-Errata".into(),
                fix_versions: vec![],
            }
        );
        // RHEL-170526 (PackageKit c10s): Not a Bug.
        assert_eq!(
            verdict_of(&issue(Some("Not a Bug"), &[], none.clone())),
            Verdict::Declined("Not a Bug".into())
        );
        // RHEL-83280 (freetype): a duplicate of a restricted issue.
        assert_eq!(
            verdict_of(&issue(Some("Duplicate"), &[], none.clone())),
            Verdict::Duplicate(None)
        );
        assert_eq!(
            verdict_of(&issue(
                Some("Duplicate"),
                &[],
                serde_json::json!([{"type": {"name": "Duplicate"}, "outwardIssue": {"key": "RHEL-2"}}])
            )),
            Verdict::Duplicate(Some("RHEL-2".into()))
        );
        assert_eq!(
            verdict_of(&issue(None, &[], none)),
            Verdict::Open("New".into())
        );
        assert!(matches!(
            verdict_check("RHEL-1", &Verdict::Duplicate(None)),
            Check::Skipped(_)
        ));
    }

    #[test]
    fn several_issues_pass_only_when_every_one_is_resolved() {
        let done = |key: &str, day| {
            checked(
                Check::Pass(format!("{key} — Closed (Done)")),
                Some("Done"),
                Some(day),
            )
        };
        let all = combine_checks(vec![done("RHEL-1", 1), done("RHEL-2", 9)]);
        assert!(
            matches!(&all.check, Check::Pass(d) if d.contains("RHEL-1") && d.contains("RHEL-2"))
        );
        assert_eq!(all.resolution_name.as_deref(), Some("Done"));
        assert_eq!(
            all.resolution_date,
            chrono::NaiveDate::from_ymd_opt(2026, 12, 9)
        );

        // One still open: the update is not resolved, and says which.
        let one_open = combine_checks(vec![
            done("RHEL-1", 1),
            checked(
                Check::Fail("RHEL-2 is New (not resolved)".into()),
                None,
                None,
            ),
        ]);
        assert!(matches!(&one_open.check, Check::Fail(d) if d == "RHEL-2 is New (not resolved)"));
        assert_eq!(one_open.resolution_date, None);

        // One declined: resolved, but not as a fix.
        let declined = combine_checks(vec![
            done("RHEL-1", 1),
            checked(
                Check::Pass("RHEL-2 — Closed (Won't Do)".into()),
                Some("Won't Do"),
                Some(3),
            ),
        ]);
        assert_eq!(declined.resolution_name.as_deref(), Some("Won't Do"));

        // One unreadable: neither passed nor failed.
        let unread = combine_checks(vec![
            done("RHEL-1", 1),
            checked(Check::Skipped("JIRA RHEL-2 not visible".into()), None, None),
        ]);
        assert!(matches!(unread.check, Check::Skipped(_)));
    }
}
