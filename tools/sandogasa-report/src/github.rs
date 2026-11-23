// SPDX-License-Identifier: Apache-2.0 OR MIT

//! GitHub activity reporting — PRs authored/merged, reviews,
//! comments, and authored commit counts. Sourced from a
//! combination of the Search Issues API (for PRs in a date
//! window) and `/users/{login}/events` (to discover which repos
//! the user touched, then per-repo authored commits).
//!
//! Structured to mirror `crate::gitlab` so a multi-domain report
//! renders each forge the same way and the merge into
//! `Report.github` plugs into `report::format_markdown` with no
//! special-casing.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use sandogasa_github::{Client, Event, ItemNode, PullRequest, PullRequestDetail, RepoCommit, User};
use serde::Serialize;

use crate::config::GithubConfig;
pub(crate) use crate::forge::instance_host;
use crate::forge::{self, TokenSpec, date_in_range, stat, stat_across};

const TOKEN_SPEC: TokenSpec = TokenSpec {
    service: "GitHub",
    label: "sandogasa: GitHub token",
    generic_env: "GITHUB_TOKEN",
    host_separators: &['.'],
    hint: "",
};

/// A user's GitHub activity for a single domain.
#[derive(Debug, Default, Serialize)]
pub struct GithubReport {
    /// API base URL the report was fetched from.
    pub instance: String,
    /// GitHub login that was queried (may differ from the
    /// profile's FAS login).
    pub user: String,
    /// Org/user-namespace filter, if set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub org: Option<String>,

    /// PRs opened by the user in the window.
    pub opened_prs: Vec<PrRef>,
    /// PRs by the user that were merged in the window.
    pub merged_prs: Vec<PrRef>,
    /// PRs the user formally reviewed (any review state).
    /// GitHub Search doesn't expose an "approved-only" filter,
    /// so this is "had at least one review by the user in the
    /// window" — broader than GitLab's `approved_mrs`.
    pub reviewed_prs: Vec<PrRef>,
    /// PRs the user commented on in the window.
    pub commented_prs: Vec<PrRef>,
    /// Issues the user opened in the window.
    pub opened_issues: Vec<IssueRef>,
    /// Issues the user commented on in the window.
    pub commented_issues: Vec<IssueRef>,

    /// Per-repo count of commits authored by the user. Computed
    /// only for repos the user pushed to (discovered via the
    /// events endpoint).
    pub commits_authored: BTreeMap<String, u64>,

    /// Annotated tags the user cut in the window. Derived by
    /// walking each touched repo's tag refs and resolving
    /// annotated-tag objects — `git push --follow-tags` folds
    /// tag creation into the PushEvent so the events stream
    /// alone can't tell us about new tags. Lightweight tags
    /// aren't included since they carry no tagger metadata.
    pub tags_pushed: Vec<TagRef>,
    /// GitHub Releases the user published in the window. Derived
    /// from `ReleaseEvent` with `action == "published"`.
    pub releases_published: Vec<ReleaseRef>,
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
    /// Current state (`open` / `closed`) where the source knows it; the
    /// event-derived lists leave it empty. A merged PR is `closed` with
    /// `merged = true`; a declined PR is `closed` with `merged = false`.
    #[serde(skip_serializing_if = "String::is_empty")]
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
    /// A state marker for the **opened** list, so a PR that was opened
    /// in the window but has since landed or been declined isn't shown
    /// as if it were still open. Empty for an open PR.
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

/// Pointer to an issue for the report's summary lists.
#[derive(Debug, Clone, Serialize)]
pub struct IssueRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Issue number.
    pub number: u64,
    /// Issue title.
    pub title: String,
    /// Issue URL (the canonical web link).
    pub url: String,
    /// Current state (`open` / `closed`) where the source knows it; the
    /// event-derived ones leave it empty.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub state: String,
}

/// A tag the user pushed.
#[derive(Debug, Clone, Serialize)]
pub struct TagRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Tag name (e.g. `v0.11.0`).
    pub tag: String,
    /// Link to the tag tree on github.com.
    pub url: String,
}

/// A GitHub Release the user published.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseRef {
    /// `owner/name` slug.
    pub repo: String,
    /// Tag the release is anchored on.
    pub tag: String,
    /// Optional release title (`release.name`). Empty/missing
    /// becomes `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `release.html_url`.
    pub url: String,
    /// `release.prerelease`.
    pub prerelease: bool,
}

