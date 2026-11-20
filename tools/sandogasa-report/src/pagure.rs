// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pagure activity reporting: the pull requests a user filed on a Pagure
//! instance, such as Fedora's dist-git (src.fedoraproject.org), opened or
//! merged in the window. Pagure answers these anonymously, so no token is
//! needed; the user is their login there (the FAS name on Fedora's,
//! unless the profile names another for the instance).
//!
//! Pagure lists a user's pull requests newest first by when they were
//! opened, so one read back to the window's start gives those opened in
//! it, and a second, of the merged ones, reaches [`MERGED_LOOKBACK_DAYS`]
//! further back for those opened earlier and merged in it.

use chrono::{DateTime, NaiveDate};
use sandogasa_distgit::{DistGitClient, PullRequest};
use serde::{Deserialize, Serialize};

use crate::forge::{self, stat};

/// Per-domain Pagure settings.
#[derive(Debug, Default, Deserialize)]
pub struct PagureConfig {
    /// The instance's root URL (`https://src.fedoraproject.org`).
    pub instance: String,
}

/// A user's Pagure activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct PagureReport {
    /// Instance root URL the report was fetched from.
    pub instance: String,
    /// Login reported on.
    pub user: String,
    /// Pull requests the user opened in the window.
    pub opened_prs: Vec<PrRef>,
    /// Pull requests by the user merged in the window.
    pub merged_prs: Vec<PrRef>,
}

/// Pointer to a pull request for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct PrRef {
    /// The project (`rpms/claws-mail`).
    pub repo: String,
    /// The pull request's number in its project.
    pub number: u64,
    pub title: String,
    pub url: String,
    /// `Open`, `Merged` or `Closed`, as Pagure says it.
    pub status: String,
}

impl PrRef {
    /// A state marker for the opened list: empty for an open one.
    fn status_marker(&self) -> &'static str {
        match self.status.as_str() {
            "Merged" => " (merged)",
            "Closed" => " (closed)",
            _ => "",
        }
    }
}

/// The host part of an instance URL.
pub(crate) use crate::forge::instance_host;

/// How long before the window a pull request merged in it may have been
/// opened and still be found.
pub const MERGED_LOOKBACK_DAYS: i64 = 180;

/// Build the Pagure report for one domain: the pull requests `user`
/// filed, sorted into those opened and those merged in the window.
pub fn pagure_report(
    cfg: &PagureConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    rt: &tokio::runtime::Runtime,
    verbose: bool,
) -> Result<PagureReport, String> {
    let base = cfg.instance.trim_end_matches('/');
    eprintln!("Reading the pull requests {user} filed on {base}...");
    let client = DistGitClient::with_base_url(base);
    let at = |day: NaiveDate| {
        day.and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc()
            .timestamp()
    };
    let read = |status: &str, from: NaiveDate| {
        if verbose {
            eprintln!("[pagure] {base}: {status} pull requests by {user} opened since {from}");
        }
        rt.block_on(client.user_pull_requests_since(user, status, at(from)))
            .map_err(|e| format!("Pagure pull requests of {user} on {base}: {e}"))
    };
    let opened = read("all", since)?;
    let merged = read(
        "Merged",
        since - chrono::Duration::days(MERGED_LOOKBACK_DAYS),
    )?;
    Ok(sort(base, user, opened, merged, since, until))
}

/// The report's lists for the window: of `opened`, those opened in it,
/// and of `merged`, those merged in it.
fn sort(
    base: &str,
    user: &str,
    opened: Vec<PullRequest>,
    merged: Vec<PullRequest>,
    since: NaiveDate,
    until: NaiveDate,
) -> PagureReport {
    let in_window = |stamp: Option<&str>| {
        stamp
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|t| DateTime::from_timestamp(t, 0))
            .is_some_and(|t| (since..=until).contains(&t.date_naive()))
    };
    let mut report = PagureReport {
        instance: base.to_string(),
        user: user.to_string(),
        ..Default::default()
    };
    let pr_ref = |pr: &PullRequest| {
        let repo = pr
            .project
            .as_ref()
            .map(|p| p.fullname.clone())
            .unwrap_or_default();
        PrRef {
            url: pr
                .full_url
                .clone()
                .unwrap_or_else(|| format!("{base}/{repo}/pull-request/{}", pr.id)),
            repo,
            number: pr.id,
            title: pr.title.clone(),
            status: pr.status.clone(),
        }
    };
    report.opened_prs = opened
        .iter()
        .filter(|pr| in_window(pr.date_created.as_deref()))
        .map(pr_ref)
        .collect();
    report.merged_prs = merged
        .iter()
        .filter(|pr| pr.status == "Merged" && in_window(pr.closed_at.as_deref()))
        .map(pr_ref)
        .collect();
    report
}

