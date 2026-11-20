// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Merged commits a SIG branch never built.
//!
//! A merge request merged into a SIG's branch ships nothing until
//! someone builds from it, and when the people who own a package's
//! content hand its builds to someone else, a merge can be dropped
//! between them. Each CBS build records the commit it was built from
//! (`git+https://…/rpms/<pkg>#<sha>`), so comparing that commit with
//! the branch head shows what was merged since, without reading the
//! spec.
//!
//! The caller says which branches a build may have come from, since
//! that is a SIG's own convention; this module asks the hub for every
//! build's commit at once, and GitLab, a few builds at a time, which of
//! those branches hold the commit and what each has gained since.

use chrono::{DateTime, Utc};
use sandogasa_gitlab::RepoCommit;
use serde::Serialize;

/// How many branches are compared at once.
pub const CONCURRENT: usize = 4;

/// Days a merged commit may wait for its build before it is flagged.
pub const DEFAULT_GRACE_DAYS: u32 = 3;

/// A package's newest build from one branch, to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub package: String,
    /// The branches the build may have come from, the likeliest first
    /// (`c9s-hs+fb`, then `c9s-hs`); an entry ending in `*` stands for
    /// every branch starting with the rest (`c10s-proposed-*`). Of the
    /// branches whose history holds the build's commit, those the first
    /// entry with any names are compared with it.
    pub branches: Vec<String>,
    pub nvr: String,
}

/// What [`check`] asks a package's repository, on GitLab for the SIGs.
pub trait Repository: Sync {
    /// The branches whose history holds `sha`.
    fn containing(&self, target: &Target, sha: &str) -> Result<Vec<String>, String>;
    /// The commits on `branch` that `sha` lacks, oldest first.
    fn since(&self, target: &Target, sha: &str, branch: &str) -> Result<Vec<RepoCommit>, String>;
}

/// A branch with merged commits its newest build does not have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Unbuilt {
    pub package: String,
    pub branch: String,
    /// The newest build from the branch.
    pub nvr: String,
    /// The commit it was built from.
    pub built: String,
    /// The commits merged since, oldest first.
    pub commits: Vec<Pending>,
}

/// A commit not built yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Pending {
    pub id: String,
    pub title: String,
    /// When it was committed (RFC 3339).
    pub committed: Option<String>,
    /// The merge request it merged, as its merge commit names it
    /// (`CentOS/Hyperscale/rpms/openssh!7`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_request: Option<String>,
}

/// A SIG's package repositories on GitLab, `<group>/<package>`.
pub struct GitLab {
    base: String,
    group: String,
    token: String,
    /// The tool's name, to log each request under with `-v`.
    log: Option<String>,
}

impl GitLab {
    /// The repositories under `group` (`CentOS/Hyperscale/rpms`) on the
    /// GitLab at `base`; an empty `token` reads them anonymously.
    pub fn new(base: &str, group: &str, token: &str) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            group: group.trim_matches('/').to_string(),
            token: token.to_string(),
            log: None,
        }
    }

    /// Log each request on standard error, as `[<tool>] …`.
    pub fn logged(mut self, tool: &str) -> Self {
        self.log = Some(tool.to_string());
        self
    }

    fn client(&self, target: &Target, what: &str) -> Result<sandogasa_gitlab::Client, String> {
        if let Some(tool) = &self.log {
            eprintln!("[{tool}] {} {what}", target.package);
        }
        sandogasa_gitlab::Client::new(
            &self.base,
            &format!("{}/{}", self.group, target.package),
            &self.token,
        )
        .map_err(|e| e.to_string())
    }
}

