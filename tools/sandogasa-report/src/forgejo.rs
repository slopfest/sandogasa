// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Forgejo / Gitea activity reporting — pull requests the user
//! opened and merged in a date window. Sourced from the global
//! issue/pull search (`/repos/issues/search?type=pulls&created=true`),
//! which returns the token owner's PRs (or, with no token, a named
//! user's) across every repo they
//! contribute to (e.g. on codeberg.org, where contributions land in
//! other people's repos, not just the user's own org).
//!
//! Structured to mirror `crate::github` so a Forgejo domain renders
//! the same way and plugs into `report::format_markdown` without
//! special-casing. One search call yields both metrics: a PR counts as
//! *opened* when its `created_at` falls in the window, and *merged*
//! when it was merged and its `merged_at` falls in the window.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use sandogasa_forgejo::{Client, Issue, PullDetail, PullRequest, RepoCommit};
use serde::Serialize;

use crate::config::ForgejoConfig;
pub(crate) use crate::forge::instance_host;
use crate::forge::{self, TokenSpec, date_in_range, stat};

const TOKEN_SPEC: TokenSpec = TokenSpec {
    service: "Forgejo/Gitea",
    label: "sandogasa: Forgejo token",
    generic_env: "FORGEJO_TOKEN",
    host_separators: &['.'],
    hint: "",
};

/// A user's Forgejo activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct ForgejoReport {
    /// Instance root URL the report was fetched from.
    pub instance: String,
    /// Forgejo login reported on: the token owner, or the name the
    /// anonymous search was made by.
    pub user: String,
    /// Repo-owner filter, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    /// PRs opened by the user in the window.
    pub opened_prs: Vec<PrRef>,
    /// PRs by the user that were merged in the window.
    pub merged_prs: Vec<PrRef>,
    /// Issues opened by the user in the window.
    pub opened_issues: Vec<IssueRef>,
    /// Issues by the user that were closed in the window.
    pub closed_issues: Vec<IssueRef>,
    /// Issues the user commented on in the window, from their activity
    /// feed, each once.
    pub commented_issues: Vec<IssueRef>,
    /// What the instance could not be asked for, and why: an anonymous
    /// search by name on a server without one.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub not_listed: Vec<String>,
}

/// Pointer to an issue for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Issue number.
    pub number: u64,
    /// Issue title.
    pub title: String,
    /// Issue URL.
    pub url: String,
}

/// Pointer to a pull request for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct PrRef {
    /// `owner/name` slug.
    pub repo: String,
    /// PR number.
    pub number: u64,
    /// PR title.
    pub title: String,
    /// PR URL (the canonical web link).
    pub url: String,
    /// Current state (`open` / `closed`). A merged PR is `closed`
    /// with `merged = true`; a declined PR is `closed` with
    /// `merged = false`.
    pub state: String,
    /// Whether the PR was merged (vs. closed without merging).
    pub merged: bool,
    /// Whether a closed-but-unmerged PR's commit nonetheless landed on
    /// the target branch (a maintainer applied it out-of-band rather
    /// than clicking merge). Determined by a follow-up API check.
    #[serde(default)]
    pub applied: bool,
}

impl PrRef {
    /// A state marker for the **opened** list, so a PR that was
    /// opened in the window but has since landed or been declined
    /// isn't shown as if it were still open. Empty for an open PR.
    fn status_marker(&self) -> &'static str {
        if self.merged {
            " (merged)"
        } else if self.applied {
            " (applied)"
        } else if self.state == "closed" {
            " (closed)"
        } else {
            ""
        }
    }
}