/// Build the GitHub activity report for one domain.
pub fn github_report(
    cfg: &GithubConfig,
    user: &str,
    since: NaiveDate,
    until: NaiveDate,
    tokens: &BTreeMap<String, String>,
    include_private: bool,
    verbose: bool,
) -> Result<GithubReport, String> {
    let token = find_token(&cfg.instance, tokens)?;
    let base = cfg.instance.trim_end_matches('/');
    let client =
        Client::new(base, &token).map_err(|e| format!("GitHub client setup on {base}: {e}"))?;

    if verbose {
        eprintln!("[github] {base}: resolving user {user}");
    }
    let user_obj = client
        .user_by_username(user)
        .map_err(|e| format!("GitHub user lookup on {base}: {e}"))?
        .ok_or_else(|| format!("user '{user}' not found on {base}"))?;

    let mut report = GithubReport {
        instance: base.to_string(),
        user: user_obj.login.clone(),
        org: cfg.org.clone(),
        ..Default::default()
    };

    let org_clause = cfg
        .org
        .as_deref()
        .map(|o| format!(" org:{o}"))
        .unwrap_or_default();
    let vis_clause = visibility_clause(include_private);
    let login = &user_obj.login;

    // PRs opened in window.
    let q = format!("type:pr author:{login} created:{since}..{until}{org_clause}{vis_clause}",);
    report.opened_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // PRs by the user that were merged in window.
    let q = format!(
        "type:pr author:{login} is:merged merged:{since}..{until}{org_clause}{vis_clause}",
    );
    report.merged_prs = run_pr_search(&client, &q, verbose)?
        .into_iter()
        .map(into_pr_ref)
        .collect();

    // Issues opened in window: the same search, for issues.
    let q = format!("type:issue author:{login} created:{since}..{until}{org_clause}{vis_clause}",);
    // A failure leaves the list empty, with a warning: the pull
    // requests are kept.
    report.opened_issues = match run_search(&client, "issue", &q, verbose) {
        Ok(found) => found.into_iter().map(into_issue_ref).collect(),
        Err(e) => {
            eprintln!("warning: {e}; issues opened are left out");
            Vec::new()
        }
    };

    // Reviewed/commented PRs come from the user's own actions, not
    // Search. GitHub Search's `reviewed-by:`/`commenter:` qualifiers
    // match "user has done this at any time" and the temporal filters
    // (`updated:`, `created:`, etc.) apply to the PR itself, not the
    // user's action — so a 2021 PR with any modification in our
    // window (e.g. a comment from someone else) would surface as
    // "reviewed in window". The events stream says what the user did
    // and when, but holds only their newest 300 events (#181), so
    // GraphQL's contributions and comment list fill in the rest.
    if verbose {
        eprintln!("[github] {base}: fetching user events");
    }
    let events = client
        .user_events(login)
        .map_err(|e| format!("GitHub events on {base}: {e}"))?;
    let events = if include_private {
        events
    } else {
        // Public then and public now, as the PR search's is:public
        // sees it: a repository made private since stays out.
        let repos: BTreeSet<String> = events
            .iter()
            .filter(|e| e.public)
            .filter_map(|e| e.repo_slug().map(String::from))
            .collect();
        if verbose {
            eprintln!(
                "[github] {base}: checking which of {} repo(s) are public",
                repos.len()
            );
        }
        let public = public_now(&client, &repos);
        let total = events.len();
        let kept = shareable_events(events, &public);
        if kept.len() < total {
            eprintln!(
                "GitHub ({base}): left out {} event(s) in private repositories; \
                 --private includes them.",
                total - kept.len()
            );
        }
        kept
    };
    let org = cfg.org.as_deref();
    if verbose {
        eprintln!("[github] {base}: reading contributions and comments since {since}");
    }
    // A failed read is a warning: the events still give what they
    // reach, and the searches above are kept.
    let contrib = client
        .contributions(login, since, until)
        .unwrap_or_else(|e| {
            eprintln!(
                "warning: cannot read GitHub contributions on {base} ({e}); commits and \
             reviews come from the recent events alone"
            );
            Default::default()
        });
    let comments = client.issue_comments(login, since).unwrap_or_else(|e| {
        eprintln!(
            "warning: cannot read GitHub comments on {base} ({e}); they come from the \
             recent events alone"
        );
        Default::default()
    });
    if contrib.capped {
        eprintln!(
            "warning: GitHub ({base}) names at most 100 repositories a day, and a day \
             reached it; commits in others are missing"
        );
    }
    for (what, unreadable) in [
        ("review", &contrib.unreadable),
        ("comment", &comments.unreadable),
    ] {
        if unreadable.count > 0 {
            eprintln!(
                "warning: GitHub ({base}) withheld {} {what}(s) this token cannot read, and \
                 commits in the same organizations go uncounted: {}",
                unreadable.count,
                unreadable
                    .reasons
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
    }
    if contrib.restricted > 0 {
        eprintln!(
            "note: GitHub ({base}) counts {} contribution(s) in private repositories \
             without naming them{}",
            contrib.restricted,
            if include_private {
                "; the ones the token can read that were pushed to in the period are \
                 counted here"
            } else {
                "; left out without --private"
            }
        );
    }
    // In the org, if one is set, and public unless --private.
    let shown = |item: &ItemNode| {
        repo_in_org(&item.repository.name_with_owner, org)
            && (include_private || !item.repository.is_private)
    };
    let (reviewed, commented, commented_issues) = prs_from_events(&events, since, until, org);
    report.reviewed_prs = merge_prs(
        contrib
            .reviewed
            .iter()
            .filter(|pr| shown(pr))
            .map(item_pr_ref),
        reviewed,
    );
    let in_period: Vec<&sandogasa_github::IssueComment> = comments
        .comments
        .iter()
        .filter(|c| date_in_range(&c.created_at, since, until))
        .collect();
    report.commented_prs = merge_prs(
        in_period
            .iter()
            .filter_map(|c| c.pull_request.as_ref())
            .filter(|pr| shown(pr))
            .map(item_pr_ref),
        commented,
    );
    // A comment on a pull request names it as an issue too: only those
    // without one are on issues.
    report.commented_issues = merge_issues(
        in_period
            .iter()
            .filter(|c| c.pull_request.is_none())
            .filter_map(|c| c.issue.as_ref())
            .filter(|issue| shown(issue))
            .map(item_issue_ref),
        commented_issues,
    );
    report.releases_published = releases_from_events(&events, since, until, org);

    // Commits: GitHub's contributions count them per repository, but
    // leave out forks, and name no private repository the token cannot
    // see there (`restricted`). So repos pushed to in the events, and
    // with --private the private ones pushed to since `since`, are
    // counted separately.
    let counted: BTreeMap<String, u64> = contrib
        .commits
        .iter()
        .filter(|(slug, _)| repo_in_org(slug, org))
        .filter(|(slug, _)| include_private || !contrib.private_repos.contains(*slug))
        .map(|(slug, n)| (slug.clone(), *n))
        .collect();
    let mut by_hand: BTreeSet<String> = events
        .iter()
        .filter(|e| e.event_type == "PushEvent")
        .filter(|e| event_in_range(e, since, until))
        .filter_map(|e| e.repo_slug().map(String::from))
        .filter(|slug| repo_in_org(slug, org))
        .collect();
    if include_private {
        by_hand.extend(
            private_repos(&client, base, login, since, verbose)
                .into_iter()
                .filter(|slug| repo_in_org(slug, org)),
        );
    }
    by_hand.retain(|slug| !counted.contains_key(slug));
    if verbose {
        eprintln!(
            "[github] {base}: counting authored commits in {} more repo(s)",
            by_hand.len()
        );
    }
    report.commits_authored = counted;
    let repos: Vec<String> = by_hand.iter().cloned().collect();
    match client.commit_counts(login, &repos, since, until) {
        Ok(counts) => {
            for failure in &counts.failures {
                eprintln!("warning: cannot count GitHub commits in {failure}");
            }
            report.commits_authored.extend(counts.counts);
        }
        Err(e) => eprintln!(
            "warning: cannot count GitHub commits in {} repo(s) on {base} ({e})",
            repos.len()
        ),
    }

    // Tags can't be derived from the user-events stream: a
    // `git push --follow-tags` folds the tag creation into the
    // existing PushEvent (which lists only the branch ref). Read each
    // repo's newest annotated tags instead, with tagger date and
    // identity.
    let tag_repos: BTreeSet<String> = report
        .commits_authored
        .keys()
        .cloned()
        .chain(by_hand)
        .collect();
    if verbose {
        eprintln!(
            "[github] {base}: scanning tags across {} repo(s)",
            tag_repos.len()
        );
    }
    report.tags_pushed = collect_tags(&client, &tag_repos, &user_obj, since, until);
    mark_applied(&client, &mut report, verbose);

    Ok(report)
}

/// Mark each closed-unmerged PR of the report whose work landed anyway
/// (applied out of band, [`applied_out_of_band`]), in every list it is
/// in: checked once per PR, so one opened and commented on reads the
/// same in both (#230). An entry from the events stream has no state
/// of its own and takes what another list knows of the same PR. The
/// checks run a few at a time, as each is two to four requests.
fn mark_applied(client: &Client, report: &mut GithubReport, verbose: bool) {
    const AT_ONCE: usize = 4;
    let key = |p: &PrRef| (p.repo.clone(), p.number);
    // What each PR is known to be, from the entries that know it.
    let mut known: BTreeMap<(String, u64), (String, bool)> = BTreeMap::new();
    let mut todo: Vec<PrRef> = Vec::new();
    for pr in report
        .opened_prs
        .iter()
        .chain(&report.reviewed_prs)
        .chain(&report.commented_prs)
        .filter(|p| !p.state.is_empty())
    {
        if known
            .insert(key(pr), (pr.state.clone(), pr.merged))
            .is_none()
            && pr.state == "closed"
            && !pr.merged
        {
            todo.push(pr.clone());
        }
    }
    let mut applied: BTreeMap<(String, u64), bool> = BTreeMap::new();
    if !todo.is_empty() {
        eprintln!(
            "Checking {} closed pull request(s) for commits applied without merging...",
            todo.len()
        );
    }
    for batch in todo.chunks(AT_ONCE) {
        let answers: Vec<bool> = std::thread::scope(|s| {
            let handles: Vec<_> = batch
                .iter()
                .map(|pr| s.spawn(move || applied_out_of_band(client, pr, verbose)))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or(false))
                .collect()
        });
        for (pr, answer) in batch.iter().zip(answers) {
            applied.insert(key(pr), answer);
        }
    }
    for pr in report
        .opened_prs
        .iter_mut()
        .chain(&mut report.reviewed_prs)
        .chain(&mut report.commented_prs)
    {
        if pr.state.is_empty()
            && let Some((state, merged)) = known.get(&key(pr))
        {
            pr.state = state.clone();
            pr.merged = *merged;
        }
        if let Some(&answer) = applied.get(&key(pr)) {
            pr.applied = answer;
        }
    }
}

/// Format the GitHub section as Markdown. `heading_suffix` is
/// `Some("<domain>")` for multi-domain runs, `None` otherwise.
pub fn format_markdown(report: &GithubReport, detail: u8) -> String {
    let detailed = detail >= 1;
    let heading = "### GitHub\n\n".to_string();

    let total_authored: u64 = report.commits_authored.values().sum();
    let authored_repos = report.commits_authored.len();
    let tag_repos = unique_repos(&report.tags_pushed, |t| &t.repo);
    let release_repos = unique_repos(&report.releases_published, |r| &r.repo);

    // See the note in `gitlab::format_markdown`: the block decides
    // for itself whether anything happened.
    let mut stats = String::new();
    stat(&mut stats, "PRs opened", report.opened_prs.len());
    stat(&mut stats, "PRs merged", report.merged_prs.len());
    // A closed PR whose commit landed out-of-band — counted separately
    // since the forge reports it as neither merged nor open.
    stat(
        &mut stats,
        "PRs applied (landed unmerged)",
        report.opened_prs.iter().filter(|p| p.applied).count(),
    );
    stat(&mut stats, "PRs reviewed", report.reviewed_prs.len());
    stat(&mut stats, "PRs commented on", report.commented_prs.len());
    stat(&mut stats, "Issues opened", report.opened_issues.len());
    stat(
        &mut stats,
        "Issues commented on",
        report.commented_issues.len(),
    );
    stat_across(
        &mut stats,
        "Commits authored",
        total_authored,
        authored_repos,
        "repo",
    );
    stat_across(
        &mut stats,
        "Tags pushed",
        report.tags_pushed.len() as u64,
        tag_repos,
        "repo",
    );
    stat_across(
        &mut stats,
        "Releases published",
        report.releases_published.len() as u64,
        release_repos,
        "repo",
    );

    if stats.is_empty() {
        return format!("{heading}No GitHub activity.\n\n");
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
    let prs = |out: &mut String, heading: &str, prs: &[PrRef]| {
        if prs.is_empty() {
            return;
        }
        out.push_str(&format!("#### {heading}\n\n"));
        if deep {
            write_pr_list(out, prs);
        } else {
            forge::write_counts_by(out, prs, |p| p.repo.as_str());
        }
    };
    prs(&mut out, "Opened", &report.opened_prs);
    prs(&mut out, "Merged", &report.merged_prs);
    prs(&mut out, "Reviewed", &report.reviewed_prs);
    prs(&mut out, "Commented on", &report.commented_prs);
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
    issues(&mut out, "Issues opened", &report.opened_issues);
    issues(&mut out, "Issues commented on", &report.commented_issues);
    if !report.commits_authored.is_empty() {
        out.push_str("#### Commits by repo\n\n");
        for (repo, count) in &report.commits_authored {
            out.push_str(&format!("- `{repo}`: {count}\n"));
        }
        out.push('\n');
    }
    if !report.tags_pushed.is_empty() {
        out.push_str("#### Tags pushed\n\n");
        if deep {
            write_tag_list(&mut out, &report.tags_pushed);
        } else {
            forge::write_counts_by(&mut out, &report.tags_pushed, |t| t.repo.as_str());
        }
    }
    if !report.releases_published.is_empty() {
        out.push_str("#### Releases published\n\n");
        if deep {
            write_release_list(&mut out, &report.releases_published);
        } else {
            forge::write_counts_by(&mut out, &report.releases_published, |r| r.repo.as_str());
        }
    }
    out
}

/// Walk the user's events and bucket PRs into `(reviewed,
/// commented)`. Only events whose `created_at` falls inside the
/// reporting window count, regardless of when the PR itself was
/// last touched.
///
/// - `PullRequestReviewEvent` → reviewed.
/// - `PullRequestReviewCommentEvent` → commented (inline diff
///   comment on a PR).
/// - `IssueCommentEvent` → commented, but only when the issue is
///   actually a PR (payload carries `issue.pull_request`).
///
/// Dedup is by `(repo, number)` so multiple events on the same
/// PR collapse to one entry, with the first-seen title winning.
fn prs_from_events(
    events: &[Event],
    since: NaiveDate,
    until: NaiveDate,
    org: Option<&str>,
) -> (Vec<PrRef>, Vec<PrRef>, Vec<IssueRef>) {
    let mut reviewed: BTreeMap<(String, u64), PrRef> = BTreeMap::new();
    let mut commented: BTreeMap<(String, u64), PrRef> = BTreeMap::new();
    let mut commented_issues: BTreeMap<(String, u64), IssueRef> = BTreeMap::new();
    for ev in events {
        if !event_in_range(ev, since, until) {
            continue;
        }
        let Some(repo) = ev.repo_slug() else { continue };
        if !repo_in_org(repo, org) {
            continue;
        }
        let repo = repo.to_string();
        match ev.event_type.as_str() {
            "PullRequestReviewEvent" | "PullRequestReviewCommentEvent" => {
                if let Some(pr) = pr_ref_from_pr_event(&ev.payload, &repo) {
                    let key = (repo.clone(), pr.number);
                    if ev.event_type == "PullRequestReviewEvent" {
                        reviewed.entry(key).or_insert(pr);
                    } else {
                        commented.entry(key).or_insert(pr);
                    }
                }
            }
            "IssueCommentEvent" => {
                // A pull request, or an issue.
                let is_pr = ev
                    .payload
                    .get("issue")
                    .and_then(|i| i.get("pull_request"))
                    .is_some();
                if !is_pr {
                    if let Some(issue) = issue_ref_from_issue_event(&ev.payload, &repo) {
                        let key = (repo.clone(), issue.number);
                        commented_issues.entry(key).or_insert(issue);
                    }
                } else if let Some(pr) = pr_ref_from_issue_event(&ev.payload, &repo) {
                    let key = (repo.clone(), pr.number);
                    commented.entry(key).or_insert(pr);
                }
            }
            _ => {}
        }
    }
    (
        reviewed.into_values().collect(),
        commented.into_values().collect(),
        commented_issues.into_values().collect(),
    )
}

/// The annotated tags `user` made in the window in `repos`, read in
/// batches through GraphQL. A failed batch is a warning: the report
/// goes on with the others' tags.
fn collect_tags(
    client: &Client,
    repos: &BTreeSet<String>,
    user: &User,
    since: NaiveDate,
    until: NaiveDate,
) -> Vec<TagRef> {
    let repos: Vec<String> = repos.iter().cloned().collect();
    let recent = client.recent_annotated_tags(&repos);
    for failure in &recent.failures {
        eprintln!("warning: cannot read GitHub tags in {failure}");
    }
    let mut out: Vec<TagRef> = Vec::new();
    for (slug, list) in recent.tags {
        for t in list {
            if !date_in_range(&t.tagger.date, since, until) || !tagger_matches_user(&t.tagger, user)
            {
                continue;
            }
            out.push(TagRef {
                url: format!("https://github.com/{slug}/tree/{}", t.tag),
                repo: slug.clone(),
                tag: t.tag,
            });
        }
    }
    out.sort_by(|a, b| a.repo.cmp(&b.repo).then(a.tag.cmp(&b.tag)));
    out
}

/// With --private, the private repositories pushed to since `since`.
/// GitHub lists them for the token's owner only, so for another user
/// they are skipped with a note; a failed read is a warning.
fn private_repos(
    client: &Client,
    base: &str,
    login: &str,
    since: NaiveDate,
    verbose: bool,
) -> Vec<String> {
    match client.token_login() {
        Ok(owner) if owner.eq_ignore_ascii_case(login) => {}
        Ok(owner) => {
            eprintln!(
                "note: GitHub ({base}) lists private repositories for the token's owner, \
                 {owner}, not {login}; theirs are left out"
            );
            return Vec::new();
        }
        Err(e) => {
            eprintln!(
                "warning: cannot tell whose GitHub token this is on {base} ({e}); private repositories are left out"
            );
            return Vec::new();
        }
    }
    if verbose {
        eprintln!("[github] {base}: listing private repos pushed to since {since}");
    }
    client
        .private_repos_pushed_since(since)
        .unwrap_or_else(|e| {
            eprintln!("warning: cannot list private GitHub repositories on {base} ({e})");
            Vec::new()
        })
}

/// A pull request GraphQL gave, for the report's lists.
fn item_pr_ref(pr: &ItemNode) -> PrRef {
    let merged = pr.state == "MERGED";
    PrRef {
        repo: pr.repository.name_with_owner.clone(),
        number: pr.number,
        title: pr.title.clone(),
        url: pr.url.clone(),
        state: if merged {
            "closed".to_string()
        } else {
            pr.state.to_lowercase()
        },
        merged,
        applied: false,
    }
}

/// `first` and `more`, each pull request once (by repo and number),
/// `first`'s where both have it.
fn merge_prs(first: impl Iterator<Item = PrRef>, more: Vec<PrRef>) -> Vec<PrRef> {
    let mut all: BTreeMap<(String, u64), PrRef> = BTreeMap::new();
    for pr in first.chain(more) {
        all.entry((pr.repo.clone(), pr.number)).or_insert(pr);
    }
    all.into_values().collect()
}

/// An issue GraphQL gave, for the report's lists.
fn item_issue_ref(issue: &ItemNode) -> IssueRef {
    IssueRef {
        repo: issue.repository.name_with_owner.clone(),
        number: issue.number,
        title: issue.title.clone(),
        url: issue.url.clone(),
        state: issue.state.to_lowercase(),
    }
}

/// `first`, then those of `more` not among them, each issue once.
fn merge_issues(first: impl Iterator<Item = IssueRef>, more: Vec<IssueRef>) -> Vec<IssueRef> {
    let mut all: BTreeMap<(String, u64), IssueRef> = BTreeMap::new();
    for issue in first.chain(more) {
        all.entry((issue.repo.clone(), issue.number))
            .or_insert(issue);
    }
    all.into_values().collect()
}

fn tagger_matches_user(tagger: &sandogasa_github::Tagger, user: &User) -> bool {
    let name_match = user
        .name
        .as_deref()
        .is_some_and(|n| n.eq_ignore_ascii_case(&tagger.name));
    let email_match = user
        .email
        .as_deref()
        .is_some_and(|e| e.eq_ignore_ascii_case(&tagger.email));
    name_match || email_match
}

/// Walk the user's events and collect GitHub Releases they
/// published in the window. Sourced from `ReleaseEvent` with
/// `action == "published"`. Dedup is by `(repo, tag)`.
fn releases_from_events(
    events: &[Event],
    since: NaiveDate,
    until: NaiveDate,
    org: Option<&str>,
) -> Vec<ReleaseRef> {
    let mut releases: BTreeMap<(String, String), ReleaseRef> = BTreeMap::new();
    for ev in events {
        if !event_in_range(ev, since, until) {
            continue;
        }
        if ev.event_type != "ReleaseEvent" {
            continue;
        }
        if ev.payload.get("action").and_then(|v| v.as_str()) != Some("published") {
            continue;
        }
        let Some(release) = ev.payload.get("release") else {
            continue;
        };
        let Some(tag) = release.get("tag_name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(repo) = ev.repo_slug() else { continue };
        if !repo_in_org(repo, org) {
            continue;
        }
        let name = release
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let url = release
            .get("html_url")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| format!("https://github.com/{repo}/releases/tag/{tag}"));
        let prerelease = release
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let key = (repo.to_string(), tag.to_string());
        releases.entry(key).or_insert(ReleaseRef {
            repo: repo.to_string(),
            tag: tag.to_string(),
            name,
            url,
            prerelease,
        });
    }
    releases.into_values().collect()
}

fn pr_ref_from_pr_event(payload: &serde_json::Value, repo: &str) -> Option<PrRef> {
    let pr = payload.get("pull_request")?;
    let number = pr.get("number").and_then(|n| n.as_u64())?;
    let title = pr
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = pr
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}"));
    Some(PrRef {
        repo: repo.to_string(),
        number,
        title,
        url,
        state: String::new(),
        merged: false,
        applied: false,
    })
}

fn pr_ref_from_issue_event(payload: &serde_json::Value, repo: &str) -> Option<PrRef> {
    let issue = payload.get("issue")?;
    let number = issue.get("number").and_then(|n| n.as_u64())?;
    let title = issue
        .get("title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let url = issue
        .get("pull_request")
        .and_then(|p| p.get("html_url"))
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{repo}/pull/{number}"));
    Some(PrRef {
        repo: repo.to_string(),
        number,
        title,
        url,
        state: String::new(),
        merged: false,
        applied: false,
    })
}

/// The issue an `IssueCommentEvent` on an issue (not a pull request)
/// is about.
fn issue_ref_from_issue_event(payload: &serde_json::Value, repo: &str) -> Option<IssueRef> {
    let issue = payload.get("issue")?;
    let number = issue.get("number").and_then(|n| n.as_u64())?;
    let text = |field: &str| {
        issue
            .get(field)
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    };
    let url = issue
        .get("html_url")
        .and_then(|u| u.as_str())
        .map(String::from)
        .unwrap_or_else(|| format!("https://github.com/{repo}/issues/{number}"));
    // The state then, not now: left empty, as for pull requests.
    Some(IssueRef {
        repo: repo.to_string(),
        number,
        title: text("title"),
        url,
        state: String::new(),
    })
}

/// The search qualifier that keeps a report to share to public
/// repositories; nothing for one that may include private work.
fn visibility_clause(include_private: bool) -> &'static str {
    if include_private { "" } else { " is:public" }
}

/// The repositories among `repos` that are public now, asked eight
/// at a time; one that is gone, hidden or cannot be read counts as
/// private.
fn public_now(client: &Client, repos: &BTreeSet<String>) -> BTreeSet<String> {
    let repos: Vec<&String> = repos.iter().collect();
    let mut public = BTreeSet::new();
    for chunk in repos.chunks(8) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|slug| {
                    scope.spawn(move || {
                        let (owner, repo) = slug.split_once('/')?;
                        matches!(client.repository_public(owner, repo), Ok(Some(true)))
                            .then(|| (*slug).clone())
                    })
                })
                .collect();
            public.extend(handles.into_iter().filter_map(|h| h.join().ok().flatten()));
        });
    }
    public
}