impl Repository for GitLab {
    fn containing(&self, target: &Target, sha: &str) -> Result<Vec<String>, String> {
        match self
            .client(target, &format!("branches holding {}", short(sha)))?
            .branches_containing(sha)
        {
            Ok(branches) => Ok(branches),
            // A commit GitLab no longer has is on no branch.
            Err(e) if e.to_string().contains("404 Commit Not Found") => Ok(Vec::new()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn since(&self, target: &Target, sha: &str, branch: &str) -> Result<Vec<RepoCommit>, String> {
        self.client(target, &format!("{branch} since {}", short(sha)))?
            .commits_between(sha, branch)
            .map_err(|e| e.to_string())
    }
}

/// What [`check`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct Outcome {
    /// Branches with commits not built, the newest of them older than
    /// the grace period.
    pub unbuilt: Vec<Unbuilt>,
    /// Branches whose newest unbuilt commit is within it.
    pub recent: Vec<Unbuilt>,
    /// Builds CBS records no commit for (one made from an uploaded
    /// SRPM), which cannot be compared.
    pub unsourced: Vec<String>,
    /// Builds whose commit no branch they may have come from holds, or
    /// GitLab no longer has (built from a merge request's branch, or
    /// from a branch since rewritten), and why: an old build, usually,
    /// with nothing to compare.
    pub unplaced: Vec<String>,
    /// Builds that could not be checked, GitLab or the hub failing, and
    /// why.
    pub problems: Vec<String>,
}

/// The merge request a merge commit's `message` names, from GitLab's
/// `See merge request <project>!<n>` line.
pub fn merge_request_of(message: &str) -> Option<String> {
    message
        .lines()
        .find_map(|l| l.trim().strip_prefix("See merge request "))
        .map(|r| r.trim().to_string())
        .filter(|r| r.contains('!'))
}

/// When the newest of `commits` was committed, as GitLab gives it
/// (RFC 3339); `None` when none has a readable date.
pub fn newest(commits: &[Pending]) -> Option<&str> {
    commits
        .iter()
        .filter_map(|c| {
            let d = c.committed.as_deref()?;
            DateTime::parse_from_rfc3339(d).ok().map(|t| (t, d))
        })
        .max_by_key(|(t, _)| *t)
        .map(|(_, d)| d)
}

/// Whether the newest of `commits` was committed at or before `cutoff`:
/// nothing has landed on the branch for the grace period, and it is
/// still not built. Commits merged from elsewhere keep their own dates,
/// so the oldest says little about when the branch got ahead. Without
/// any readable date, it counts as old.
pub fn settled(commits: &[Pending], cutoff: DateTime<Utc>) -> bool {
    newest(commits)
        .and_then(|d| DateTime::parse_from_rfc3339(d).ok())
        .is_none_or(|d| d.with_timezone(&Utc) <= cutoff)
}

/// `f` over `items`, [`CONCURRENT`] at a time, the answers in order:
/// for remote calls that have no batch form.
pub fn bounded<T, R, F>(items: &[T], f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(CONCURRENT) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|i| scope.spawn(|| f(i))).collect();
            out.extend(
                handles
                    .into_iter()
                    .map(|h| h.join().unwrap_or_else(|e| std::panic::resume_unwind(e))),
            );
        });
    }
    out
}

/// The branches of `containing` that `entries` picks: those named by
/// the first entry naming any, an entry ending in `*` naming every
/// branch that starts with the rest.
pub fn pick(entries: &[String], containing: &[String]) -> Vec<String> {
    for entry in entries {
        let named: Vec<String> = containing
            .iter()
            .filter(|b| match entry.strip_suffix('*') {
                Some(prefix) => b.starts_with(prefix),
                None => *b == entry,
            })
            .cloned()
            .collect();
        if !named.is_empty() {
            return named;
        }
    }
    Vec::new()
}

/// Compare each target's build, made from `built` (its commit, `None`
/// when CBS has none), with the branches holding it that the target
/// picks ([`pick`]), asking `repo`; a few targets at a time. A branch
/// whose newest unbuilt commit is newer than `cutoff` is only `recent`.
pub fn check(
    targets: &[Target],
    built: &[Option<String>],
    repo: &dyn Repository,
    cutoff: DateTime<Utc>,
) -> Outcome {
    let mut outcome = Outcome::default();
    let jobs: Vec<(&Target, &Option<String>)> = targets.iter().zip(built).collect();
    // Each branch compared, with what each build from it lacks: several
    // dist tags can come from one branch (a variant built from `cNs-hs`),
    // and the branch is built once any of them is at its head.
    let mut branches: Vec<Unbuilt> = Vec::new();
    for (r, (t, _)) in bounded(&jobs, |(t, sha)| sha.as_deref().map(|s| one(t, s, repo)))
        .into_iter()
        .zip(&jobs)
    {
        match r {
            None => outcome.unsourced.push(t.nvr.clone()),
            Some(Err(Missed::Unplaced(e))) => outcome.unplaced.push(e),
            Some(Err(Missed::Failed(e))) => outcome.problems.push(e),
            Some(Ok(found)) => {
                for u in found {
                    match branches
                        .iter_mut()
                        .find(|b| b.package == u.package && b.branch == u.branch)
                    {
                        Some(b) if u.commits.len() < b.commits.len() => *b = u,
                        Some(_) => {}
                        None => branches.push(u),
                    }
                }
            }
        }
    }
    for u in branches.into_iter().filter(|u| !u.commits.is_empty()) {
        match settled(&u.commits, cutoff) {
            true => outcome.unbuilt.push(u),
            false => outcome.recent.push(u),
        }
    }
    outcome
}