/// Build the Forgejo activity report for one domain.
pub fn forgejo_report(
    cfg: &ForgejoConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    tokens: &BTreeMap<String, String>,
    verbose: bool,
) -> Result<ForgejoReport, String> {
    let base = cfg.instance.trim_end_matches('/');
    // With a token the search is "what I created"; without one, the
    // public search by name, which instances such as src.opensuse.org
    // answer anonymously. No token and no name is the old error.
    let (client, by) = match find_token(&cfg.instance, tokens) {
        Ok(token) => (Client::new(base, &token), None),
        Err(_) if !user.is_empty() => (Client::anonymous(base), Some(user)),
        Err(e) => return Err(e),
    };
    let client = client.map_err(|e| format!("Forgejo/Gitea client setup on {base}: {e}"))?;
    let whose = match by {
        Some(u) => format!("{u} (anonymously)"),
        None => "the token owner".to_string(),
    };

    eprintln!("Searching {base} for the pull requests and issues {whose} opened...");
    if verbose {
        eprintln!("[forgejo] {base}: searching pulls created by {whose}");
    }
    let mut report = ForgejoReport {
        instance: base.to_string(),
        user: user.to_string(),
        owner: cfg.owner.clone(),
        ..Default::default()
    };
    // One `state=all` search gives both opened and merged; we filter
    // each by the relevant timestamp.
    let pulls = searched(
        &mut report,
        by.is_some(),
        "pull requests",
        match by {
            Some(u) => client.pull_requests_by(u, "all", cfg.owner.as_deref()),
            None => client.my_pull_requests("all", cfg.owner.as_deref()),
        },
    )?;
    for pr in pulls {
        if pr
            .created_at
            .as_deref()
            .is_some_and(|d| date_in_range(d, since, until))
        {
            let mut r = into_pr_ref(&pr);
            // A PR opened in the window but now closed-without-merging
            // may still have landed (commit applied out-of-band). Check
            // whether its head commit is on the target branch.
            if r.state == "closed" && !r.merged {
                r.applied = applied_out_of_band(&client, &r, verbose);
            }
            report.opened_prs.push(r);
        }
        if pr.is_merged()
            && pr
                .merged_at()
                .is_some_and(|d| date_in_range(d, since, until))
        {
            report.merged_prs.push(into_pr_ref(&pr));
        }
    }

    if verbose {
        eprintln!("[forgejo] {base}: searching issues created by {whose}");
    }
    let issues = searched(
        &mut report,
        by.is_some(),
        "issues",
        match by {
            Some(u) => client.issues_by(u, "all", cfg.owner.as_deref()),
            None => client.my_issues("all", cfg.owner.as_deref()),
        },
    )?;
    for issue in issues {
        if issue
            .created_at
            .as_deref()
            .is_some_and(|d| date_in_range(d, since, until))
        {
            report.opened_issues.push(into_issue_ref(&issue));
        }
        if issue.state == "closed"
            && issue
                .closed_at()
                .is_some_and(|d| date_in_range(d, since, until))
        {
            report.closed_issues.push(into_issue_ref(&issue));
        }
    }
    // The feed of the account the searches covered: the token's owner,
    // whose login the profile need not name, or the name searched by.
    let feed_user = match by {
        Some(u) => Some(u.to_string()),
        None => match client.current_user() {
            Ok(owner) => {
                // The token's owner is who the report is about, when
                // no name was given.
                if report.user.is_empty() {
                    report.user = owner.login.clone();
                }
                Some(owner.login)
            }
            Err(e) => {
                eprintln!(
                    "warning: cannot tell whose token is used on {base} ({e}); issues \
                     commented on are left out"
                );
                None
            }
        },
    };
    if let Some(feed_user) = feed_user {
        report.commented_issues = commented_issues(&client, base, &feed_user, since, until);
    }
    Ok(report)
}

/// What a search found. An anonymous search by name that failed is a
/// warning, recorded in `report` as not listed, since the other lists
/// and the activity feed's still stand; a search with a token that
/// failed (an expired token, one meant for another instance) ends the
/// run, as a wrong token should not read as a quiet period.
fn searched<T>(
    report: &mut ForgejoReport,
    anonymous: bool,
    what: &str,
    found: Result<Vec<T>, Box<dyn std::error::Error>>,
) -> Result<Vec<T>, String> {
    match found {
        Ok(found) => Ok(found),
        Err(e) if anonymous => {
            eprintln!(
                "warning: {what} are not listed: {e}; give a token for {} \
                 (`sandogasa-report config`)",
                report.instance
            );
            // Said in the report without the error, which quotes a
            // stranger's login from the answer.
            let why = if e.to_string().contains("does not search by creator") {
                "searching by creator needs a token here"
            } else {
                "the search failed"
            };
            report.not_listed.push(format!("{what} ({why})"));
            Ok(Vec::new())
        }
        Err(e) => Err(format!(
            "Forgejo/Gitea {what} search on {}: {e}",
            report.instance
        )),
    }
}

/// How many requests to a Forgejo instance are in flight at once.
const PARALLEL: usize = 8;

