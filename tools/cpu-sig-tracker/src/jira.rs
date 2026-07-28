// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Thin wrapper around [`sandogasa_jira`] for the Red Hat JIRA
//! (`https://issues.redhat.com`). Loads the API token from config
//! if present; otherwise falls back to anonymous access (which
//! works for public issues).
//!
//! Also owns the blocking wrappers every subcommand uses to look
//! up an issue from synchronous code.

use crate::utils::Check;

/// Build a JIRA client, loading an optional token from the
/// environment (`JIRA_TOKEN`) or the local config. Base URL
/// comes from [`crate::utils::jira_base`] so tests can point
/// it at a mock server.
pub fn client() -> sandogasa_jira::JiraClient {
    let token = std::env::var("JIRA_TOKEN").ok().or_else(|| {
        crate::config::load()
            .ok()
            .and_then(|c| c.jira.map(|j| j.access_token))
    });
    let c = sandogasa_jira::JiraClient::new(&crate::utils::jira_base());
    match token {
        Some(t) => c.with_api_key(t),
        None => c,
    }
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
fn runtime() -> Result<&'static tokio::runtime::Runtime, String> {
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
    runtime
        .block_on(client().issue(key))
        .map_err(|e| FetchError::Lookup(e.to_string()))
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
        }
    }
    let Some(key) = jira_key else {
        return skipped("no JIRA key found in issue body".to_string());
    };
    match fetch(key, verbose) {
        Ok(Some(issue)) if issue.is_resolved() => {
            let summary = match issue.resolution() {
                Some(r) => format!("{} ({})", issue.status(), r),
                None => issue.status().to_string(),
            };
            JiraCheck {
                check: Check::Pass(format!("{key} — {summary}")),
                resolution_name: issue.resolution().map(|s| s.to_string()),
                resolution_date: issue.resolution_date(),
            }
        }
        Ok(Some(issue)) => JiraCheck {
            check: Check::Fail(format!("{key} is {} (not resolved)", issue.status())),
            resolution_name: None,
            resolution_date: None,
        },
        Ok(None) => skipped(format!("JIRA {key} not visible")),
        Err(FetchError::Runtime(e)) => skipped(format!("tokio runtime init failed: {e}")),
        Err(FetchError::Lookup(e)) => skipped(format!("JIRA {key} fetch failed: {e}")),
    }
}