/// [`check`] with each build's commit read from the hub at `hub_url`,
/// in one call.
pub fn find(
    hub_url: &str,
    targets: &[Target],
    repo: &dyn Repository,
    cutoff: DateTime<Utc>,
) -> Result<Outcome, String> {
    let nvrs: Vec<String> = targets.iter().map(|t| t.nvr.clone()).collect();
    let built = crate::packages::build_commits_at(hub_url, &nvrs)?;
    Ok(check(targets, &built, repo, cutoff))
}

/// One branch as text: a line saying how many commits its newest build
/// lacks and the day of the newest, then each commit with its day and
/// merge request.
pub fn render(u: &Unbuilt) -> String {
    let mut out = format!(
        "{} {}: {} commit(s) merged since {} ({}) was built",
        u.package,
        u.branch,
        u.commits.len(),
        u.nvr,
        short(&u.built)
    );
    if let Some(d) = newest(&u.commits) {
        out.push_str(&format!(", the newest from {}", day(d)));
    }
    out.push('\n');
    for c in &u.commits {
        let day = c.committed.as_deref().map_or("", day);
        out.push_str(&format!("  {day} {} {}", short(&c.id), c.title));
        if let Some(mr) = &c.merge_request {
            out.push_str(&format!(" ({mr})"));
        }
        out.push('\n');
    }
    out
}

/// How the line an issue body carries for unbuilt commits starts.
pub const LINE: &str = "- **Not built**:";

/// The line a tracking issue's body carries while `found` (branches
/// past the grace period) have commits not built:
/// `- **Not built**: 9 commit(s) on c9s-hs+fb since <nvr>, the newest
/// from 2026-11-10`, one clause per branch. `None` when there are none,
/// so a body that had the line loses it.
pub fn body_line(found: &[Unbuilt]) -> Option<String> {
    if found.is_empty() {
        return None;
    }
    let clauses: Vec<String> = found
        .iter()
        .map(|u| {
            let mut c = format!(
                "{} commit(s) on `{}` since {}",
                u.commits.len(),
                u.branch,
                u.nvr
            );
            if let Some(d) = newest(&u.commits) {
                c.push_str(&format!(", the newest from {}", day(d)));
            }
            c
        })
        .collect();
    Some(format!("{LINE} {}", clauses.join("; ")))
}

/// `body` with its [`LINE`] replaced by `line`, added at the end when it
/// had none, or removed for `None`.
pub fn with_body_line(body: &str, line: Option<&str>) -> String {
    let mut out: Vec<&str> = body
        .lines()
        .filter(|l| !l.trim_start().starts_with(LINE))
        .collect();
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    let mut text = out.join("\n");
    if let Some(line) = line {
        text.push_str(if text.is_empty() { "" } else { "\n\n" });
        text.push_str(line);
    }
    text
}

/// What a comment on a build of `branch` says about `commits`, the
/// branch's commits the build lacks: how many, the newest's day, and
/// each one's title, so whoever moves the build sees what it does not
/// carry. It does not say they are unbuilt: a newer build, in testing
/// say, may have them. `None` when there are none.
pub fn paragraph(branch: &str, commits: Vec<RepoCommit>) -> Option<String> {
    if commits.is_empty() {
        return None;
    }
    let commits: Vec<Pending> = commits.into_iter().map(pending).collect();
    let mut text = format!(
        "`{branch}` has {} commit(s) this build does not",
        commits.len()
    );
    if let Some(d) = newest(&commits) {
        text.push_str(&format!(", the newest from {}", day(d)));
    }
    text.push_str(":\n");
    for c in &commits {
        text.push_str(&format!("\n- {} {}", short(&c.id), c.title));
        if let Some(mr) = &c.merge_request {
            text.push_str(&format!(" ({mr})"));
        }
    }
    Some(text)
}