/// The events a shared report keeps: public when they happened, in a
/// repository `public` (now) lists. Reviews, comments, releases, tags
/// and commit counts are all derived from these.
fn shareable_events(events: Vec<Event>, public: &BTreeSet<String>) -> Vec<Event> {
    events
        .into_iter()
        .filter(|e| e.public && e.repo_slug().is_some_and(|s| public.contains(s)))
        .collect()
}

fn run_pr_search(client: &Client, query: &str, verbose: bool) -> Result<Vec<PullRequest>, String> {
    run_search(client, "PR", query, verbose)
}

/// Search issues or pull requests (`what`, for messages): GitHub's
/// issue search answers both.
fn run_search(
    client: &Client,
    what: &str,
    query: &str,
    verbose: bool,
) -> Result<Vec<PullRequest>, String> {
    if verbose {
        eprintln!("[github] {what} search: {query}");
    }
    client
        .search_pull_requests(query)
        .map_err(|e| format!("GitHub {what} search failed: {e}"))
}

/// An issue the search gave, for the report's lists.
fn into_issue_ref(issue: PullRequest) -> IssueRef {
    IssueRef {
        repo: issue.repo_slug().unwrap_or_default(),
        number: issue.number,
        title: issue.title,
        url: issue.html_url,
        state: issue.state,
    }
}