/// The issues `user` commented on in the window, each once, newest
/// first: from their activity feed (which names the issue but not its
/// title), with the titles looked up a few at a time. A feed that
/// cannot be read is a warning, and the list stays empty.
fn commented_issues(
    client: &Client,
    base: &str,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
) -> Vec<IssueRef> {
    eprintln!("Reading the activity of {user} on {base} since {since}...");
    let feed = match client.user_activity(user, Some(&format!("{since}T00:00:00Z"))) {
        Ok(feed) => feed,
        Err(e) => {
            eprintln!(
                "warning: cannot read the activity of {user} on {base} ({e}); issues \
                 commented on are left out"
            );
            return Vec::new();
        }
    };
    if feed.len() >= sandogasa_forgejo::FEED_EVENT_CAP {
        eprintln!(
            "warning: the activity of {user} on {base} has more than {} events since \
             {since}; issues commented on early in the period may be missing",
            sandogasa_forgejo::FEED_EVENT_CAP
        );
    }
    let mut seen = std::collections::BTreeSet::new();
    let wanted: Vec<(String, u64)> = feed
        .iter()
        .filter(|a| a.op_type == "comment_issue" && date_in_range(&a.created, since, until))
        .filter_map(|a| Some((a.repo_slug()?.to_string(), a.issue_ref()?.0)))
        .filter(|key| seen.insert(key.clone()))
        .collect();
    if wanted.is_empty() {
        return Vec::new();
    }
    eprintln!(
        "Looking up {} issue(s) commented on at {base}...",
        wanted.len()
    );
    let titles = forge::in_parallel(&wanted, PARALLEL, |(slug, number)| {
        let (owner, repo) = slug.split_once('/')?;
        client.issue(owner, repo, *number).ok().map(|i| i.title)
    });
    let unread = titles.iter().filter(|t| t.is_none()).count();
    if unread > 0 {
        eprintln!("warning: cannot read the titles of {unread} issue(s) on {base}");
    }
    wanted
        .into_iter()
        .zip(titles)
        .map(|((repo, number), title)| IssueRef {
            url: format!("{base}/{repo}/issues/{number}"),
            repo,
            number,
            title: title.unwrap_or_else(|| "(title not readable)".to_string()),
        })
        .collect()
}