/// Why a build was not compared.
enum Missed {
    /// No branch it may have come from holds its commit.
    Unplaced(String),
    /// GitLab failed.
    Failed(String),
}

/// One target: each branch it picks among those holding `sha`, with the
/// commits it gained since, none for a branch at the build's commit.
fn one(target: &Target, sha: &str, repo: &dyn Repository) -> Result<Vec<Unbuilt>, Missed> {
    let about = |e: String| format!("{}: {}: {e}", target.package, target.nvr);
    let containing = repo
        .containing(target, sha)
        .map_err(|e| Missed::Failed(about(e)))?;
    let branches = pick(&target.branches, &containing);
    if branches.is_empty() {
        return Err(Missed::Unplaced(about(format!(
            "none of {} holds the commit it was built from ({})",
            target.branches.join(", "),
            short(sha)
        ))));
    }
    let mut found = Vec::new();
    for branch in branches {
        let commits = repo
            .since(target, sha, &branch)
            .map_err(|e| Missed::Failed(about(format!("{branch}: {e}"))))?;
        found.push(Unbuilt {
            package: target.package.clone(),
            branch,
            nvr: target.nvr.clone(),
            built: sha.to_string(),
            commits: commits.into_iter().map(pending).collect(),
        });
    }
    Ok(found)
}

/// A commit as reported.
fn pending(c: RepoCommit) -> Pending {
    Pending {
        merge_request: merge_request_of(&c.message),
        id: c.id,
        title: c.title,
        committed: c.committed_date,
    }
}

/// The day of an RFC 3339 time.
fn day(time: &str) -> &str {
    time.get(..10).unwrap_or(time)
}