fn into_pr_ref(pr: PullRequest) -> PrRef {
    let repo = pr.repo_slug().unwrap_or_default();
    let merged = pr.merged_at().is_some();
    PrRef {
        repo,
        number: pr.number,
        title: pr.title,
        url: pr.html_url,
        state: pr.state,
        merged,
        applied: false,
    }
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
                    "[github] applied-check: fetch {}#{} failed: {e}",
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
                    "[github] applied-check: compare for {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            false
        }
    }
}

/// The fallback when the PR's own commit is not on the target branch:
/// rebased or reworded, it lands under a new sha. GitHub lists a
/// branch's commits by author, so ask for the PR author's since the
/// earlier of the PR's opening and its own commits
/// ([`forge::listing_since`]), and match them against the PR's commits by title and
/// author (`forge::rebased_onto`). Two calls, spent only on a PR that
/// has already failed the sha check.
fn landed_rebased(
    client: &Client,
    owner: &str,
    repo: &str,
    pr: &PrRef,
    detail: &PullRequestDetail,
    verbose: bool,
) -> bool {
    let (Some(since), Some(login)) = (
        detail.created_at.as_deref(),
        detail.user.as_ref().map(|u| u.login.as_str()),
    ) else {
        return false;
    };
    let result = client
        .pull_request_commits(owner, repo, pr.number)
        .and_then(|pr_commits| {
            let opened = since;
            let since = forge::listing_since(opened, &commit_dates(&pr_commits));
            let branch =
                client.branch_commits_by(owner, repo, &detail.base.ref_name, login, &since)?;
            Ok(forge::rebased_onto(
                &landed(&pr_commits),
                &landed(&branch),
                Some(opened),
            ))
        });
    match result {
        Ok(applied) => applied,
        Err(e) => {
            if verbose {
                eprintln!(
                    "[github] applied-check: rebased lookup for {}#{} failed: {e}",
                    pr.repo, pr.number
                );
            }
            false
        }
    }
}

/// When each of `commits` was authored, as the forge gave it.
fn commit_dates(commits: &[RepoCommit]) -> Vec<Option<String>> {
    commits
        .iter()
        .map(|c| c.commit.author.as_ref().and_then(|a| a.date.clone()))
        .collect()
}