/// Whether a closed-unmerged PR's commit nonetheless landed on its
/// target branch. Fetches the PR's head/base refs (the search result
/// omits them), then asks whether the head commit is contained in the
/// base branch. Any lookup failure is treated as "not applied" (a
/// warning under `--verbose`) — we only ever *upgrade* the label when
/// we're certain, never falsely claim a contribution landed.
fn applied_out_of_band(client: &Client, pr: &PrRef, verbose: bool) -> bool {
    let Some((owner, repo)) = pr.repo.split_once('/') else {
        return false;
    };
    let detail = match client.pull_request(owner, repo, pr.number) {
        Ok(d) => d,
        Err(e) => {
            if verbose {
                eprintln!(
                    "[forgejo] applied-check: fetch {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            return false;
        }
    };
    match client.commit_contained(owner, repo, &detail.base.ref_name, &detail.head.sha) {
        Ok(true) => true,
        Ok(false) => landed_rebased(client, owner, repo, pr, &detail, verbose),
        Err(e) => {
            if verbose {
                eprintln!(
                    "[forgejo] applied-check: compare for {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            false
        }
    }
}

/// The fallback when the PR's own commit is not on the target branch:
/// rebased or reworded, it lands under a new sha. Match the PR's
/// commits against the branch's commits since the PR was opened, by
/// title and author (`forge::rebased_onto`). Two calls, spent only on
/// a PR that has already failed the sha check.
fn landed_rebased(
    client: &Client,
    owner: &str,
    repo: &str,
    pr: &PrRef,
    detail: &PullDetail,
    verbose: bool,
) -> bool {
    let Some(since) = detail.created_at.as_deref() else {
        return false;
    };
    let result = client
        .pull_request_commits(owner, repo, pr.number)
        .and_then(|pr_commits| {
            let branch = client.branch_commits_since(owner, repo, &detail.base.ref_name, since)?;
            Ok(forge::rebased_onto(&landed(&pr_commits), &landed(&branch)))
        });
    match result {
        Ok(applied) => applied,
        Err(e) => {
            if verbose {
                eprintln!(
                    "[forgejo] applied-check: rebased lookup for {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            false
        }
    }
}

fn landed(commits: &[RepoCommit]) -> Vec<forge::Landed> {
    commits
        .iter()
        .map(|c| forge::Landed {
            title: c.title().to_string(),
            author_name: c
                .commit
                .author
                .as_ref()
                .map(|a| a.name.clone())
                .unwrap_or_default(),
            author_email: c
                .commit
                .author
                .as_ref()
                .map(|a| a.email.clone())
                .unwrap_or_default(),
        })
        .collect()
}

/// Format the Forgejo section as Markdown.
pub fn format_markdown(report: &ForgejoReport, detail: u8) -> String {
    let detailed = detail >= 1;
    let heading = "### Forgejo/Gitea\n\n".to_string();

    let applied = report.opened_prs.iter().filter(|p| p.applied).count();

    // See the note in `gitlab::format_markdown`: the block decides
    // for itself whether anything happened.
    let mut stats = String::new();
    stat(&mut stats, "PRs opened", report.opened_prs.len());
    stat(&mut stats, "PRs merged", report.merged_prs.len());
    // A closed PR whose commit landed out-of-band — counted separately
    // since the forge reports it as neither merged nor open.
    stat(&mut stats, "PRs applied (landed unmerged)", applied);
    stat(&mut stats, "Issues opened", report.opened_issues.len());
    stat(&mut stats, "Issues closed", report.closed_issues.len());
    stat(
        &mut stats,
        "Issues commented on",
        report.commented_issues.len(),
    );

    // What could not be asked for is said, so a missing list does not
    // read as a quiet period.
    for what in &report.not_listed {
        stats.push_str(&format!("- _Not listed: {what}_\n"));
    }
    if stats.is_empty() {
        return format!("{heading}No Forgejo/Gitea activity.\n\n");
    }
    let mut out = heading;
    out.push_str(&stats);
    out.push('\n');

    if !detailed {
        return out;
    }
    // Level 1 counts by repo; level 2 lists the items (DEVELOPMENT.md,
    // "Detail levels").
    let deep = detail >= 2;
    let prs = |out: &mut String, heading: &str, prs: &[PrRef], markers: bool| {
        if prs.is_empty() {
            return;
        }
        out.push_str(&format!("#### {heading}\n\n"));
        if deep {
            write_pr_list(out, prs, markers);
        } else {
            forge::write_counts_by(out, prs, |p| p.repo.as_str());
        }
    };
    let issues = |out: &mut String, heading: &str, issues: &[IssueRef]| {
        if issues.is_empty() {
            return;
        }
        out.push_str(&format!("#### {heading}\n\n"));
        if deep {
            write_issue_list(out, issues);
        } else {
            forge::write_counts_by(out, issues, |i| i.repo.as_str());
        }
    };
    prs(&mut out, "PRs opened", &report.opened_prs, true);
    prs(&mut out, "PRs merged", &report.merged_prs, false);
    issues(&mut out, "Issues opened", &report.opened_issues);
    issues(&mut out, "Issues closed", &report.closed_issues);
    issues(&mut out, "Issues commented on", &report.commented_issues);
    out
}

fn into_pr_ref(pr: &PullRequest) -> PrRef {
    PrRef {
        repo: pr.repo_slug().unwrap_or_default().to_string(),
        number: pr.number,
        title: pr.title.clone(),
        url: pr.html_url.clone(),
        state: pr.state.clone(),
        merged: pr.is_merged(),
        applied: false,
    }
}

fn into_issue_ref(issue: &Issue) -> IssueRef {
    IssueRef {
        repo: issue.repo_slug().unwrap_or_default().to_string(),
        number: issue.number,
        title: issue.title.clone(),
        url: issue.html_url.clone(),
    }
}

fn write_issue_list(out: &mut String, issues: &[IssueRef]) {
    for it in issues {
        out.push_str(&format!(
            "- [{}#{}]({}) {}\n",
            it.repo, it.number, it.url, it.title,
        ));
    }
    out.push('\n');
}

/// Render a PR list. `with_status` appends a `(merged)`/`(closed)`
/// marker — used for the opened list, where a PR's later fate matters;
/// the merged list is uniformly merged, so it's rendered without.
fn write_pr_list(out: &mut String, prs: &[PrRef], with_status: bool) {
    for pr in prs {
        let marker = if with_status { pr.status_marker() } else { "" };
        out.push_str(&format!(
            "- [{}#{}]({}) {}{marker}\n",
            pr.repo, pr.number, pr.url, pr.title,
        ));
    }
    out.push('\n');
}

/// The keyring entry for `instance`'s token, which `config` keeps
/// it in.
pub(crate) fn secret(instance: &str) -> sandogasa_config::secret::SecretRef {
    forge::secret(instance, &TOKEN_SPEC)
}

/// Look up the Forgejo API token for an instance.
///
/// Order: instance-specific env var → generic env var → the
/// keyring's entry for the instance → `forgejo_tokens.<hostname>` from the user
/// overlay → error.
pub(crate) fn find_token(
    instance: &str,
    tokens: &BTreeMap<String, String>,
) -> Result<String, String> {
    forge::find_token(instance, tokens, &TOKEN_SPEC)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The env-var name is a documented interface, so keep
    /// asserting it per service through the module's spec.
    fn instance_token_env(instance: &str) -> String {
        forge::instance_token_env(instance, &TOKEN_SPEC)
    }

    fn pr(json: &str) -> PullRequest {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn date_in_range_parses_rfc3339() {
        let s = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        assert!(date_in_range("2026-06-25T11:22:09+02:00", s, u));
        assert!(!date_in_range("2026-07-01T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn into_pr_ref_pulls_slug_and_url() {
        let p = pr(r#"{"number":35,"title":"Fix select box","state":"closed",
            "html_url":"https://codeberg.org/o/r/pulls/35",
            "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#);
        let r = into_pr_ref(&p);
        assert_eq!(r.repo, "o/r");
        assert_eq!(r.number, 35);
        assert_eq!(r.url, "https://codeberg.org/o/r/pulls/35");
    }

    #[test]
    fn instance_token_env_generates_env_var_name() {
        assert_eq!(
            instance_token_env("https://codeberg.org/"),
            "FORGEJO_TOKEN_CODEBERG_ORG"
        );
    }

    #[test]
    #[serial_test::serial]
    fn find_token_errors_when_nothing_set() {
        let tokens = BTreeMap::new();
        let err = find_token("https://nonexistent.example.test", &tokens).unwrap_err();
        assert!(err.contains("no Forgejo/Gitea token"));
    }

    #[test]
    #[serial_test::serial]
    fn find_token_falls_back_to_config() {
        let mut tokens = BTreeMap::new();
        tokens.insert(
            "nonexistent.example.test".to_string(),
            "from-config".to_string(),
        );
        let var = instance_token_env("https://nonexistent.example.test");
        if std::env::var(&var).is_ok() || std::env::var("FORGEJO_TOKEN").is_ok() {
            return;
        }
        let tok = find_token("https://nonexistent.example.test", &tokens).unwrap();
        assert_eq!(tok, "from-config");
    }

    #[test]
    fn format_empty_report() {
        let report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        let md = format_markdown(&report, 0);
        assert!(md.contains("### Forgejo/Gitea\n"));
        assert!(md.contains("No Forgejo/Gitea activity"));
    }

    #[test]
    fn format_non_empty_lists_merged() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.merged_prs.push(PrRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 92,
            title: "Drop removed bfd macros".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/pulls/92".into(),
            state: "closed".into(),
            merged: true,
            applied: false,
        });
        let md = format_markdown(&report, 2);
        assert!(md.contains("### Forgejo/Gitea\n"));
        assert!(md.contains("**PRs merged:** 1"));
        assert!(md.contains("#### PRs merged"));
        assert!(md.contains("ptesarik/libkdumpfile#92"));
        assert!(md.contains("Drop removed bfd macros"));
        // The merged list isn't annotated with a redundant marker.
        assert!(!md.contains("(merged)"));
    }

    #[test]
    fn opened_list_marks_closed_and_merged_state() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        // Opened then declined (closed, not merged, not applied) → "(closed)".
        report.opened_prs.push(PrRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 92,
            title: "Drop removed bfd macros".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/pulls/92".into(),
            state: "closed".into(),
            merged: false,
            applied: false,
        });
        // Opened and still open → no marker.
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 7,
            title: "still going".into(),
            url: "https://codeberg.org/o/r/pulls/7".into(),
            state: "open".into(),
            merged: false,
            applied: false,
        });
        // Closed without merging, but the commit landed out-of-band → "(applied)".
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 8,
            title: "taken via cherry-pick".into(),
            url: "https://codeberg.org/o/r/pulls/8".into(),
            state: "closed".into(),
            merged: false,
            applied: true,
        });
        let md = format_markdown(&report, 2);
        assert!(md.contains("#92](https://codeberg.org/ptesarik/libkdumpfile/pulls/92) Drop removed bfd macros (closed)"));
        assert!(md.contains("#7](https://codeberg.org/o/r/pulls/7) still going\n"));
        assert!(!md.contains("still going ("));
        assert!(
            md.contains("#8](https://codeberg.org/o/r/pulls/8) taken via cherry-pick (applied)")
        );
        // The summary surfaces the applied count (one of the three).
        assert!(md.contains("- **PRs applied (landed unmerged):** 1"));
    }

    #[test]
    fn applied_count_omitted_when_zero() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.opened_prs.push(PrRef {
            repo: "o/r".into(),
            number: 1,
            title: "x".into(),
            url: "https://codeberg.org/o/r/pulls/1".into(),
            state: "open".into(),
            merged: false,
            applied: false,
        });
        let md = format_markdown(&report, 0);
        assert!(!md.contains("PRs applied"));
    }

    #[test]
    fn issues_render_opened_and_closed() {
        let mut report = ForgejoReport {
            instance: "https://codeberg.org".into(),
            user: "michelin".into(),
            ..Default::default()
        };
        report.opened_issues.push(IssueRef {
            repo: "ptesarik/libkdumpfile".into(),
            number: 91,
            title: "build fails with binutils 2.46".into(),
            url: "https://codeberg.org/ptesarik/libkdumpfile/issues/91".into(),
        });
        let md = format_markdown(&report, 2);
        assert!(md.contains("- **Issues opened:** 1"));
        // A zero line is left out entirely, rather than reported as 0.
        assert!(!md.contains("Issues closed"));
        assert!(!md.contains("PRs opened"));
        assert!(md.contains("#### Issues opened"));
        assert!(md.contains(
            "[ptesarik/libkdumpfile#91](https://codeberg.org/ptesarik/libkdumpfile/issues/91) build fails with binutils 2.46"
        ));
    }

    #[test]
    fn into_issue_ref_pulls_slug_and_url() {
        let issue: Issue = serde_json::from_str(
            r#"{"number":91,"title":"build fails","state":"closed",
                "html_url":"https://codeberg.org/o/r/issues/91",
                "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#,
        )
        .unwrap();
        let r = into_issue_ref(&issue);
        assert_eq!(r.repo, "o/r");
        assert_eq!(r.number, 91);
        assert_eq!(r.url, "https://codeberg.org/o/r/issues/91");
    }

    #[test]
    fn issues_commented_on_come_from_the_activity_feed() {
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let at = |when: &str| format!("{when}T10:00:00Z");
        let event = |op: &str, repo: &str, content: &str, when: &str| json!({"op_type": op, "repo": {"full_name": repo}, "content": content, "created": at(when)});
        rt.block_on(async {
            // The search ignores created_by: a warning, not a failure.
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/issues/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {"number": 1, "title": "t", "user": {"login": "someone"}}
                ])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/users/me/activities/feeds"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    event("comment_issue", "fesco/tickets", "3704|LGTM", "2026-11-17"),
                    event(
                        "comment_issue",
                        "fesco/tickets",
                        "[\"3704\",\"again\"]",
                        "2026-11-02"
                    ),
                    event(
                        "comment_issue",
                        "packaging/guidelines",
                        "1569|ok",
                        "2026-11-05"
                    ),
                    // An issue that cannot be read: listed without its title.
                    event("comment_issue", "hidden/repo", "5|x", "2026-11-04"),
                    event(
                        "create_issue",
                        "releng/tickets",
                        "13584|Stalled",
                        "2026-11-06"
                    ),
                    // Before the period.
                    event("comment_issue", "fesco/tickets", "3600|old", "2026-06-20")
                ])))
                .mount(&server)
                .await;
            for (repo, n, title) in [
                ("fesco/tickets", 3704, "Change: eBPF"),
                ("packaging/guidelines", 1569, "Guideline"),
            ] {
                Mock::given(method("GET"))
                    .and(path(format!("/api/v1/repos/{repo}/issues/{n}")))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_json(json!({"number": n, "title": title})),
                    )
                    .mount(&server)
                    .await;
            }
        });
        let cfg: ForgejoConfig = serde_json::from_value(json!({"instance": server.uri()})).unwrap();
        let day = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let r = forgejo_report(
            &cfg,
            "me",
            day("2026-07-01"),
            day("2026-11-20"),
            &BTreeMap::new(),
            false,
        )
        .unwrap();
        assert!(r.opened_prs.is_empty() && r.opened_issues.is_empty());
        // What the server could not be asked for is in the report.
        assert_eq!(r.not_listed.len(), 2);
        assert_eq!(
            r.not_listed,
            [
                "pull requests (searching by creator needs a token here)",
                "issues (searching by creator needs a token here)"
            ]
        );
        let commented: Vec<String> = r
            .commented_issues
            .iter()
            .map(|i| format!("{}#{} {} {}", i.repo, i.number, i.title, i.url))
            .collect();
        let base = server.uri();
        assert_eq!(
            commented,
            [
                format!("fesco/tickets#3704 Change: eBPF {base}/fesco/tickets/issues/3704"),
                format!(
                    "packaging/guidelines#1569 Guideline {base}/packaging/guidelines/issues/1569"
                ),
                format!("hidden/repo#5 (title not readable) {base}/hidden/repo/issues/5"),
            ]
        );
        let md = format_markdown(&r, 2);
        assert!(md.contains("- **Issues commented on:** 3"), "{md}");
        assert!(
            md.contains("- _Not listed: issues (searching by creator needs a token here)_"),
            "{md}"
        );
        assert!(
            md.contains("#### Issues commented on\n\n- [fesco/tickets#3704]"),
            "{md}"
        );
    }

    #[test]
    fn with_a_token_the_owners_pulls_and_issues_are_sorted_into_lists() {
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let repo = json!({"full_name": "o/r", "name": "r", "owner": "o"});
        rt.block_on(async {
            let get = |p: &str, body: serde_json::Value| {
                Mock::given(method("GET"))
                    .and(path(p.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(body))
            };
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/issues/search"))
                .and(query_param("type", "pulls"))
                .and(query_param("created", "true"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    // Opened and merged in the period.
                    {"number": 1, "title": "merged", "state": "closed", "html_url": "u1",
                     "created_at": "2026-08-01T00:00:00Z", "repository": repo,
                     "pull_request": {"merged": true, "merged_at": "2026-08-02T00:00:00Z"}},
                    // Opened in the period, declined, but its commit landed.
                    {"number": 2, "title": "applied", "state": "closed", "html_url": "u2",
                     "created_at": "2026-08-03T00:00:00Z", "repository": repo},
                    // Declined, and nothing landed.
                    {"number": 3, "title": "declined", "state": "closed", "html_url": "u3",
                     "created_at": "2026-08-04T00:00:00Z", "repository": repo}
                ])))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/issues/search"))
                .and(query_param("type", "issues"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {"number": 9, "title": "bug", "state": "closed", "html_url": "u9",
                     "created_at": "2026-07-10T00:00:00Z", "closed_at": "2026-07-12T00:00:00Z",
                     "repository": repo}
                ])))
                .mount(&server)
                .await;
            let detail = |n: u64, sha: &str| {
                json!({"number": n, "merged": false, "created_at": "2026-08-03T00:00:00Z",
                       "head": {"ref": "fix", "sha": sha}, "base": {"ref": "main", "sha": "b"}})
            };
            get("/api/v1/repos/o/r/pulls/2", detail(2, "landed"))
                .mount(&server)
                .await;
            get("/api/v1/repos/o/r/pulls/3", detail(3, "lost"))
                .mount(&server)
                .await;
            get(
                "/api/v1/repos/o/r/compare/main...landed",
                json!({"total_commits": 0}),
            )
            .mount(&server)
            .await;
            get(
                "/api/v1/repos/o/r/compare/main...lost",
                json!({"total_commits": 1}),
            )
            .mount(&server)
            .await;
            get("/api/v1/repos/o/r/pulls/3/commits", json!([]))
                .mount(&server)
                .await;
            get("/api/v1/repos/o/r/commits", json!([]))
                .mount(&server)
                .await;
            // The feed read is the token owner's, whatever the profile
            // calls them; one that cannot be read is a warning.
            get("/api/v1/user", json!({"id": 1, "login": "owner"}))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/api/v1/users/owner/activities/feeds"))
                .respond_with(ResponseTemplate::new(500))
                .expect(1)
                .mount(&server)
                .await;
        });
        let cfg: ForgejoConfig = serde_json::from_value(json!({"instance": server.uri()})).unwrap();
        let tokens = BTreeMap::from([(instance_host(&server.uri()), "tok".to_string())]);
        let day = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        let r = forgejo_report(
            &cfg,
            "",
            day("2026-07-01"),
            day("2026-08-31"),
            &tokens,
            true,
        )
        .unwrap();
        let prs = |v: &[PrRef]| -> Vec<String> {
            v.iter()
                .map(|p| format!("{}{}", p.number, p.status_marker()))
                .collect()
        };
        assert_eq!(
            prs(&r.opened_prs),
            ["1 (merged)", "2 (applied)", "3 (closed)"]
        );
        assert_eq!(prs(&r.merged_prs), ["1 (merged)"]);
        assert_eq!(r.opened_issues.len(), 1);
        assert_eq!(r.closed_issues[0].number, 9);
        assert!(r.commented_issues.is_empty());
        assert!(r.not_listed.is_empty());
        let md = format_markdown(&r, 1);
        assert!(md.contains("- **Issues closed:** 1"), "{md}");
        // No name given: the report is about the token's owner, and
        // says so.
        assert_eq!(r.user, "owner");
        // A token the server refuses is an error, not a quiet period.
        let refused = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("GET"))
                .and(path("/api/v1/repos/issues/search"))
                .respond_with(ResponseTemplate::new(401))
                .mount(&refused),
        );
        let cfg: ForgejoConfig =
            serde_json::from_value(json!({"instance": refused.uri()})).unwrap();
        let tokens = BTreeMap::from([(instance_host(&refused.uri()), "tok".to_string())]);
        let err = forgejo_report(
            &cfg,
            "me",
            day("2026-07-01"),
            day("2026-08-31"),
            &tokens,
            false,
        )
        .unwrap_err();
        assert!(
            err.starts_with("Forgejo/Gitea pull requests search on"),
            "{err}"
        );
    }

    #[test]
    fn report_partitions_opened_and_merged_by_timestamp() {
        // Built by faking the client's output via the public type:
        // we exercise the partition logic, not the HTTP layer.
        let since = NaiveDate::from_ymd_opt(2026, 6, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 6, 30).unwrap();
        let pulls = vec![
            // opened + merged in window.
            pr(r#"{"number":1,"title":"a","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/1",
                "created_at":"2026-06-10T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-06-20T00:00:00Z","draft":false}}"#),
            // opened in window, still open (not merged).
            pr(r#"{"number":2,"title":"b","state":"open",
                "html_url":"https://codeberg.org/o/r/pulls/2",
                "created_at":"2026-06-15T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"}}"#),
            // merged in window but opened earlier — merged only.
            pr(r#"{"number":3,"title":"c","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/3",
                "created_at":"2026-05-01T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-06-05T00:00:00Z","draft":false}}"#),
            // outside the window entirely.
            pr(r#"{"number":4,"title":"d","state":"closed",
                "html_url":"https://codeberg.org/o/r/pulls/4",
                "created_at":"2026-01-01T00:00:00Z",
                "repository":{"full_name":"o/r","name":"r","owner":"o"},
                "pull_request":{"merged":true,"merged_at":"2026-01-02T00:00:00Z","draft":false}}"#),
        ];
        let mut report = ForgejoReport::default();
        for p in pulls {
            if p.created_at
                .as_deref()
                .is_some_and(|d| date_in_range(d, since, until))
            {
                report.opened_prs.push(into_pr_ref(&p));
            }
            if p.is_merged()
                && p.merged_at()
                    .is_some_and(|d| date_in_range(d, since, until))
            {
                report.merged_prs.push(into_pr_ref(&p));
            }
        }
        let opened: Vec<u64> = report.opened_prs.iter().map(|p| p.number).collect();
        let merged: Vec<u64> = report.merged_prs.iter().map(|p| p.number).collect();
        assert_eq!(opened, vec![1, 2]);
        assert_eq!(merged, vec![1, 3]);
    }
}