/// The first 12 characters of a commit id.
fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(id: &str, date: &str, message: &str) -> RepoCommit {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "title": message.lines().next().unwrap_or(""),
            "message": message,
            "committed_date": date,
        }))
        .unwrap()
    }

    fn target(package: &str, branches: &[&str]) -> Target {
        Target {
            package: package.into(),
            branches: branches.iter().map(|b| b.to_string()).collect(),
            nvr: format!("{package}-1-1.hs+fb.el9"),
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn unbuilt(branch: &str, n: usize) -> Unbuilt {
        Unbuilt {
            package: "openssh".into(),
            branch: branch.into(),
            nvr: "openssh-9.9p1-16.1.hs+fb.el9".into(),
            built: "55ea2316".into(),
            commits: (0..n)
                .map(|i| pending(commit(&format!("c{i}"), "2026-11-10T10:00:00Z", "Fix")))
                .collect(),
        }
    }

    #[test]
    fn an_issue_body_carries_one_line_while_commits_wait() {
        let line = body_line(&[unbuilt("c9s-hs+fb", 9), unbuilt("c9s-hs", 1)]).unwrap();
        assert_eq!(
            line,
            "- **Not built**: 9 commit(s) on `c9s-hs+fb` since openssh-9.9p1-16.1.hs+fb.el9, \
             the newest from 2026-11-10; 1 commit(s) on `c9s-hs` since \
             openssh-9.9p1-16.1.hs+fb.el9, the newest from 2026-11-10"
        );
        assert_eq!(body_line(&[]), None);
        let body = "- **MR**: x\n- **Release**: c9s\n";
        let with = with_body_line(body, Some("- **Not built**: 1"));
        assert_eq!(
            with,
            "- **MR**: x\n- **Release**: c9s\n\n- **Not built**: 1"
        );
        // Replaced, then removed once built.
        assert_eq!(
            with_body_line(&with, Some("- **Not built**: 2")),
            "- **MR**: x\n- **Release**: c9s\n\n- **Not built**: 2"
        );
        assert_eq!(
            with_body_line(&with, None),
            "- **MR**: x\n- **Release**: c9s"
        );
        assert_eq!(
            with_body_line("", Some("- **Not built**: 1")),
            "- **Not built**: 1"
        );
    }

    #[test]
    fn a_build_comment_lists_what_its_branch_has_since() {
        assert_eq!(paragraph("c9s-hs", vec![]), None);
        let text = paragraph(
            "c9s-hs+fb",
            vec![
                commit(
                    "aaaaaaaaaaaaaaaa",
                    "2026-10-29T00:00:00Z",
                    "Fix CVE-2026-106552",
                ),
                commit(
                    "bbbb",
                    "2026-11-10T00:00:00Z",
                    "Merge c9s release 17\nSee merge request g/openssh!7",
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            text,
            "`c9s-hs+fb` has 2 commit(s) this build does not, the newest from 2026-11-10:\
             \n\n- aaaaaaaaaaaa Fix CVE-2026-106552\n- bbbb Merge c9s release 17 \
             (g/openssh!7)"
        );
    }

    #[test]
    fn bounded_keeps_the_order() {
        let items: Vec<u32> = (0..10).collect();
        assert_eq!(
            bounded(&items, |i| i * 2),
            (0..10).map(|i| i * 2).collect::<Vec<_>>()
        );
        assert!(bounded(&[] as &[u32], |i| *i).is_empty());
    }

    #[test]
    fn a_merge_commit_names_its_merge_request() {
        let msg = "Merge branch 'c9s-17' into 'c9s-hs+fb'\n\nRebase on stock\n\n\
                   See merge request CentOS/Hyperscale/rpms/openssh!7";
        assert_eq!(
            merge_request_of(msg).as_deref(),
            Some("CentOS/Hyperscale/rpms/openssh!7")
        );
        assert_eq!(merge_request_of("Fix the build"), None);
        assert_eq!(merge_request_of("See merge request nothing"), None);
    }

    /// A `(sha, branch)` compared.
    type Compared = (&'static str, &'static str);

    /// A repository answering from tables: which branches hold each
    /// sha, and each `(sha, branch)`'s commits since.
    struct Fake {
        holding: Vec<(&'static str, Vec<&'static str>)>,
        since: Vec<(Compared, Result<Vec<RepoCommit>, String>)>,
    }

    impl Repository for Fake {
        fn containing(&self, _: &Target, sha: &str) -> Result<Vec<String>, String> {
            self.holding
                .iter()
                .find(|(s, _)| *s == sha)
                .map(|(_, b)| b.iter().map(|b| b.to_string()).collect())
                .ok_or_else(|| "502 Bad Gateway".to_string())
        }
        fn since(&self, _: &Target, sha: &str, branch: &str) -> Result<Vec<RepoCommit>, String> {
            self.since
                .iter()
                .find(|(k, _)| *k == (sha, branch))
                .map(|(_, r)| r.clone())
                .unwrap_or_else(|| panic!("compared {sha} with {branch}"))
        }
    }

    #[test]
    fn a_flavor_is_compared_with_the_branch_holding_its_build() {
        let entries = |e: &[&str]| e.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let hs = entries(&["c9s-hs+fb", "c9s-hs", "c9s-sig-hyperscale"]);
        assert_eq!(
            pick(&hs, &entries(&["c9s", "c9s-hs+fb", "c9s-hs"])),
            ["c9s-hs+fb"]
        );
        // Built from c9s-hs while a stale c9s-hs+fb exists elsewhere.
        assert_eq!(pick(&hs, &entries(&["c9s-hs"])), ["c9s-hs"]);
        assert!(pick(&hs, &entries(&["c9s", "main"])).is_empty());
        let pu = entries(&["c10s-proposed-*"]);
        assert_eq!(
            pick(
                &pu,
                &entries(&[
                    "c10s",
                    "c10s-proposed-RHEL-1",
                    "c10s-proposed-RHEL-2",
                    "c9s-proposed-RHEL-3"
                ])
            ),
            ["c10s-proposed-RHEL-1", "c10s-proposed-RHEL-2"]
        );
    }

    #[test]
    fn a_branch_is_flagged_once_its_newest_commit_has_waited_past_the_grace_period() {
        let targets = [
            target("openssh", &["c9s-hs+fb", "c9s-hs"]),
            target("fresh", &["c9s-hs"]),
            target("built", &["c9s-hs"]),
            target("legacy", &["c9s-hs", "c9s-sig-hyperscale"]),
            target("nosource", &["c9s-hs"]),
            target("flaky", &["c9s-hs"]),
            target("elsewhere", &["c9s-hs"]),
        ];
        let built = [
            Some("55ea2316".to_string()),
            Some("f00".to_string()),
            Some("head".to_string()),
            Some("01d".to_string()),
            None,
            Some("unanswered".to_string()),
            Some("mr".to_string()),
        ];
        let repo = Fake {
            holding: vec![
                ("55ea2316", vec!["c9s-hs", "c9s-hs+fb"]),
                ("f00", vec!["c9s-hs"]),
                ("head", vec!["c9s-hs"]),
                ("01d", vec!["c9s-sig-hyperscale"]),
                ("mr", vec!["fix-branch"]),
            ],
            since: vec![
                (
                    ("55ea2316", "c9s-hs+fb"),
                    Ok(vec![
                        commit("c1", "2026-10-11T09:00:00Z", "Rebase on c9s release 17"),
                        commit(
                            "c2aaaaaaaaaaaaaaaa",
                            "2026-11-10T10:00:00+00:00",
                            "Merge branch 'x'\nSee merge request g/openssh!7",
                        ),
                    ]),
                ),
                (
                    ("f00", "c9s-hs"),
                    Ok(vec![commit("n1", "2026-11-19T12:00:00Z", "New")]),
                ),
                (("head", "c9s-hs"), Ok(vec![])),
                (
                    ("01d", "c9s-sig-hyperscale"),
                    Ok(vec![commit("l1", "2026-10-01T00:00:00Z", "Old")]),
                ),
            ],
        };
        let out = check(&targets, &built, &repo, at("2026-11-17T12:00:00Z"));
        let names = |v: &[Unbuilt]| {
            v.iter()
                .map(|u| format!("{} {}", u.package, u.branch))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&out.unbuilt),
            ["openssh c9s-hs+fb", "legacy c9s-sig-hyperscale"]
        );
        assert_eq!(names(&out.recent), ["fresh c9s-hs"]);
        assert_eq!(out.unsourced, ["nosource-1-1.hs+fb.el9"]);
        assert_eq!(out.problems.len(), 1, "{:?}", out.problems);
        assert!(out.problems[0].contains("502"), "{:?}", out.problems);
        assert_eq!(out.unplaced.len(), 1, "{:?}", out.unplaced);
        assert!(
            out.unplaced[0].contains("none of c9s-hs holds"),
            "{:?}",
            out.unplaced
        );

        let text = render(&out.unbuilt[0]);
        assert_eq!(
            text,
            "openssh c9s-hs+fb: 2 commit(s) merged since openssh-1-1.hs+fb.el9 (55ea2316) \
             was built, the newest from 2026-11-10\n  2026-10-11 c1 Rebase on c9s release 17\n  \
             2026-11-10 c2aaaaaaaaaa Merge branch 'x' (g/openssh!7)\n"
        );
    }

    #[test]
    fn a_branch_is_built_once_any_build_from_it_is_at_its_head() {
        // perf: an old experimental build and a current main one, both
        // from c9s-hs; qemu: both behind, reported once.
        let targets = [
            target("perf", &["c9s-hsx", "c9s-hs"]),
            target("perf", &["c9s-hs"]),
            target("qemu", &["c9s-hsx", "c9s-hs"]),
            target("qemu", &["c9s-hs"]),
        ];
        let built = [
            Some("old".to_string()),
            Some("head".to_string()),
            Some("older".to_string()),
            Some("behind".to_string()),
        ];
        let repo = Fake {
            holding: vec![
                ("old", vec!["c9s-hs"]),
                ("head", vec!["c9s-hs"]),
                ("older", vec!["c9s-hs"]),
                ("behind", vec!["c9s-hs"]),
            ],
            since: vec![
                (
                    ("old", "c9s-hs"),
                    Ok(vec![commit("a", "2026-09-08T00:00:00Z", "x"); 10]),
                ),
                (("head", "c9s-hs"), Ok(vec![])),
                (
                    ("older", "c9s-hs"),
                    Ok(vec![commit("b", "2026-09-08T00:00:00Z", "y"); 5]),
                ),
                (
                    ("behind", "c9s-hs"),
                    Ok(vec![commit("c", "2026-09-09T00:00:00Z", "z")]),
                ),
            ],
        };
        let out = check(&targets, &built, &repo, at("2026-11-17T00:00:00Z"));
        assert_eq!(out.unbuilt.len(), 1, "{:?}", out.unbuilt);
        assert_eq!(out.unbuilt[0].package, "qemu");
        assert_eq!(out.unbuilt[0].commits.len(), 1);
    }

    #[test]
    fn a_commit_gitlab_no_longer_has_is_on_no_branch() {
        let mut server = mockito::Server::new();
        server
            .mock(
                "GET",
                mockito::Matcher::Regex("/repository/commits/gone/refs".into()),
            )
            .match_query(mockito::Matcher::Any)
            .with_status(404)
            .with_body(r#"{"message": "404 Commit Not Found"}"#)
            .create();
        server
            .mock(
                "GET",
                mockito::Matcher::Regex("/repository/commits/flaky/refs".into()),
            )
            .match_query(mockito::Matcher::Any)
            .with_status(502)
            .create();
        let repo = GitLab::new(&server.url(), "g/rpms", "");
        let t = target("rpm", &["c8s-hs"]);
        assert_eq!(repo.containing(&t, "gone").unwrap(), Vec::<String>::new());
        assert!(repo.containing(&t, "flaky").is_err());
        let out = check(
            &[t.clone(), t],
            &[Some("gone".into()), Some("flaky".into())],
            &repo,
            at("2026-11-17T00:00:00Z"),
        );
        assert_eq!(out.unplaced.len(), 1, "{:?}", out.unplaced);
        assert_eq!(out.problems.len(), 1, "{:?}", out.problems);
    }

    #[test]
    fn the_newest_commit_decides_whether_a_branch_has_waited() {
        let p = |d: Option<&str>| Pending {
            id: "x".into(),
            title: String::new(),
            committed: d.map(String::from),
            merge_request: None,
        };
        let cutoff = at("2026-11-17T00:00:00Z");
        assert!(settled(&[p(None)], cutoff));
        assert!(settled(&[p(Some("garbled"))], cutoff));
        assert!(!settled(&[p(Some("2026-11-18T00:00:00Z"))], cutoff));
        assert!(settled(&[p(Some("2026-11-17T00:00:00Z"))], cutoff));
        // The newest decides: a stock commit from weeks ago merged with
        // one from yesterday is still within the grace period.
        let merged = [
            p(Some("2026-10-01T00:00:00Z")),
            p(Some("2026-11-18T00:00:00+02:00")),
        ];
        assert!(!settled(&merged, cutoff));
        assert_eq!(newest(&merged), Some("2026-11-18T00:00:00+02:00"));
    }

    #[test]
    fn gitlab_asks_the_packages_repository() {
        let mut server = mockito::Server::new();
        let refs = server
            .mock(
                "GET",
                "/api/v4/projects/g%2Frpms%2Fopenssh/repository/commits/55ea2316/refs",
            )
            .match_query(mockito::Matcher::Any)
            .with_body(r#"[{"type": "branch", "name": "c9s-hs+fb"}]"#)
            .create();
        let compare = server
            .mock(
                "GET",
                "/api/v4/projects/g%2Frpms%2Fopenssh/repository/compare",
            )
            .match_query(mockito::Matcher::Any)
            .with_body(
                r#"{"commits": [{"id": "c1", "committed_date": "2026-11-01T00:00:00Z"}],
                    "diffs": [{"new_path": "x.spec"}]}"#,
            )
            .create();
        let repo = GitLab::new(&format!("{}/", server.url()), "/g/rpms/", "").logged("test");
        let t = target("openssh", &["c9s-hs+fb"]);
        assert_eq!(repo.containing(&t, "55ea2316").unwrap(), ["c9s-hs+fb"]);
        assert_eq!(repo.since(&t, "55ea2316", "c9s-hs+fb").unwrap()[0].id, "c1");
        refs.assert();
        compare.assert();
    }

    #[test]
    fn find_reads_each_builds_commit_from_the_hub_in_one_call() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/")
            .with_status(200)
            .with_body(
                "<?xml version='1.0'?><methodResponse><params><param><value><array><data>\
                 <value><array><data><value><struct><member><name>source</name><value>\
                 <string>git+https://gitlab.com/CentOS/Hyperscale/rpms/openssh#55ea2316\
                 </string></value></member></struct></value></data></array></value>\
                 </data></array></value></param></params></methodResponse>",
            )
            .expect(1)
            .create();
        let targets = [target("openssh", &["c9s-hs+fb"])];
        let repo = Fake {
            holding: vec![("55ea2316", vec!["c9s-hs+fb"])],
            since: vec![(
                ("55ea2316", "c9s-hs+fb"),
                Ok(vec![commit("c1", "2026-11-01T00:00:00Z", "x")]),
            )],
        };
        let out = find(&server.url(), &targets, &repo, at("2026-11-17T00:00:00Z")).unwrap();
        assert_eq!(out.unbuilt.len(), 1);
        mock.assert();
    }
}
