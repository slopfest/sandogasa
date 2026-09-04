// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `forge` — what a contributor did on Forgejo, per repository, from
//! their public activity feed; `fesco/tickets` is the tracker this was
//! built for.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};
use sandogasa_forgejo::{Activity, Client};
use serde::Serialize;

use crate::ServiceLastSeen;

/// Fedora's Forgejo.
pub const DEFAULT_URL: &str = "https://forge.fedoraproject.org";

/// One thing the user did.
#[derive(Debug, Clone, Serialize)]
pub struct Item {
    /// RFC 3339.
    pub when: String,
    /// `opened`, `commented`, `closed`, … (see [`action_label`]).
    pub action: String,
    pub repo: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    /// Issue title, or the comment's text.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// A repository's share of the activity.
#[derive(Debug, Clone, Serialize)]
pub struct RepoActivity {
    pub repo: String,
    /// Action → how many times.
    pub counts: BTreeMap<String, u32>,
    pub last_active: String,
    pub items: Vec<Item>,
}

/// The `forge` report.
#[derive(Debug, Serialize)]
pub struct Report {
    pub username: String,
    pub days: u32,
    pub since: String,
    pub repos: Vec<RepoActivity>,
}

/// A human word for an activity-feed `op_type`.
pub fn action_label(op_type: &str) -> String {
    match op_type {
        "create_issue" => "opened".to_string(),
        "comment_issue" => "commented".to_string(),
        "close_issue" => "closed".to_string(),
        "reopen_issue" => "reopened".to_string(),
        "create_pull_request" => "PR opened".to_string(),
        "merge_pull_request" => "PR merged".to_string(),
        "close_pull_request" => "PR closed".to_string(),
        "approve_pull_request" => "PR approved".to_string(),
        "reject_pull_request" => "PR changes requested".to_string(),
        "comment_pull" => "PR commented".to_string(),
        "commit_repo" => "pushed".to_string(),
        "create_repo" => "created repo".to_string(),
        other => other.replace('_', " "),
    }
}

/// One activity as an [`Item`].
fn item(a: &Activity) -> Item {
    let repo = a.repo_slug().unwrap_or("?").to_string();
    let (number, text) = match a.issue_ref() {
        Some((n, t)) => (Some(n), t),
        None => (None, String::new()),
    };
    let url = a
        .comment
        .as_ref()
        .and_then(|c| c.html_url.clone())
        .or_else(|| {
            let base = a.repo.as_ref()?.html_url.clone()?;
            number.map(|n| format!("{base}/issues/{n}"))
        });
    Item {
        when: a.created.clone(),
        action: action_label(&a.op_type),
        repo,
        number,
        text: text.lines().next().unwrap_or("").chars().take(80).collect(),
        url,
    }
}

/// Group activities by repository, newest first within each; `repos`
/// narrows to those slugs (empty: all).
pub fn summarize(activities: &[Activity], repos: &[String]) -> Vec<RepoActivity> {
    let mut by_repo: BTreeMap<String, RepoActivity> = BTreeMap::new();
    for a in activities {
        let Some(slug) = a.repo_slug() else {
            continue;
        };
        if !repos.is_empty() && !repos.iter().any(|r| r == slug) {
            continue;
        }
        let it = item(a);
        let entry = by_repo
            .entry(slug.to_string())
            .or_insert_with(|| RepoActivity {
                repo: slug.to_string(),
                counts: BTreeMap::new(),
                last_active: it.when.clone(),
                items: Vec::new(),
            });
        *entry.counts.entry(it.action.clone()).or_default() += 1;
        if it.when > entry.last_active {
            entry.last_active = it.when.clone();
        }
        entry.items.push(it);
    }
    let mut out: Vec<RepoActivity> = by_repo.into_values().collect();
    for r in &mut out {
        r.items.sort_by(|a, b| b.when.cmp(&a.when));
    }
    out.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    out
}

/// The user's activity since `since` (blocking).
fn fetch(username: &str, url: &str, since: Option<DateTime<Utc>>) -> Result<Vec<Activity>, String> {
    let client = Client::anonymous(url).map_err(|e| e.to_string())?;
    client
        .user_activity(username, since.map(|s| s.to_rfc3339()).as_deref())
        .map_err(|e| e.to_string())
}

fn counts_line(counts: &BTreeMap<String, u32>) -> String {
    counts
        .iter()
        .map(|(k, v)| format!("{v} {k}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The `forge` subcommand.
pub async fn cmd_forge(
    username: &str,
    repos: &[String],
    days: u32,
    url: &str,
    json: bool,
    now: DateTime<Utc>,
) -> Result<(), Box<dyn std::error::Error>> {
    let since = now - Duration::days(i64::from(days));
    let (u, base) = (username.to_string(), url.to_string());
    let activities = tokio::task::spawn_blocking(move || fetch(&u, &base, Some(since))).await??;
    let report = Report {
        username: username.to_string(),
        days,
        since: since.to_rfc3339(),
        repos: summarize(&activities, repos),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Forge: {username} (last {days} days)");
    if report.repos.is_empty() {
        println!(
            "\n  No activity{}.",
            if repos.is_empty() {
                ""
            } else {
                " in the given repositories"
            }
        );
        return Ok(());
    }
    // Every item when the user named repositories; a per-repo summary otherwise.
    for r in &report.repos {
        println!(
            "\n  {:<32} {} — last {}",
            r.repo,
            counts_line(&r.counts),
            &r.last_active[..10]
        );
        if !repos.is_empty() {
            for it in &r.items {
                let num = it.number.map(|n| format!("#{n}")).unwrap_or_default();
                println!(
                    "    {}  {:<12} {:<6} {}",
                    &it.when[..10],
                    it.action,
                    num,
                    it.text
                );
            }
        }
    }
    Ok(())
}

/// The newest thing the user did on Forgejo, for `last-seen`.
pub async fn check_forge(username: &str, url: &str) -> ServiceLastSeen {
    let (u, base) = (username.to_string(), url.to_string());
    let joined = tokio::task::spawn_blocking(move || {
        let client = Client::anonymous(&base).map_err(|e| e.to_string())?;
        // Newest first: the first event is the answer.
        client
            .user_activity(&u, None)
            .map(|mut v| {
                v.truncate(1);
                v
            })
            .map_err(|e| e.to_string())
    })
    .await;
    let result: Result<Vec<Activity>, String> = match joined {
        Ok(r) => r,
        Err(e) => Err(e.to_string()),
    };
    let service = "Forge".to_string();
    match result {
        Ok(v) => match v.first() {
            Some(a) => {
                let it = item(a);
                let detail = match it.number {
                    Some(n) => format!("{} {}#{n}", it.action, it.repo),
                    None => format!("{} {}", it.action, it.repo),
                };
                ServiceLastSeen {
                    service,
                    last_active: DateTime::parse_from_rfc3339(&a.created)
                        .map(|d| d.with_timezone(&Utc).to_rfc3339())
                        .ok(),
                    detail: Some(detail),
                    ..Default::default()
                }
            }
            None => ServiceLastSeen {
                service,
                detail: Some("no activity".to_string()),
                ..Default::default()
            },
        },
        Err(e) => ServiceLastSeen {
            service,
            error: Some(e),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn activity(op: &str, repo: &str, created: &str, content: &str) -> Activity {
        serde_json::from_value(serde_json::json!({
            "op_type": op, "created": created, "content": content,
            "repo": {"full_name": repo, "html_url": format!("https://forge/{repo}")}
        }))
        .unwrap()
    }

    #[test]
    fn summarize_groups_by_repo_and_counts_actions() {
        let acts = vec![
            activity(
                "comment_issue",
                "fesco/tickets",
                "2026-09-03T22:15:00Z",
                "[\"3677\",\"+1\"]",
            ),
            activity(
                "create_issue",
                "fesco/tickets",
                "2026-07-08T10:00:00Z",
                "[\"3635\",\"RFE\"]",
            ),
            activity(
                "create_issue",
                "releng/fedora-scm-requests",
                "2026-09-03T19:37:00Z",
                "[\"700\",\"New Branch\"]",
            ),
            activity("commit_repo", "salimma/x", "2026-09-01T00:00:00Z", ""),
        ];
        let all = summarize(&acts, &[]);
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].repo, "fesco/tickets"); // newest last_active first
        assert_eq!(all[0].counts["commented"], 1);
        assert_eq!(all[0].counts["opened"], 1);
        assert_eq!(all[0].items[0].number, Some(3677));
        assert_eq!(
            all[0].items[0].url.as_deref(),
            Some("https://forge/fesco/tickets/issues/3677")
        );
        let only = summarize(&acts, &["salimma/x".to_string()]);
        assert_eq!(only.len(), 1);
        assert_eq!(only[0].items[0].action, "pushed");
        assert_eq!(only[0].items[0].number, None);
    }
}