/// Format the Pagure section as Markdown.
pub fn format_markdown(report: &PagureReport, detail: u8) -> String {
    let heading = format!("### Pagure ({})\n\n", instance_host(&report.instance));
    let mut stats = String::new();
    stat(&mut stats, "PRs opened", report.opened_prs.len());
    stat(&mut stats, "PRs merged", report.merged_prs.len());
    if stats.is_empty() {
        return format!("{heading}No Pagure activity.\n\n");
    }
    let mut out = heading;
    out.push_str(&stats);
    out.push('\n');
    if detail == 0 {
        return out;
    }
    let deep = detail >= 2;
    let prs = |out: &mut String, heading: &str, prs: &[PrRef], with_status: bool| {
        if prs.is_empty() {
            return;
        }
        out.push_str(&format!("#### {heading}\n\n"));
        if deep {
            for pr in prs {
                let marker = if with_status { pr.status_marker() } else { "" };
                out.push_str(&format!(
                    "- [{}#{}]({}) {}{marker}\n",
                    pr.repo, pr.number, pr.url, pr.title
                ));
            }
            out.push('\n');
        } else {
            forge::write_counts_by(out, prs, |p| p.repo.as_str());
        }
    };
    prs(&mut out, "PRs opened", &report.opened_prs, true);
    prs(&mut out, "PRs merged", &report.merged_prs, false);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filed(id: u64, repo: &str, status: &str, created: i64, closed: Option<i64>) -> PullRequest {
        serde_json::from_value(serde_json::json!({
            "id": id, "title": format!("pr {id}"), "status": status,
            "date_created": created.to_string(),
            "closed_at": closed.map(|c| c.to_string()),
            "project": {"fullname": repo},
            "full_url": format!("https://src.example/{repo}/pull-request/{id}")
        }))
        .unwrap()
    }

    // 2026-08-01T00:00:00Z and a day.
    const AUG1: i64 = 1_785_542_400;
    const DAY: i64 = 86_400;

    #[test]
    fn filed_requests_sort_into_opened_and_merged() {
        let since = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let r = sort(
            "https://src.example",
            "me",
            vec![
                filed(11, "rpms/a", "Merged", AUG1 + DAY, Some(AUG1 + 2 * DAY)),
                filed(9, "rpms/b", "Closed", AUG1 + 3 * DAY, Some(AUG1 + 4 * DAY)),
                filed(3, "rpms/c", "Open", AUG1 + 5 * DAY, None),
            ],
            vec![
                filed(11, "rpms/a", "Merged", AUG1 + DAY, Some(AUG1 + 2 * DAY)),
                // Opened before, merged in the window: merged only.
                filed(2, "rpms/d", "Merged", AUG1 - 40 * DAY, Some(AUG1 + DAY)),
                // Merged before the window.
                filed(1, "rpms/e", "Merged", AUG1 - 50 * DAY, Some(AUG1 - DAY)),
            ],
            since,
            until,
        );
        let ids = |v: &[PrRef]| v.iter().map(|p| p.number).collect::<Vec<_>>();
        assert_eq!(ids(&r.opened_prs), [11, 9, 3]);
        assert_eq!(ids(&r.merged_prs), [11, 2]);
        let md = format_markdown(&r, 2);
        assert!(
            md.starts_with(
                "### Pagure (src.example)\n\n- **PRs opened:** 3\n- **PRs merged:** 2\n"
            ),
            "{md}"
        );
        assert!(
            md.contains("- [rpms/b#9](https://src.example/rpms/b/pull-request/9) pr 9 (closed)\n"),
            "{md}"
        );
        assert!(md.contains("#### PRs merged\n\n- [rpms/a#11]"), "{md}");
        assert!(format_markdown(&r, 1).contains("#### PRs opened\n\n"));
        let empty = PagureReport::default();
        assert!(format_markdown(&empty, 2).contains("No Pagure activity."));
    }

    #[test]
    fn a_users_requests_are_read_back_to_the_start_of_the_window() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let page = |n: u32, requests: serde_json::Value| {
            Mock::given(method("GET"))
                .and(path("/api/0/user/me/requests/filed"))
                .and(query_param("status", "all"))
                .and(query_param("page", n.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "requests": requests, "total_requests": 3,
                    "pagination": {"pages": 3, "per_page": 100}
                })))
        };
        let pr = |id: u64, created: i64| {
            serde_json::json!({"id": id, "title": "t", "status": "Open",
                "date_created": created.to_string(), "project": {"fullname": "rpms/x"}})
        };
        rt.block_on(async {
            page(1, serde_json::json!([pr(3, AUG1 + 2 * DAY)]))
                .mount(&server)
                .await;
            // Reaches back before the window: the read stops here.
            page(2, serde_json::json!([pr(2, AUG1 + DAY), pr(1, AUG1 - DAY)]))
                .mount(&server)
                .await;
            page(3, serde_json::json!([]))
                .expect(0)
                .mount(&server)
                .await;
            // The merged ones, from further back: one opened 40 days
            // before the window and merged in it.
            Mock::given(method("GET"))
                .and(path("/api/0/user/me/requests/filed"))
                .and(query_param("status", "Merged"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "requests": [{"id": 7, "title": "late", "status": "Merged",
                        "date_created": (AUG1 - 40 * DAY).to_string(),
                        "closed_at": (AUG1 + 3 * DAY).to_string(),
                        "project": {"fullname": "rpms/y"}}],
                    "pagination": {"pages": 1, "per_page": 100}
                })))
                .expect(1)
                .mount(&server)
                .await;
        });
        let cfg = PagureConfig {
            instance: server.uri(),
        };
        let since = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let r = pagure_report(&cfg, "me", since, until, &rt, true).unwrap();
        let ids: Vec<u64> = r.opened_prs.iter().map(|p| p.number).collect();
        assert_eq!(ids, [3, 2]);
        let merged: Vec<u64> = r.merged_prs.iter().map(|p| p.number).collect();
        assert_eq!(merged, [7]);
        assert_eq!(
            r.opened_prs[0].url,
            format!("{}/rpms/x/pull-request/3", server.uri())
        );
    }
}