fn landed(commits: &[RepoCommit]) -> Vec<forge::Landed> {
    commits
        .iter()
        .map(|c| forge::Landed {
            authored: c.commit.author.as_ref().and_then(|a| a.date.clone()),
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

fn write_pr_list(out: &mut String, prs: &[PrRef]) {
    for pr in prs {
        out.push_str(&format!(
            "- [{}#{}]({}) {}{}\n",
            pr.repo,
            pr.number,
            pr.url,
            pr.title,
            pr.status_marker(),
        ));
    }
    out.push('\n');
}

fn write_issue_list(out: &mut String, issues: &[IssueRef]) {
    for issue in issues {
        let closed = if issue.state == "closed" {
            " (closed)"
        } else {
            ""
        };
        out.push_str(&format!(
            "- [{}#{}]({}) {}{closed}\n",
            issue.repo, issue.number, issue.url, issue.title,
        ));
    }
    out.push('\n');
}

fn write_tag_list(out: &mut String, tags: &[TagRef]) {
    let mut by_repo: BTreeMap<&str, Vec<&TagRef>> = BTreeMap::new();
    for t in tags {
        by_repo.entry(t.repo.as_str()).or_default().push(t);
    }
    for (repo, entries) in by_repo {
        let formatted: Vec<String> = entries
            .iter()
            .map(|t| format!("[{}]({})", t.tag, t.url))
            .collect();
        out.push_str(&format!("- `{repo}`: {}\n", formatted.join(", ")));
    }
    out.push('\n');
}

fn write_release_list(out: &mut String, releases: &[ReleaseRef]) {
    for r in releases {
        let prerelease = if r.prerelease { " (prerelease)" } else { "" };
        let suffix = match &r.name {
            Some(name) if name != &r.tag => format!(" — {name}"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "- [{} {}]({}){suffix}{prerelease}\n",
            r.repo, r.tag, r.url,
        ));
    }
    out.push('\n');
}

fn unique_repos<T>(items: &[T], repo_of: impl Fn(&T) -> &String) -> usize {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for it in items {
        seen.insert(repo_of(it).as_str());
    }
    seen.len()
}

fn event_in_range(event: &Event, since: NaiveDate, until: NaiveDate) -> bool {
    date_in_range(&event.created_at, since, until)
}

fn repo_in_org(slug: &str, org: Option<&str>) -> bool {
    match org {
        None => true,
        Some(prefix) => {
            // Slug is `owner/name`; we match on the owner part
            // (and tolerate slugs that already start with
            // `<prefix>/`).
            let prefix = prefix.trim_end_matches('/');
            slug == prefix
                || slug.starts_with(&format!("{prefix}/"))
                || slug.split_once('/').is_some_and(|(o, _)| o == prefix)
        }
    }
}

/// The keyring entry for `instance`'s token, which `config` keeps
/// it in.
pub(crate) fn secret(instance: &str) -> sandogasa_config::secret::SecretRef {
    forge::secret(instance, &TOKEN_SPEC)
}

/// Look up the GitHub API token for an instance.
///
/// Order: instance-specific env var → generic env var → the
/// keyring's entry for the instance → `github_tokens.<hostname>` from the user
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

    #[test]
    fn repo_in_org_matches_owner_prefix() {
        assert!(repo_in_org("slopfest/sandogasa", Some("slopfest")));
        assert!(repo_in_org("slopfest", Some("slopfest")));
        assert!(!repo_in_org("slopfest-extras/foo", Some("slopfest")));
        assert!(!repo_in_org("other/foo", Some("slopfest")));
    }

    #[test]
    fn repo_in_org_no_filter_matches_all() {
        assert!(repo_in_org("anything/here", None));
    }

    #[test]
    fn event_in_range_parses_iso8601() {
        let mut e: Event = serde_json::from_str(
            r#"{"id":"1","type":"PushEvent","repo":{"id":1,"name":"o/r"},
                "created_at":"2026-02-15T10:00:00Z"}"#,
        )
        .unwrap();
        let s = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        assert!(event_in_range(&e, s, u));
        e.created_at = "2026-04-01T00:00:00Z".into();
        assert!(!event_in_range(&e, s, u));
    }

    #[test]
    fn instance_token_env_generates_env_var_name() {
        assert_eq!(
            instance_token_env("https://api.github.com"),
            "GITHUB_TOKEN_API_GITHUB_COM"
        );
        assert_eq!(
            instance_token_env("https://api.example.com/"),
            "GITHUB_TOKEN_API_EXAMPLE_COM"
        );
    }

    #[test]
    #[serial_test::serial]
    fn find_token_errors_when_nothing_set() {
        let tokens = BTreeMap::new();
        let err = find_token("https://nonexistent.example.test", &tokens).unwrap_err();
        assert!(err.contains("no GitHub token"));
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
        if std::env::var(&var).is_ok() || std::env::var("GITHUB_TOKEN").is_ok() {
            return;
        }
        let tok = find_token("https://nonexistent.example.test", &tokens).unwrap();
        assert_eq!(tok, "from-config");
    }

    fn make_event(
        event_type: &str,
        repo: &str,
        created_at: &str,
        payload: serde_json::Value,
    ) -> Event {
        serde_json::from_value(serde_json::json!({
            "id": "1",
            "type": event_type,
            "repo": {"id": 1, "name": repo},
            "created_at": created_at,
            "public": true,
            "payload": payload,
        }))
        .unwrap()
    }

    /// A GitHub with one user, `me`, who in February 2026 opened a
    /// PR and pushed to `o/pub` (public), `o/priv` (private) and
    /// `o/since` (public then, private now). The PR searches answer
    /// only the query the visibility asks for.
    fn mock_github(include_private: bool) -> GithubReport {
        mock_github_with(include_private, false).unwrap()
    }

    /// The mock GitHub, with GraphQL answering 502 when `graphql_down`.
    fn mock_github_with(include_private: bool, graphql_down: bool) -> Result<GithubReport, String> {
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let vis = visibility_clause(include_private);
        let pr = json!({"total_count": 1, "items": [{
            "number": 3, "title": "Fix it", "state": "open",
            "html_url": "https://github.com/o/pub/pull/3"}]});
        let push = |repo: &str, public: bool| {
            json!({"id": "1", "type": "PushEvent", "repo": {"id": 1, "name": repo},
                   "created_at": "2026-02-15T10:00:00Z", "public": public, "payload": {}})
        };
        rt.block_on(async {
            let get = |p: &str, body: serde_json::Value| {
                Mock::given(method("GET"))
                    .and(path(p.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(body))
            };
            get("/users/me", json!({"id": 1, "login": "me", "name": "Me"}))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/search/issues"))
                .and(query_param(
                    "q",
                    format!("type:pr author:me created:2026-02-01..2026-02-28{vis}"),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(pr))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/search/issues"))
                .and(query_param(
                    "q",
                    format!("type:issue author:me created:2026-02-01..2026-02-28{vis}"),
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "total_count": 1, "items": [{
                        "number": 4, "title": "It breaks", "state": "closed",
                        "html_url": "https://github.com/o/pub/issues/4"}]
                })))
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/search/issues"))
                .and(query_param(
                    "q",
                    format!("type:pr author:me is:merged merged:2026-02-01..2026-02-28{vis}"),
                ))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"total_count": 0, "items": []})),
                )
                .mount(&server)
                .await;
            get(
                "/users/me/events",
                json!([
                    push("o/pub", true),
                    push("o/priv", false),
                    push("o/since", true)
                ]),
            )
            .mount(&server)
            .await;
            // o/since was public when pushed to, and is private now.
            get("/repos/o/pub", json!({"id": 1, "private": false}))
                .mount(&server)
                .await;
            get("/repos/o/since", json!({"id": 3, "private": true}))
                .mount(&server)
                .await;
            // GraphQL: o/old has commits but no event (#181), o/secret
            // is private; a review, comments, and a tag.
            let repo = |name: &str, private: bool| {
                json!({"nameWithOwner": name, "isPrivate": private})
            };
            let item = |name: &str, private: bool, n: u64, state: &str| {
                json!({"number": n, "title": format!("PR {n}"), "state": state,
                       "url": format!("https://github.com/{name}/pull/{n}"),
                       "repository": repo(name, private)})
            };
            let graphql = |marker: &str, data: serde_json::Value| {
                Mock::given(method("POST"))
                    .and(path("/graphql"))
                    .and(wiremock::matchers::body_string_contains(marker.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": data})))
            };
            graphql("contributionsCollection", json!({"user": {"contributionsCollection": {
                "commitContributionsByRepository": [
                    {"repository": repo("o/old", false), "contributions": {"totalCount": 4}},
                    {"repository": repo("o/secret", true), "contributions": {"totalCount": 3}}
                ],
                "pullRequestReviewContributions": {
                    "pageInfo": {"hasNextPage": false, "endCursor": null},
                    "nodes": [{"pullRequest": item("o/old", false, 9, "MERGED")}]
                }
            }}}))
            .mount(&server)
            .await;
            let comment = |at: &str, pr: serde_json::Value| {
                json!({"createdAt": at, "updatedAt": at, "url": "u", "issue": pr.clone(), "pullRequest": pr})
            };
            let on_issue = |at: &str, name: &str, private: bool, n: u64| {
                let mut issue = item(name, private, n, "OPEN");
                issue["url"] = json!(format!("https://github.com/{name}/issues/{n}"));
                json!({"createdAt": at, "updatedAt": at, "url": "u", "issue": issue, "pullRequest": null})
            };
            graphql("issueComments", json!({"user": {"issueComments": {
                "pageInfo": {"hasNextPage": true, "endCursor": "more"},
                "nodes": [
                    comment("2026-02-20T10:00:00Z", item("o/pub", false, 5, "OPEN")),
                    comment("2026-02-19T10:00:00Z", item("o/secret", true, 2, "OPEN")),
                    on_issue("2026-02-18T10:00:00Z", "o/pub", false, 6),
                    on_issue("2026-02-17T10:00:00Z", "o/secret", true, 8),
                    // Before the period: the read stops here.
                    comment("2026-01-02T10:00:00Z", item("o/pub", false, 1, "OPEN"))
                ]
            }}}))
            // One page: the older comment ends the read.
            .expect(u64::from(!graphql_down))
            .mount(&server)
            .await;
            // The tag query names the first repo, in order, r0: o/old.
            graphql("refs/tags/", json!({"r0": {"refs": {"nodes": [
                {"name": "v1", "target": {"tagger": {
                    "name": "Me", "email": "me@example.org", "date": "2026-02-10T09:00:00Z"}}},
                {"name": "v0", "target": {"tagger": {
                    "name": "Me", "email": "me@example.org", "date": "2025-12-10T09:00:00Z"}}},
                {"name": "light", "target": {}}
            ]}}}))
            .mount(&server)
            .await;
            get(
                "/user/repos",
                json!([
                    {"full_name": "o/secret2", "pushed_at": "2026-02-20T00:00:00Z"},
                    {"full_name": "o/ancient", "pushed_at": "2025-01-01T00:00:00Z"}
                ]),
            )
            .mount(&server)
            .await;
            get("/user", json!({"id": 1, "login": "me"}))
                .mount(&server)
                .await;
            graphql("user(login: $login) { id }", json!({"user": {"id": "U_me"}}))
                .mount(&server)
                .await;
            // Counted separately, in name order: o/pub alone shared;
            // o/priv, o/pub, o/secret2 and o/since with --private.
            let history = |n: u64| {
                json!({"defaultBranchRef": {"target": {"history": {"totalCount": n}}}})
            };
            let counts = if include_private {
                json!({"r0": history(5), "r1": history(2), "r2": history(1), "r3": history(7)})
            } else {
                json!({"r0": history(2)})
            };
            graphql("history(author", counts).mount(&server).await;
            if graphql_down {
                Mock::given(method("POST"))
                    .and(path("/graphql"))
                    .respond_with(ResponseTemplate::new(502))
                    .with_priority(1)
                    .mount(&server)
                    .await;
            }
        });
        let cfg: GithubConfig =
            serde_json::from_value(serde_json::json!({"instance": server.uri()})).unwrap();
        let mut tokens = BTreeMap::new();
        tokens.insert(forge::instance_host(&server.uri()), "tok".to_string());
        let day = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        github_report(
            &cfg,
            "me",
            day("2026-02-01"),
            day("2026-02-28"),
            &tokens,
            include_private,
            false,
        )
    }

    #[test]
    fn a_report_goes_on_without_graphql() {
        // The searches and events are kept; commits, reviews, comments
        // and tags that need GraphQL are missing, with warnings.
        let report = mock_github_with(true, true).unwrap();
        assert_eq!(report.opened_prs.len(), 1);
        assert!(report.commits_authored.is_empty());
        assert!(report.tags_pushed.is_empty());
    }

    #[test]
    fn private_repos_are_the_token_owners() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .and(path("/user"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_json(serde_json::json!({"id": 2, "login": "Someone"})),
                )
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/user/repos"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                    {"full_name": "s/p", "pushed_at": "2026-02-01T00:00:00Z"}
                ])))
                // Only for the owner's own report.
                .expect(1)
                .mount(&server)
                .await;
            server
        });
        let client = Client::new(&server.uri(), "tok").unwrap();
        let since = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        assert!(private_repos(&client, "b", "me", since, false).is_empty());
        assert_eq!(
            private_repos(&client, "b", "someone", since, false),
            ["s/p"]
        );
        // A token that cannot be read: nothing listed.
        let down = Client::new("http://127.0.0.1:9", "tok").unwrap();
        assert!(private_repos(&down, "b", "me", since, false).is_empty());
        drop(rt);
    }

    #[test]
    fn a_shared_report_asks_only_for_public_work() {
        let prs = |list: &[PrRef]| -> Vec<String> {
            list.iter()
                .map(|p| format!("{}#{} {}", p.repo, p.number, p.state))
                .collect()
        };
        let shared = mock_github(false);
        assert_eq!(shared.opened_prs.len(), 1);
        // o/old has no event: its commits come from the contributions.
        assert_eq!(
            shared.commits_authored,
            BTreeMap::from([("o/old".to_string(), 4), ("o/pub".to_string(), 2)])
        );
        assert_eq!(prs(&shared.reviewed_prs), ["o/old#9 closed"]);
        assert!(shared.reviewed_prs[0].merged);
        assert_eq!(prs(&shared.commented_prs), ["o/pub#5 open"]);
        let issues = |list: &[IssueRef]| -> Vec<String> {
            list.iter()
                .map(|i| format!("{}#{} {}", i.repo, i.number, i.state))
                .collect()
        };
        assert_eq!(issues(&shared.opened_issues), ["o/pub#4 closed"]);
        // A comment on a pull request is not one on an issue.
        assert_eq!(issues(&shared.commented_issues), ["o/pub#6 open"]);
        let tags: Vec<String> = shared
            .tags_pushed
            .iter()
            .map(|t| format!("{} {}", t.repo, t.tag))
            .collect();
        assert_eq!(tags, ["o/old v1"]);
        let full = mock_github(true);
        assert_eq!(full.opened_prs.len(), 1);
        assert_eq!(
            full.commits_authored,
            BTreeMap::from([
                ("o/old".to_string(), 4),
                ("o/priv".to_string(), 5),
                ("o/pub".to_string(), 2),
                ("o/secret".to_string(), 3),
                ("o/secret2".to_string(), 1),
                ("o/since".to_string(), 7)
            ])
        );
        assert_eq!(
            prs(&full.commented_prs),
            ["o/pub#5 open", "o/secret#2 open"]
        );
        assert_eq!(
            issues(&full.commented_issues),
            ["o/pub#6 open", "o/secret#8 open"]
        );
    }

    #[test]
    fn shared_events_were_public_then_and_are_public_now() {
        let at = "2026-11-18T10:00:00Z";
        let push = |repo: &str, public: bool| {
            let mut e = make_event("PushEvent", repo, at, serde_json::json!({}));
            e.public = public;
            e
        };
        let events = vec![
            push("o/private", false),
            push("o/made-private", true),
            push("o/public", true),
        ];
        // `public_now` lists the repositories public today.
        let now: BTreeSet<String> = ["o/private", "o/public"].map(String::from).into();
        let kept: Vec<String> = shareable_events(events, &now)
            .iter()
            .filter_map(|e| e.repo_slug().map(String::from))
            .collect();
        assert_eq!(kept, ["o/public"]);
        // An event without the field is taken as private.
        let bare: Event = serde_json::from_value(serde_json::json!({
            "id": "2", "type": "PushEvent", "repo": {"id": 1, "name": "o/r"},
            "created_at": at
        }))
        .unwrap();
        assert!(!bare.public);
    }

    #[test]
    fn prs_from_events_review_in_window() {
        let events = vec![make_event(
            "PullRequestReviewEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "pull_request": {
                    "number": 42,
                    "title": "Add foo",
                    "html_url": "https://github.com/slopfest/sandogasa/pull/42"
                }
            }),
        )];
        let (reviewed, commented, _) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(reviewed.len(), 1);
        assert_eq!(reviewed[0].number, 42);
        assert!(commented.is_empty());
    }

    #[test]
    fn prs_from_events_skips_old_pr_with_only_other_user_activity() {
        // The original bug: a 2021 PR the user reviewed long ago
        // shows up in Search because someone else's comment in
        // the window bumps the PR's `updated:` timestamp. With
        // events, only the user's own actions count — an event
        // from 2021 falls outside the window and gets dropped.
        let events = vec![make_event(
            "PullRequestReviewEvent",
            "starship/starship",
            "2021-08-15T10:00:00Z",
            serde_json::json!({
                "pull_request": {"number": 2612, "title": "old", "html_url": "x"}
            }),
        )];
        let (reviewed, commented, _) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 4, 26).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            None,
        );
        assert!(reviewed.is_empty());
        assert!(commented.is_empty());
    }

    #[test]
    fn prs_from_events_issue_comment_only_if_pr() {
        // IssueCommentEvent fires for both issues and PRs; only
        // the PR variant should count as a "PR commented on".
        let on_issue = make_event(
            "IssueCommentEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "issue": {"number": 1, "title": "issue-only"}
                // No `pull_request` field → it's an issue.
            }),
        );
        let on_pr = make_event(
            "IssueCommentEvent",
            "slopfest/sandogasa",
            "2026-02-15T10:00:00Z",
            serde_json::json!({
                "issue": {
                    "number": 5,
                    "title": "PR thread",
                    "pull_request": {"html_url": "https://github.com/slopfest/sandogasa/pull/5"}
                }
            }),
        );
        let (_, commented, issues) = prs_from_events(
            &[on_issue, on_pr],
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(commented.len(), 1);
        assert_eq!(commented[0].number, 5);
        // The comment on the issue is kept, as an issue.
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].url,
            "https://github.com/slopfest/sandogasa/issues/1"
        );
    }

    #[test]
    fn prs_from_events_dedups_multiple_events_on_same_pr() {
        // Two comments + one review on the same PR collapse to
        // one entry in each bucket.
        let events = vec![
            make_event(
                "PullRequestReviewEvent",
                "o/r",
                "2026-02-10T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
            make_event(
                "PullRequestReviewCommentEvent",
                "o/r",
                "2026-02-11T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
            make_event(
                "PullRequestReviewCommentEvent",
                "o/r",
                "2026-02-12T10:00:00Z",
                serde_json::json!({"pull_request": {"number": 7, "title": "x", "html_url": "https://github.com/o/r/pull/7"}}),
            ),
        ];
        let (reviewed, commented, _) = prs_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            None,
        );
        assert_eq!(reviewed.len(), 1);
        assert_eq!(commented.len(), 1);
    }

    #[test]
    fn into_pr_ref_extracts_slug_from_html_url() {
        let pr: PullRequest = serde_json::from_str(
            r#"{"number":42,"title":"Fix it","state":"open",
                "html_url":"https://github.com/slopfest/sandogasa/pull/42"}"#,
        )
        .unwrap();
        let r = into_pr_ref(pr);
        assert_eq!(r.repo, "slopfest/sandogasa");
        assert_eq!(r.number, 42);
        assert_eq!(r.title, "Fix it");
    }

    fn make_tagger(name: &str, email: &str, date: &str) -> sandogasa_github::Tagger {
        serde_json::from_value(serde_json::json!({
            "name": name, "email": email, "date": date
        }))
        .unwrap()
    }

    fn make_user(login: &str, name: Option<&str>, email: Option<&str>) -> User {
        serde_json::from_value(serde_json::json!({
            "id": 1, "login": login, "name": name, "email": email
        }))
        .unwrap()
    }

    #[test]
    fn tagger_matches_user_by_name_or_email() {
        let user = make_user(
            "michel-slm",
            Some("Michel Lind"),
            Some("michel@michel-slm.name"),
        );
        // Different email (commit identity) — name match wins.
        assert!(tagger_matches_user(
            &make_tagger(
                "Michel Lind",
                "salimma@fedoraproject.org",
                "2026-05-15T17:03:15Z"
            ),
            &user
        ));
        // Different name (display name change) — email match wins.
        assert!(tagger_matches_user(
            &make_tagger(
                "Someone Else",
                "michel@michel-slm.name",
                "2026-05-15T17:03:15Z"
            ),
            &user
        ));
        // Neither — reject.
        assert!(!tagger_matches_user(
            &make_tagger("Other", "other@example.com", "2026-05-15T17:03:15Z"),
            &user
        ));
    }

    #[test]
    fn tagger_matches_is_case_insensitive() {
        let user = make_user("u", Some("Michel Lind"), Some("M@example.com"));
        assert!(tagger_matches_user(
            &make_tagger("MICHEL LIND", "other@example.com", "2026-05-15T00:00:00Z"),
            &user
        ));
        assert!(tagger_matches_user(
            &make_tagger("other", "m@example.com", "2026-05-15T00:00:00Z"),
            &user
        ));
    }

    #[test]
    fn tagger_date_in_window_parses_iso8601() {
        let s = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let u = NaiveDate::from_ymd_opt(2026, 5, 31).unwrap();
        assert!(date_in_range("2026-05-15T17:03:15Z", s, u));
        assert!(!date_in_range("2026-06-01T00:00:00Z", s, u));
        assert!(!date_in_range("garbage", s, u));
    }

    #[test]
    fn releases_from_events_extracts_published() {
        let events = vec![
            make_event(
                "ReleaseEvent",
                "slopfest/sandogasa",
                "2026-05-15T10:00:00Z",
                serde_json::json!({
                    "action": "published",
                    "release": {
                        "tag_name": "v0.11.0",
                        "name": "Release v0.11.0",
                        "html_url": "https://github.com/slopfest/sandogasa/releases/tag/v0.11.0",
                        "prerelease": false
                    }
                }),
            ),
            // edited action — skip.
            make_event(
                "ReleaseEvent",
                "slopfest/sandogasa",
                "2026-05-15T11:00:00Z",
                serde_json::json!({
                    "action": "edited",
                    "release": {"tag_name": "v0.10.0", "name": "old", "html_url": "x"}
                }),
            ),
        ];
        let releases = releases_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            None,
        );
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].tag, "v0.11.0");
        assert_eq!(releases[0].name.as_deref(), Some("Release v0.11.0"));
        assert!(!releases[0].prerelease);
    }

    #[test]
    fn releases_from_events_handles_missing_name_and_prerelease() {
        let events = vec![make_event(
            "ReleaseEvent",
            "o/r",
            "2026-05-15T10:00:00Z",
            serde_json::json!({
                "action": "published",
                "release": {
                    "tag_name": "v0.0.1-rc1",
                    "html_url": "https://github.com/o/r/releases/tag/v0.0.1-rc1",
                    "prerelease": true
                }
            }),
        )];
        let releases = releases_from_events(
            &events,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            None,
        );
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].name, None);
        assert!(releases[0].prerelease);
    }

    #[test]
    fn format_empty_report() {
        let report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        let md = format_markdown(&report, 0);
        assert!(md.contains("### GitHub\n"));
        assert!(md.contains("No GitHub activity"));
    }

    #[test]
    fn level_one_counts_prs_by_repo() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        for (n, repo) in [(1, "o/a"), (2, "o/b"), (3, "o/b")] {
            report.opened_prs.push(PrRef {
                repo: repo.into(),
                number: n,
                title: format!("pr {n}"),
                url: format!("https://github.com/{repo}/pull/{n}"),
                state: "open".into(),
                merged: false,
                applied: false,
            });
        }
        let md = format_markdown(&report, 1);
        assert!(
            md.contains("#### Opened\n\n- `o/b`: 2\n- `o/a`: 1\n"),
            "{md}"
        );
        assert!(
            !md.contains("pr 1"),
            "level 1 counts, it does not list: {md}"
        );
        assert!(format_markdown(&report, 2).contains("pull/1) pr 1"));
    }

    #[test]
    fn issues_are_counted_listed_and_take_their_notes() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        let issue = |repo: &str, number: u64, title: &str, state: &str| IssueRef {
            repo: repo.into(),
            number,
            title: title.into(),
            url: format!("https://github.com/{repo}/issues/{number}"),
            state: state.into(),
        };
        report
            .opened_issues
            .push(issue("o/a", 1, "it breaks", "closed"));
        report
            .opened_issues
            .push(issue("o/b", 2, "it bends", "open"));
        report
            .commented_issues
            .push(issue("o/b", 3, "asked about", ""));
        let md = format_markdown(&report, 1);
        assert!(md.contains("- **Issues opened:** 2"), "{md}");
        assert!(md.contains("- **Issues commented on:** 1"), "{md}");
        assert!(
            md.contains("#### Issues opened\n\n- `o/a`: 1\n- `o/b`: 1\n"),
            "{md}"
        );
        let md = format_markdown(&report, 2);
        assert!(
            md.contains("- [o/a#1](https://github.com/o/a/issues/1) it breaks (closed)\n"),
            "{md}"
        );
        assert!(md.contains("issues/2) it bends\n"), "{md}");
        assert!(md.contains("#### Issues commented on\n\n- [o/b#3]"), "{md}");
        // A note on a comment of the issue attaches to it.
        let note = crate::notes::Stored {
            path: "n.md".into(),
            private: false,
            hidden: false,
            note: crate::notes::parse(
                "url: https://github.com/o/b/issues/3#issuecomment-9\ndate: 2026-02-03\n\nAsked for the reporter's logs.\n",
            )
            .unwrap(),
        };
        let (with_notes, unplaced) = crate::notes::attach(&md, &[note]);
        assert!(unplaced.is_empty());
        assert!(
            with_notes.contains("Asked for the reporter's logs."),
            "{with_notes}"
        );
    }

    /// #230, rasdaemon#272's shape: the maintainer applied the PR's
    /// commit as it was, so it kept its date, 15 minutes before the PR
    /// was opened, and the branch is only listed from that date.
    #[test]
    fn a_commit_applied_with_its_own_earlier_date_counts_as_applied() {
        use serde_json::json;
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let title = "meson.build: build openbmc-unified-sel on every architecture";
        let author = json!({"name": "Me", "email": "me@example.com",
                            "date": "2026-11-18T21:53:57Z"});
        rt.block_on(async {
            let get = |p: &str, body: serde_json::Value| {
                Mock::given(method("GET"))
                    .and(path(p.to_string()))
                    .respond_with(ResponseTemplate::new(200).set_body_json(body))
            };
            get(
                "/repos/o/r/pulls/272",
                json!({
                "number": 272, "state": "closed", "merged": false,
                "head": {"ref": "fix", "sha": "3780377"},
                "base": {"ref": "master", "sha": "609799a"},
                "created_at": "2026-11-20T11:17:51Z", "user": {"login": "me"}}),
            )
            .mount(&server)
            .await;
            // The PR's head is not on master under its own sha.
            get(
                "/repos/o/r/compare/master...3780377",
                json!({"ahead_by": 1}),
            )
            .mount(&server)
            .await;
            get(
                "/repos/o/r/pulls/272/commits",
                json!([{
                "sha": "3780377", "commit": {"message": title, "author": author}}]),
            )
            .mount(&server)
            .await;
            // Listed from the PR commit's date, the applied one is there.
            Mock::given(method("GET"))
                .and(path("/repos/o/r/commits"))
                .and(query_param("since", "2026-11-18T21:53:57Z"))
                .and(query_param("author", "me"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([{
                    "sha": "ffd1392",
                    "commit": {"message": format!("{title}\n\nSigned-off-by: M"),
                               "author": author}}])))
                .expect(1)
                .mount(&server)
                .await;
        });
        let client = Client::new(&server.uri(), "tok").unwrap();
        let pr = |list: &str| PrRef {
            repo: "o/r".into(),
            number: 272,
            title: title.into(),
            url: format!("https://github.com/o/r/pull/272#{list}"),
            state: "closed".into(),
            merged: false,
            applied: false,
        };
        // Commented on, as the events stream gives it: no state.
        let from_events = PrRef {
            state: String::new(),
            ..pr("reviewed")
        };
        let mut report = GithubReport {
            opened_prs: vec![pr("opened")],
            reviewed_prs: vec![from_events],
            commented_prs: vec![pr("commented")],
            ..Default::default()
        };
        mark_applied(&client, &mut report, false);
        // Checked once (the listing mock expects one request), and
        // marked alike in every list.
        assert!(report.opened_prs[0].applied);
        assert!(report.commented_prs[0].applied);
        assert!(report.reviewed_prs[0].applied);
        assert_eq!(report.reviewed_prs[0].state, "closed");
        drop(rt);
    }

    #[test]
    fn opened_list_marks_closed_merged_and_applied_state() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        let pr = |number: u64, title: &str, state: &str, merged: bool, applied: bool| PrRef {
            repo: "o/r".into(),
            number,
            title: title.into(),
            url: format!("https://github.com/o/r/pull/{number}"),
            state: state.into(),
            merged,
            applied,
        };
        report
            .opened_prs
            .push(pr(1, "declined", "closed", false, false));
        report
            .opened_prs
            .push(pr(2, "still going", "open", false, false));
        report
            .opened_prs
            .push(pr(3, "taken via cherry-pick", "closed", false, true));
        report
            .opened_prs
            .push(pr(4, "landed", "closed", true, false));
        // The event-derived lists carry no state and get no marker.
        report
            .reviewed_prs
            .push(pr(5, "looked at", "", false, false));
        let md = format_markdown(&report, 2);
        assert!(md.contains("pull/1) declined (closed)\n"), "{md}");
        assert!(md.contains("pull/2) still going\n"), "{md}");
        assert!(
            md.contains("pull/3) taken via cherry-pick (applied)\n"),
            "{md}"
        );
        assert!(md.contains("pull/4) landed (merged)\n"), "{md}");
        assert!(md.contains("pull/5) looked at\n"), "{md}");
        assert!(
            md.contains("- **PRs applied (landed unmerged):** 1"),
            "{md}"
        );
    }

    #[test]
    fn format_non_empty() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            org: Some("slopfest".into()),
            ..Default::default()
        };
        report.opened_prs.push(PrRef {
            repo: "slopfest/sandogasa".into(),
            number: 42,
            title: "Fix build".into(),
            url: "https://github.com/slopfest/sandogasa/pull/42".into(),
            state: "open".into(),
            merged: false,
            applied: false,
        });
        report
            .commits_authored
            .insert("slopfest/sandogasa".into(), 7);
        let md = format_markdown(&report, 2);
        assert!(md.contains("### GitHub\n"));
        assert!(md.contains("**PRs opened:** 1"));
        assert!(md.contains("**Commits authored:** 7 across 1 repo(s)"));
        assert!(md.contains("#### Opened"));
        assert!(md.contains("#42"));
        assert!(md.contains("Fix build"));
    }

    #[test]
    fn format_renders_tags_and_releases() {
        let mut report = GithubReport {
            instance: "https://api.github.com".into(),
            user: "octocat".into(),
            ..Default::default()
        };
        report.tags_pushed.push(TagRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.11.0".into(),
            url: "https://github.com/slopfest/sandogasa/tree/v0.11.0".into(),
        });
        report.tags_pushed.push(TagRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.10.2".into(),
            url: "https://github.com/slopfest/sandogasa/tree/v0.10.2".into(),
        });
        report.releases_published.push(ReleaseRef {
            repo: "slopfest/sandogasa".into(),
            tag: "v0.11.0".into(),
            name: Some("Release v0.11.0".into()),
            url: "https://github.com/slopfest/sandogasa/releases/tag/v0.11.0".into(),
            prerelease: false,
        });
        let md = format_markdown(&report, 2);
        assert!(md.contains("**Tags pushed:** 2 across 1 repo(s)"));
        assert!(md.contains("**Releases published:** 1 across 1 repo(s)"));
        assert!(md.contains("#### Tags pushed"));
        assert!(md.contains("v0.11.0"));
        assert!(md.contains("v0.10.2"));
        assert!(md.contains("#### Releases published"));
        assert!(md.contains("Release v0.11.0"));
    }
}
