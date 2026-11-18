// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Telling a build's tracking issue what happened to it in CBS (#144):
//! a comment, the `stage::` label and, from `To do`, the `In progress`
//! status. Planned before the CBS write is confirmed, so the comment is
//! part of what is confirmed, and posted once the write is done. A
//! failure here is a warning: the build has moved either way. What it
//! reports along the way goes to stderr, so a caller's stdout (a
//! `--json` report) stays its own.
//!
//! The tracking issues are Proposed Updates', which cpu-sig-tracker
//! files per merge request, and Hyperscale's `rfe::new-version` ones,
//! which hs-relmon files per package; see [`crate::tracking`] for
//! how one is found.

use std::cmp::Ordering;

use crate::tags::{TagChange, tag_and_untag};
use crate::tracking::{
    Provenance, UntagReason, proposed_release, requested_version, stage_label, stage_note,
    stage_noted, tracked_mr_url, tracks_proposed_branch, untag_label, untag_note, untag_noted,
};
use crate::{Sig, Stage};
use sandogasa_gitlab::{Client, IssueUpdate};

/// What happened to a build that an issue is told of.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// It reached a stage.
    Reached(Stage),
    /// It was untagged from a tag, for a reason the comment gives.
    Untagged(String),
}

/// One comment to post, and the label to set with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    /// The build and what happened to it, to post it only once it has.
    pub nvr: String,
    pub event: Event,
    pub path: String,
    pub iid: u64,
    pub issue_url: String,
    pub note: String,
    /// The `stage::` label, when the stage has one and the project or
    /// one of its groups has it: added by name, GitLab would otherwise
    /// make a project label of it.
    pub label: Option<&'static str>,
    /// The `stage::` label the stage has but nothing defines yet.
    pub missing_label: Option<&'static str>,
    /// A label to take off the issue: `stage::rejected`, once a build
    /// reaches a stage again, which `-candidate`, having no label of
    /// its own, would not replace.
    pub unlabel: Option<&'static str>,
    /// The version the issue asks for, for a Hyperscale one: a build of
    /// an older version is not what it is about, and gets no comment.
    pub at_least: Option<String>,
    /// For a Hyperscale release: when to close the issue.
    pub closing: Option<Closing>,
    /// The tool doing the write, whose `config` sets a GitLab token.
    pub tool: &'static str,
}

/// When a Hyperscale issue is done: hs-relmon files one per package,
/// asking for a version, and it is done once every release the SIG
/// carries the package for has that version or a newer one released.
/// The release being promoted counts once it is tagged; a release still
/// behind leaves the issue open for hs-relmon's next scan to close.
#[derive(Debug, Clone, PartialEq)]
pub struct Closing {
    pub package: String,
    pub version: String,
    /// The `-release` tag of each release the package has a SIG branch
    /// for.
    pub tags: Vec<String>,
}

/// The newest build of a package in each of some tags, one entry per
/// tag: [`crate::packages::latest_in_tags`], or a stand-in.
pub type LatestIn<'a> = &'a dyn Fn(&[String], &str) -> Result<Vec<Option<String>>, String>;

/// One answer per item of a list, read in one go: the commits builds
/// were made from ([`crate::packages::build_commits`]), the builds tasks
/// made ([`crate::packages::task_builds`]), or a stand-in.
pub type ReadEach<'a> = &'a dyn Fn(&[String]) -> Result<Vec<Option<String>>, String>;

/// What the tracking-issue comments need from the package's checkout,
/// which the tool doing the CBS write has (`cbspkg`'s git checkout), or
/// a stand-in.
pub trait Checkout {
    /// Fetch what the checkout lacks to compare `commits` (the builds')
    /// with the merge request `mr`. It runs before anything is
    /// confirmed, so it changes no branch and no working tree: only what
    /// the repository has fetched.
    fn fetch_missing(&self, mr: &MergeRequest, commits: &[Option<String>]);
    /// What `commit` is against the merge request's `head`.
    fn provenance(&self, head: &str, commit: &str) -> Provenance;
    /// The `-release` tags of the Hyperscale releases the package has a
    /// SIG branch for, newest first.
    fn release_tags(&self) -> Vec<String>;
}

/// The SIG branch of a package whose builds move, and its checkout.
#[derive(Clone, Copy)]
pub struct Branch<'a> {
    pub sig: Sig,
    pub package: &'a str,
    pub branch: &'a str,
    pub checkout: &'a dyn Checkout,
    /// A dry run: nothing is fetched into the checkout, so what a build
    /// was made from is told only from what it has.
    pub dry_run: bool,
    /// The tool doing the write, whose `config` sets a GitLab token.
    pub tool: &'static str,
}

/// What to tell the tracking issue of `branch` of `package`, per
/// promotion in `steps` (`(nvr, stage, commands)`), leaving out what it
/// has been told already. Nothing for a SIG this does not cover yet, or
/// without an open issue for the branch, and a warning when GitLab
/// cannot be read.
///
/// Each comment says what the build was made from: `commits_of` gives
/// the commits CBS built, for all the builds at once
/// (`crate::packages::build_commits`), and
/// the checkout ([`Checkout`]) compares it with the merge request's head, so
/// SIG-only commits on top are listed.
pub fn plan(
    base: &str,
    token: Option<&str>,
    on: &Branch,
    steps: &[(String, Stage, Vec<TagChange>)],
    commits_of: ReadEach,
) -> Vec<Notice> {
    let Branch {
        sig,
        package,
        checkout,
        dry_run,
        tool,
        ..
    } = *on;
    if steps.is_empty() {
        return Vec::new();
    }
    let nvrs: Vec<&str> = steps.iter().map(|(nvr, _, _)| nvr.as_str()).collect();
    let Some(Found {
        client,
        issue,
        notes,
        path,
    }) = find_issue(base, token, on, &nvrs)
    else {
        return Vec::new();
    };
    let at_least = requested_at_least(sig, &issue, package);
    let rejected = issue.labels.iter().any(|l| l == REJECTED);
    let pending: Vec<&(String, Stage, Vec<TagChange>)> = steps
        .iter()
        .filter(|(nvr, stage, _)| {
            // A build's NVR is a stand-in until it is built.
            if let Some(v) = &at_least
                && !nvr.starts_with('<')
                && !reaches(nvr, v)
            {
                eprintln!(
                    "{nvr} is older than the {v} {} asks for: no comment to post.",
                    issue.web_url
                );
                return false;
            }
            let told = stage_noted(notes.iter().map(|n| n.body.as_str()), nvr, *stage);
            if told {
                eprintln!(
                    "{} says {nvr} reached -{} already.",
                    issue.web_url,
                    stage.as_str()
                );
            }
            !told
        })
        .collect();
    // What each build was made from, against the merge request's head:
    // read only when there is something to tell.
    let mr = if pending.is_empty() {
        None
    } else {
        issue
            .description
            .as_deref()
            .and_then(tracked_mr_url)
            .and_then(|url| match merge_request_of(url, token) {
                Ok(mr) => Some(mr),
                Err(e) => {
                    eprintln!(
                        "warning: cannot read {url} ({e}); the comment cannot say what the build \
                         carries"
                    );
                    None
                }
            })
    };
    // Asked for all the builds at once (#144), and only when there is
    // something to tell.
    let pending_nvrs: Vec<String> = pending.iter().map(|(nvr, _, _)| nvr.clone()).collect();
    let commits: Vec<Option<String>> = if pending_nvrs.is_empty() {
        Vec::new()
    } else {
        match commits_of(&pending_nvrs) {
            Ok(c) if c.len() == pending_nvrs.len() => c,
            Ok(_) => {
                eprintln!(
                    "warning: cannot tell what the builds were made from (an answer is missing)"
                );
                vec![None; pending_nvrs.len()]
            }
            Err(e) => {
                eprintln!("warning: cannot tell what the builds were made from ({e})");
                vec![None; pending_nvrs.len()]
            }
        }
    };
    if let Some(mr) = &mr
        && !dry_run
    {
        checkout.fetch_missing(mr, &commits);
    }
    let earlier: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
    // The releases a Hyperscale release closes the issue for, read once.
    let release_tags = if at_least.is_some() && pending.iter().any(|(_, s, _)| *s == Stage::Release)
    {
        checkout.release_tags()
    } else {
        Vec::new()
    };
    let mut notices = Vec::new();
    for ((nvr, stage, changes), commit) in pending.into_iter().zip(&commits) {
        let (tag, untagged) = tag_and_untag(changes);
        // Re-promoting a released build only finishes its untag from
        // -testing; the release tag is the testing one's sibling.
        let tag = match (tag, untagged, stage) {
            (Some(t), _, _) => t.to_string(),
            (None, Some(testing), Stage::Release) => match testing.strip_suffix("-testing") {
                Some(prefix) => format!("{prefix}-release"),
                None => continue,
            },
            _ => continue,
        };
        notices.push(Notice {
            nvr: nvr.clone(),
            event: Event::Reached(*stage),
            path: path.clone(),
            iid: issue.iid,
            issue_url: issue.web_url.clone(),
            note: stage_note(
                nvr,
                *stage,
                &tag,
                untagged,
                &match (&mr, commit) {
                    (Some(mr), Some(commit)) => checkout.provenance(&mr.head, commit),
                    _ => Provenance::Unknown,
                },
                &earlier,
            ),
            label: stage_label(*stage),
            missing_label: None,
            unlabel: rejected.then_some(REJECTED),
            closing: match (&at_least, stage) {
                (Some(version), Stage::Release) if !release_tags.is_empty() => Some(Closing {
                    package: package.to_string(),
                    version: version.clone(),
                    tags: release_tags.clone(),
                }),
                _ => None,
            },
            at_least: at_least.clone(),
            tool,
        });
    }
    // Which stage labels exist, read only when one is wanted.
    check_labels(&client, &path, &mut notices);
    notices
}

/// The label a rejected build leaves on its issue
/// ([`crate::tracking::untag_label`]).
const REJECTED: &str = "stage::rejected";

/// The version a Hyperscale issue asks for, which a build has to reach
/// for the issue to be told of it; `None` for Proposed Updates.
fn requested_at_least(sig: Sig, issue: &sandogasa_gitlab::Issue, package: &str) -> Option<String> {
    match sig {
        Sig::Hyperscale => requested_version(&issue.title, package).map(str::to_string),
        Sig::ProposedUpdates => None,
    }
}

/// A tracking issue found for a branch: its client, the issue and its
/// comments, and the project's path.
struct Found {
    client: Client,
    issue: sandogasa_gitlab::Issue,
    notes: Vec<sandogasa_gitlab::Note>,
    path: String,
}

/// The open tracking issue of `on`'s branch, for builds `nvrs` (a
/// Hyperscale package's issue is the one asking for the newest version
/// they reach), with its comments; `None`, saying why, when there is
/// none or GitLab cannot be read.
fn find_issue(base: &str, token: Option<&str>, on: &Branch, nvrs: &[&str]) -> Option<Found> {
    let Branch {
        sig,
        package,
        branch,
        ..
    } = *on;
    // Which issues to read, and which of them is this branch's.
    let labels = match sig {
        // The release's issues only: GitLab ANDs the labels.
        Sig::ProposedUpdates => format!("cpu-sig-tracker,{}", proposed_release(branch)?),
        Sig::Hyperscale => "rfe::new-version".to_string(),
    };
    let tracks = |i: &sandogasa_gitlab::Issue| match sig {
        Sig::ProposedUpdates => {
            tracks_proposed_branch(&i.labels, i.description.as_deref().unwrap_or(""), branch)
        }
        Sig::Hyperscale => requested_version(&i.title, package).is_some(),
    };
    let path = format!("{}/{package}", sig.gitlab_group());
    let read = Client::new(base, &path, token.unwrap_or("")).and_then(|client| {
        let issues: Vec<sandogasa_gitlab::Issue> = client
            .list_issues(&labels, Some("opened"))?
            .into_iter()
            .filter(|i| tracks(i))
            .collect();
        let Some(issue) = pick_issue(issues, sig, package, nvrs) else {
            return Ok(None);
        };
        let notes = client.issue_notes(issue.iid)?;
        Ok(Some((client, issue, notes)))
    });
    match read {
        Ok(Some((client, issue, notes))) => Some(Found {
            client,
            issue,
            notes,
            path,
        }),
        Ok(None) => {
            eprintln!("{path} has no open tracking issue for {branch}: no comment to post.");
            None
        }
        Err(e) => {
            eprintln!(
                "warning: cannot read {path}'s tracking issues ({e}); no comment will be posted"
            );
            None
        }
    }
}

/// Keep the `stage::` label of each of `notices` that the project at
/// `path`, or one of its groups, has; one nothing defines yet is set
/// aside as missing, as adding it by name would make a project label.
fn check_labels(client: &Client, path: &str, notices: &mut [Notice]) {
    if !notices.iter().any(|n| n.label.is_some()) {
        return;
    }
    let labels = client.label_names().unwrap_or_else(|e| {
        eprintln!("warning: cannot read {path}'s labels ({e}); no stage label is set");
        Vec::new()
    });
    for n in notices {
        if let Some(wanted) = n.label
            && !labels.iter().any(|l| l == wanted)
        {
            n.label = None;
            n.missing_label = Some(wanted);
        }
    }
}

/// What to tell the tracking issue of `on`'s branch of the builds in
/// `untags` (`(nvr, tag)`), untagged for `reason`, leaving out what it
/// has been told already: a comment saying why, and the `stage::` label
/// the reason gives (`stage::rejected`, `stage::retired`). Housekeeping
/// untags, which say nothing about the update, are not planned (#144).
pub fn plan_untags(
    base: &str,
    token: Option<&str>,
    on: &Branch,
    untags: &[(String, String)],
    reason: &UntagReason,
) -> Vec<Notice> {
    if untags.is_empty() {
        return Vec::new();
    }
    let nvrs: Vec<&str> = untags.iter().map(|(nvr, _)| nvr.as_str()).collect();
    let Some(Found {
        client,
        issue,
        notes,
        path,
    }) = find_issue(base, token, on, &nvrs)
    else {
        return Vec::new();
    };
    // As for a stage reached: a Hyperscale build older than the version
    // its issue asks for is not what the issue is about.
    let at_least = requested_at_least(on.sig, &issue, on.package);
    let mut notices: Vec<Notice> = untags
        .iter()
        .filter(|(nvr, _)| match &at_least {
            Some(v) if !reaches(nvr, v) => {
                eprintln!(
                    "{nvr} is older than the {v} {} asks for: no comment to post.",
                    issue.web_url
                );
                false
            }
            _ => true,
        })
        .filter(|(nvr, tag)| {
            let told = untag_noted(notes.iter().map(|n| n.body.as_str()), nvr, tag);
            if told {
                eprintln!("{} says {nvr} left {tag} already.", issue.web_url);
            }
            !told
        })
        .map(|(nvr, tag)| Notice {
            nvr: nvr.clone(),
            event: Event::Untagged(tag.clone()),
            path: path.clone(),
            iid: issue.iid,
            issue_url: issue.web_url.clone(),
            note: untag_note(nvr, tag, reason),
            label: untag_label(reason),
            missing_label: None,
            unlabel: None,
            at_least: None,
            closing: None,
            tool: on.tool,
        })
        .collect();
    check_labels(&client, &path, &mut notices);
    notices
}

/// `text` with control characters other than tabs replaced, so that
/// text from git, GitLab or the person running the tool cannot move the
/// cursor or change the colors of the plan it is shown in.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() && c != '\t' {
                '\u{fffd}'
            } else {
                c
            }
        })
        .collect()
}

/// How the plan shows a notice: the issue, the comment indented, and
/// the label; without a token to post with, that it will not be told.
/// Plain text; [`describe_styled`] colors it for a terminal.
pub fn describe(notice: &Notice, postable: bool) -> String {
    describe_styled(notice, postable, false)
}

/// [`describe`], colored when `color` is on, as the workspace's color
/// chart has it: the heading bold, or yellow when the comment cannot be
/// posted; the quote marks and the comment's hidden marker gray, as
/// secondary; a missing label yellow.
pub fn describe_styled(notice: &Notice, postable: bool, color: bool) -> String {
    use sandogasa_cli::style::{BOLD, DIM, YELLOW, paint};
    let url = printable(&notice.issue_url);
    let mut out = if postable {
        format!(
            "{}\n",
            paint(&format!("Then comment on {url}:"), BOLD, color)
        )
    } else {
        format!(
            "{}\n",
            paint(
                &format!(
                    "Without a GitLab token (`{} config`, or GITLAB_TOKEN), no comment is posted on {url}:",
                    notice.tool
                ),
                YELLOW,
                color
            )
        )
    };
    for line in notice.note.lines() {
        let line = printable(line);
        if line.trim_start().starts_with("<!-- sandogasa:") {
            out.push_str(&format!("  {}\n", paint(&format!("> {line}"), DIM, color)));
        } else {
            out.push_str(&format!("  {} {line}\n", paint(">", DIM, color)));
        }
    }
    if let Some(label) = notice.label {
        out.push_str(&format!("  and label it {label}\n"));
    }
    if let Some(label) = notice.unlabel {
        out.push_str(&format!("  and take off {label}\n"));
    }
    if let Some(c) = notice.closing.as_ref().filter(|_| postable) {
        out.push_str(&format!(
            "  and close it as Done if {} then {} {} or newer\n",
            c.tags.join(", "),
            if c.tags.len() == 1 { "has" } else { "all have" },
            c.version
        ));
    }
    if let Some(label) = notice.missing_label {
        out.push_str(&format!(
            "  {}\n",
            paint(
                &format!(
                    "(no {label} label exists in the project or the SIG's group: \
                     create one in the group for the issue to get it)"
                ),
                YELLOW,
                color
            )
        ));
    }
    out
}

/// Post the notices of `notices` whose event happened, as `done` says
/// (a stage reached, a tag left), warning about each part that fails:
/// the comment, then the labels. A stage reached also moves the work
/// item from `To do` to `In progress`, and a Hyperscale release closes
/// its issue when every release has the version (`latest` reads what
/// they have); an untag leaves the status to the tracker.
pub fn post(
    base: &str,
    token: Option<&str>,
    notices: &[Notice],
    done: &[(String, Event)],
    latest: LatestIn,
) {
    let notices: Vec<&Notice> = notices
        .iter()
        .filter(|n| {
            done.iter()
                .any(|(nvr, event)| *nvr == n.nvr && *event == n.event)
        })
        .filter(|n| match &n.at_least {
            Some(v) if !reaches(&n.nvr, v) => {
                eprintln!(
                    "{} is older than the {v} {} asks for: no comment is posted.",
                    n.nvr, n.issue_url
                );
                false
            }
            _ => true,
        })
        .collect();
    if notices.is_empty() {
        return;
    }
    let Some(token) = token else {
        eprintln!(
            "No GitLab token, so the tracking issue is not updated: \
             set one with `{} config` or GITLAB_TOKEN.",
            notices[0].tool
        );
        return;
    };
    for notice in notices {
        let client = match Client::new(base, &notice.path, token) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: cannot reach {} ({e})", notice.path);
                continue;
            }
        };
        // Closed first, so the comment says it is done only once it is.
        let done_note = notice
            .closing
            .as_ref()
            .and_then(|c| done_note(c, &notice.issue_url, latest))
            .filter(|_| close_as_done(&client, notice));
        let note = match &done_note {
            Some(para) => with_paragraph(&notice.note, para),
            None => notice.note.clone(),
        };
        match client.add_note(notice.iid, &note) {
            Ok(()) => eprintln!("Commented on {}.", notice.issue_url),
            Err(e) => {
                eprintln!(
                    "{}",
                    unposted(&notice.issue_url, done_note.is_some(), &e, &note)
                );
                continue;
            }
        }
        if notice.label.is_some() || notice.unlabel.is_some() {
            let update = IssueUpdate {
                add_labels: notice.label.map(str::to_string),
                remove_labels: notice.unlabel.map(str::to_string),
                ..Default::default()
            };
            if let Err(e) = client.edit_issue(notice.iid, &update) {
                eprintln!("warning: could not relabel {} ({e})", notice.issue_url);
            }
        }
        // An untag leaves the work item's status to the tracker.
        if done_note.is_some() || !matches!(notice.event, Event::Reached(_)) {
            continue;
        }
        // A tracked update being promoted is in progress, whatever the
        // issue says; a later status (Done) is left alone.
        match client.get_work_item_status(notice.iid) {
            Ok(Some(status)) if status == "To do" => {
                if let Err(e) = client.set_work_item_status(notice.iid, "In progress") {
                    eprintln!(
                        "warning: could not set {} to In progress ({e})",
                        notice.issue_url
                    );
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!(
                "warning: could not read {}'s status ({e}); it is left as it is",
                notice.issue_url
            ),
        }
    }
}

/// Whether there are tracking issues for `sig` to tell: Proposed
/// Updates' and Hyperscale's.
pub fn covers(sig: Sig) -> bool {
    matches!(sig, Sig::ProposedUpdates | Sig::Hyperscale)
}

/// The CBS hub's newest builds, for [`post`].
pub fn cbs_latest(tags: &[String], package: &str) -> Result<Vec<Option<String>>, String> {
    crate::packages::latest_in_tags(tags, package)
}

/// Which of `issues`, each tracking the branch, to tell of `nvrs`. A
/// Proposed Update has one per branch. Hyperscale may have several
/// `rfe::new-version` issues open, one per version hs-relmon saw: the
/// one asking for the newest version the builds reach, else the first.
fn pick_issue(
    issues: Vec<sandogasa_gitlab::Issue>,
    sig: Sig,
    package: &str,
    nvrs: &[&str],
) -> Option<sandogasa_gitlab::Issue> {
    if sig != Sig::Hyperscale || issues.len() < 2 {
        return issues.into_iter().next();
    }
    let reached = |i: &sandogasa_gitlab::Issue| {
        requested_version(&i.title, package)
            .filter(|v| nvrs.iter().any(|nvr| reaches(nvr, v)))
            .map(str::to_string)
    };
    let best = issues
        .iter()
        .enumerate()
        .filter_map(|(n, i)| Some((reached(i)?, n)))
        .max_by(|a, b| sandogasa_rpmvercmp::rpmvercmp(&a.0, &b.0))
        .map(|(_, n)| n)
        .unwrap_or(0);
    issues.into_iter().nth(best)
}

/// The version of an NVR: `7.1` of `ethtool-7.1-1.hs.el9`. A build's
/// stand-in, `<the build in …>`, has none.
fn version_of(nvr: &str) -> Option<&str> {
    if nvr.starts_with('<') {
        return None;
    }
    let mut parts = nvr.rsplitn(3, '-');
    let (_release, version, _name) = (parts.next()?, parts.next()?, parts.next()?);
    Some(version)
}

/// Whether `nvr` is at or past `version`.
fn reaches(nvr: &str, version: &str) -> bool {
    version_of(nvr).is_some_and(|v| sandogasa_rpmvercmp::rpmvercmp(v, version) != Ordering::Less)
}

/// A tracked merge request: where its commits are fetched from, and its
/// head.
pub struct MergeRequest {
    /// The repository to fetch from.
    pub repo_url: String,
    /// The merge request's ref there.
    pub mr_ref: String,
    /// Its head commit.
    pub head: String,
}

/// The merge request at `url`, read from its own GitLab with `token`.
fn merge_request_of(
    url: &str,
    token: Option<&str>,
) -> Result<MergeRequest, Box<dyn std::error::Error>> {
    let (base, project, iid) = sandogasa_gitlab::parse_mr_url(url)?;
    let mr = Client::new(&base, &project, token.unwrap_or(""))?.merge_request(iid)?;
    let head = mr.sha.ok_or("GitLab gives no head commit")?;
    Ok(MergeRequest {
        repo_url: format!("{base}/{project}.git"),
        mr_ref: format!("refs/merge-requests/{iid}/head"),
        head,
    })
}

/// The paragraph saying a Hyperscale issue is done, when every tag of
/// `closing` has its version or newer; `None`, saying why, when one is
/// behind or cannot be read.
fn done_note(closing: &Closing, issue_url: &str, latest: LatestIn) -> Option<String> {
    let found = match latest(&closing.tags, &closing.package) {
        Ok(found) => found,
        Err(e) => {
            eprintln!(
                "warning: cannot read what the release tags have ({e}); {issue_url} stays open"
            );
            return None;
        }
    };
    if found.len() != closing.tags.len() {
        eprintln!(
            "warning: the hub answered {} of {} release tags; {issue_url} stays open",
            found.len(),
            closing.tags.len()
        );
        return None;
    }
    let mut lines = Vec::new();
    for (tag, nvr) in closing.tags.iter().zip(&found) {
        match nvr {
            Some(nvr) if reaches(nvr, &closing.version) => {
                lines.push(format!("- `{tag}`: `{nvr}`"));
            }
            Some(nvr) => {
                eprintln!(
                    "{issue_url} stays open: {tag} has {nvr}, older than {}.",
                    closing.version
                );
                return None;
            }
            None => {
                eprintln!(
                    "{issue_url} stays open: {tag} has no build of {}.",
                    closing.package
                );
                return None;
            }
        }
    }
    Some(format!(
        "Every release now has {} or newer, so this is done:\n\n{}",
        closing.version,
        lines.join("\n")
    ))
}

/// `note` with `paragraph` added before its closing marker.
fn with_paragraph(note: &str, paragraph: &str) -> String {
    match note.rfind("\n\n<!-- sandogasa:") {
        Some(at) => format!("{}\n\n{paragraph}{}", &note[..at], &note[at..]),
        None => format!("{note}\n\n{paragraph}"),
    }
}

/// The warning for a comment that could not be posted, with the
/// comment, so it can be posted by hand: commenting on a closed issue
/// works. An issue `closed` just now has nothing else saying why.
fn unposted(issue_url: &str, closed: bool, error: &impl std::fmt::Display, note: &str) -> String {
    let why = if closed {
        "; it was closed, and this comment says why, so post it by hand"
    } else {
        "; to post it by hand"
    };
    format!("warning: could not comment on {issue_url} ({error}){why}:\n\n{note}\n")
}

/// Close the issue of `notice` and set it to Done, warning about each
/// part that fails; whether it was closed. One left open is hs-relmon's
/// to close at its next scan.
fn close_as_done(client: &Client, notice: &Notice) -> bool {
    let close = IssueUpdate {
        state_event: Some("close".to_string()),
        ..Default::default()
    };
    if let Err(e) = client.edit_issue(notice.iid, &close) {
        eprintln!(
            "warning: could not close {} ({e}); hs-relmon closes it at its next scan",
            notice.issue_url
        );
        return false;
    }
    eprintln!("Closed {}.", notice.issue_url);
    if let Err(e) = client.set_work_item_status(notice.iid, "Done") {
        eprintln!("warning: could not set {} to Done ({e})", notice.issue_url);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout that has nothing: no commit to compare, and the given
    /// release branches.
    struct NoCheckout(&'static [&'static str]);

    impl Checkout for NoCheckout {
        fn fetch_missing(&self, _: &MergeRequest, _: &[Option<String>]) {}
        fn provenance(&self, _: &str, _: &str) -> Provenance {
            Provenance::Unknown
        }
        fn release_tags(&self) -> Vec<String> {
            self.0.iter().map(|t| t.to_string()).collect()
        }
    }

    /// git's branch `branch` in `sig`, from a checkout that is not there.
    fn on<'a>(sig: Sig, branch: &'a str) -> Branch<'a> {
        Branch {
            sig,
            package: "git",
            branch,
            checkout: &NoCheckout(&[]),
            dry_run: true,
            tool: "cbspkg",
        }
    }

    /// Builds whose sources name no commit.
    fn none(nvrs: &[String]) -> Result<Vec<Option<String>>, String> {
        Ok(vec![None; nvrs.len()])
    }

    fn no_latest(_: &[String], _: &str) -> Result<Vec<Option<String>>, String> {
        panic!("only a Hyperscale release reads the release tags")
    }

    const PATH: &str = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fgit";
    const NVR: &str = "git-2.52.1-1~proposed.el10";
    const TESTING: &str = "proposed_updates10s-packages-main-testing";
    const RELEASE: &str = "proposed_updates10s-packages-main-release";

    fn step(stage: Stage) -> (String, Stage, Vec<TagChange>) {
        let changes = match stage {
            Stage::Release => vec![TagChange::tag(RELEASE, NVR), TagChange::untag(TESTING, NVR)],
            _ => vec![TagChange::tag(TESTING, NVR)],
        };
        (NVR.to_string(), stage, changes)
    }

    /// git's c10s Proposed Update, its tracking issue #1 telling of a
    /// testing build already when `told` is, and the group having
    /// `labels`.
    fn serve(server: &mut mockito::Server, told: bool, labels: &str) {
        server
            .mock("GET", format!("{PATH}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([
                    {"iid": 2, "title": "git c9s", "state": "opened", "web_url": "u2",
                     "labels": ["c9s", "cpu-sig-tracker"],
                     "description": "- **JIRA**: [RHEL-306262](j) — New"},
                    {"iid": 1, "title": "git c10s", "state": "opened", "web_url": "u1",
                     "labels": ["c10s", "cpu-sig-tracker"],
                     "description": "- **JIRA**: [RHEL-306261](j) — New"},
                ])
                .to_string(),
            )
            .create();
        let earlier = if told {
            stage_note(
                NVR,
                Stage::Testing,
                TESTING,
                None,
                &Provenance::Unknown,
                &[],
            )
        } else {
            "set status to **In progress**".to_string()
        };
        server
            .mock("GET", format!("{PATH}/issues/1/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(serde_json::json!([{"id": 9, "body": earlier}]).to_string())
            .create();
        server
            .mock("GET", format!("{PATH}/labels").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(labels)
            .create();
    }

    /// git's c10s build rejected in review: its issue is told why, and
    /// labeled `stage::rejected`; an issue told already is not told
    /// again, and posting it leaves the work item's status alone.
    #[test]
    fn an_untag_is_told_to_its_tracking_issue_once() {
        let mut server = mockito::Server::new();
        serve(&mut server, false, r#"[{"name": "stage::rejected"}]"#);
        let branch = "c10s-proposed-RHEL-306261";
        let untags = [(NVR.to_string(), TESTING.to_string())];
        let notices = plan_untags(
            &server.url(),
            Some("t"),
            &on(Sig::ProposedUpdates, branch),
            &untags,
            &UntagReason::Rejected,
        );
        assert_eq!(notices.len(), 1);
        let n = &notices[0];
        assert_eq!(n.event, Event::Untagged(TESTING.to_string()));
        assert_eq!(n.iid, 1);
        assert_eq!(n.label, Some("stage::rejected"));
        assert!(n.note.contains("rejected in review"), "{}", n.note);
        assert!(describe(n, true).contains("and label it stage::rejected"));
        // Colored for a terminal: the heading bold, the quote marks and
        // the hidden marker gray; plain otherwise.
        use sandogasa_cli::style::{BOLD, DIM, RESET, YELLOW};
        let styled = describe_styled(n, true, true);
        assert!(
            styled.starts_with(&format!("{BOLD}Then comment on")),
            "{styled}"
        );
        assert!(styled.contains(&format!("  {DIM}>{RESET} ")), "{styled}");
        assert!(
            styled.contains(&format!("  {DIM}> <!-- sandogasa:")),
            "{styled}"
        );
        assert_eq!(describe_styled(n, true, false), describe(n, true));
        assert!(
            describe_styled(n, false, true).starts_with(&format!("{YELLOW}Without a GitLab token"))
        );
        // Only the tool's own marker is grayed, and escape sequences
        // from the comment's text do not reach the terminal.
        assert_eq!(printable("a\x1b[0mb\tc"), "a\u{fffd}[0mb\tc");
        let mut other = n.clone();
        other.note = "<!-- see bug -->\nx\x1b[2Jy".to_string();
        let styled = describe_styled(&other, true, true);
        assert!(
            styled.contains(&format!("  {DIM}>{RESET} <!-- see bug -->")),
            "{styled}"
        );
        assert!(!styled.contains("\x1b[2J"), "{styled}");
        // Nothing to untag: nothing read.
        assert!(
            plan_untags(
                "http://127.0.0.1:1",
                Some("t"),
                &on(Sig::ProposedUpdates, branch),
                &[],
                &UntagReason::Rejected,
            )
            .is_empty()
        );

        // Posted: the comment and the label, and no status change.
        let comment = server
            .mock("POST", format!("{PATH}/issues/1/notes").as_str())
            .with_status(201)
            .with_body(r#"{"id": 10, "body": "b"}"#)
            .expect(1)
            .create();
        let label = server
            .mock("PUT", format!("{PATH}/issues/1").as_str())
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"add_labels": "stage::rejected"}),
            ))
            .with_status(200)
            .with_body(r#"{"iid": 1, "title": "t", "state": "opened", "web_url": "u1"}"#)
            .expect(1)
            .create();
        let status = server.mock("POST", "/api/graphql").expect(0).create();
        let done = [(NVR.to_string(), Event::Untagged(TESTING.to_string()))];
        post(&server.url(), Some("t"), &notices, &done, &no_latest);
        comment.assert();
        label.assert();
        status.assert();

        // Told already: nothing to plan.
        // The comment told mocked first: mockito answers with the first
        // mock that matches.
        let mut told = mockito::Server::new();
        told.mock("GET", format!("{PATH}/issues/1/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(serde_json::json!([{"id": 9, "body": n.note.clone()}]).to_string())
            .create();
        serve(&mut told, false, "[]");
        let again = plan_untags(
            &told.url(),
            Some("t"),
            &on(Sig::ProposedUpdates, branch),
            &untags,
            &UntagReason::Rejected,
        );
        assert!(again.is_empty());
    }

    /// A rejected update's next build: reaching a stage takes
    /// `stage::rejected` off its issue, even at `-candidate`, which has no
    /// label of its own to replace it.
    #[test]
    fn a_new_build_takes_the_rejected_label_off() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", format!("{PATH}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{"iid": 1, "title": "git c10s", "state": "opened",
                    "web_url": "u1", "labels": ["c10s", "cpu-sig-tracker", "stage::rejected"],
                    "description": "- **JIRA**: [RHEL-306261](j) — New"}])
                .to_string(),
            )
            .create();
        server
            .mock("GET", format!("{PATH}/issues/1/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        let steps = [(
            NVR.to_string(),
            Stage::Candidate,
            vec![TagChange::tag(
                "proposed_updates10s-packages-main-candidate",
                NVR,
            )],
        )];
        let notices = plan(
            &server.url(),
            Some("t"),
            &on(Sig::ProposedUpdates, "c10s-proposed-RHEL-306261"),
            &steps,
            &none,
        );
        assert_eq!(notices[0].unlabel, Some("stage::rejected"));
        assert!(describe(&notices[0], true).contains("and take off stage::rejected"));
        server
            .mock("POST", format!("{PATH}/issues/1/notes").as_str())
            .with_status(201)
            .with_body(r#"{"id": 10, "body": "b"}"#)
            .create();
        let relabel = server
            .mock("PUT", format!("{PATH}/issues/1").as_str())
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"remove_labels": "stage::rejected"}),
            ))
            .with_status(200)
            .with_body(r#"{"iid": 1, "title": "t", "state": "opened", "web_url": "u1"}"#)
            .expect(1)
            .create();
        server
            .mock("POST", "/api/graphql")
            .with_status(200)
            .with_body(r#"{"data": {"project": {"workItems": {"nodes": []}}}}"#)
            .create();
        let done = [(NVR.to_string(), Event::Reached(Stage::Candidate))];
        post(&server.url(), Some("t"), &notices, &done, &no_latest);
        relabel.assert();
    }

    #[test]
    fn a_promotion_is_told_to_its_tracking_issue_once() {
        let mut server = mockito::Server::new();
        serve(&mut server, true, r#"[{"name": "stage::testing"}]"#);
        let steps = [step(Stage::Testing), step(Stage::Release)];
        let branch = "c10s-proposed-RHEL-306261";
        let notices = plan(
            &server.url(),
            None,
            &on(Sig::ProposedUpdates, branch),
            &steps,
            &none,
        );
        // Testing was told already; the release is new, and the group
        // has no stage::release label.
        assert_eq!(notices.len(), 1);
        let release = &notices[0];
        assert_eq!((release.iid, release.issue_url.as_str()), (1, "u1"));
        assert!(
            release
                .note
                .contains("untagged from `proposed_updates10s-packages-main-testing`")
        );
        assert_eq!(release.label, None);
        assert_eq!(release.missing_label, Some("stage::release"));
        let shown = describe(release, true);
        assert!(
            shown.starts_with("Then comment on u1:\n  > `git-2.52.1-1~proposed.el10` is released"),
            "{shown}"
        );
        assert!(shown.contains("no stage::release label exists"), "{shown}");
        assert!(describe(release, false).starts_with("Without a GitLab token"));

        // A released build still in -testing, promoted again only to be
        // untagged: the issue is told of the release all the same.
        let untag_only = (
            NVR.to_string(),
            Stage::Release,
            vec![TagChange::untag(TESTING, NVR)],
        );
        let notices = plan(
            &server.url(),
            None,
            &on(Sig::ProposedUpdates, branch),
            &[untag_only],
            &none,
        );
        assert!(notices[0].note.contains(&format!(
            "tagged into `{RELEASE}` and untagged from `{TESTING}`"
        )));
        // Another branch's issue is not this one's; another SIG, or
        // nothing to promote, asks nothing.
        assert!(
            plan(
                &server.url(),
                None,
                &on(Sig::ProposedUpdates, "c10s-proposed-RHEL-1"),
                &steps,
                &none
            )
            .is_empty()
        );
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                &on(Sig::Hyperscale, branch),
                &steps,
                &none
            )
            .is_empty()
        );
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                &on(Sig::ProposedUpdates, branch),
                &[],
                &none
            )
            .is_empty()
        );
        // GitLab unreadable: a warning, nothing to tell.
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                &on(Sig::ProposedUpdates, branch),
                &steps,
                &none
            )
            .is_empty()
        );
    }

    #[test]
    fn a_notice_is_posted_labeled_and_moves_the_issue_into_progress() {
        let mut server = mockito::Server::new();
        serve(&mut server, false, r#"[{"name": "stage::testing"}]"#);
        let steps = [step(Stage::Testing)];
        let branch = "c10s-proposed-RHEL-306261";
        let notices = plan(
            &server.url(),
            Some("t"),
            &on(Sig::ProposedUpdates, branch),
            &steps,
            &none,
        );
        assert_eq!(notices[0].label, Some("stage::testing"));
        assert!(describe(&notices[0], true).contains("and label it stage::testing"));

        let comment = server
            .mock("POST", format!("{PATH}/issues/1/notes").as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "body": stage_note(NVR, Stage::Testing, TESTING, None, &Provenance::Unknown, &[]),
            })))
            .with_status(201)
            .with_body(r#"{"id": 10, "body": "b"}"#)
            .expect(1)
            .create();
        let label = server
            .mock("PUT", format!("{PATH}/issues/1").as_str())
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"add_labels": "stage::testing"}),
            ))
            .with_status(200)
            .with_body(r#"{"iid": 1, "title": "t", "state": "opened", "web_url": "u1"}"#)
            .expect(1)
            .create();
        // Status: To do, so it is set to In progress.
        let graphql = server
            .mock("POST", "/api/graphql")
            .with_status(200)
            .with_body(
                r#"{"data": {"project": {"workItems": {"nodes": [{"id": "gid://gitlab/WorkItem/5", "widgets": [{"type": "STATUS", "status": {"name": "To do"}}]}]}}}}"#,
            )
            .expect_at_least(2)
            .create();
        // Posted only for a stage the build reached.
        post(&server.url(), Some("t"), &notices, &[], &no_latest);
        let done = [(NVR.to_string(), Event::Reached(Stage::Testing))];
        post(&server.url(), Some("t"), &notices, &done, &no_latest);
        comment.assert();
        label.assert();
        graphql.assert();

        // Without a token nothing is posted.
        post(&server.url(), None, &notices, &done, &no_latest);
        comment.assert();
    }

    #[test]
    fn a_version_is_reached_by_itself_and_later_ones() {
        assert_eq!(version_of("ethtool-7.1-1.hs.el9"), Some("7.1"));
        assert_eq!(version_of("rust-foo-bar-1.2.3-2.el10"), Some("1.2.3"));
        assert_eq!(version_of("ethtool"), None);
        assert!(reaches("ethtool-7.1-1.hs.el9", "7.1"));
        assert!(reaches("ethtool-7.10-1.hs.el9", "7.9"));
        assert!(!reaches("ethtool-7.0-3.hs.el9", "7.1"));
        assert!(!reaches("<the build in hyperscale9s-packages-main>", "7.1"));
    }

    /// ethtool's Hyperscale issue asks for 7.1: a 7.1 build is told of,
    /// a 7.0 rebuild is not, and a build not yet made is planned with
    /// its stand-in, to be checked once it has its NVR.
    #[test]
    fn a_hyperscale_build_is_told_to_its_new_version_issue() {
        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fethtool";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::UrlEncoded(
                "labels".into(),
                "rfe::new-version".into(),
            ))
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 2, "title": "ethtool-7.1 is available", "state": "opened",
                    "web_url": "h2", "labels": ["rfe::new-version"],
                    "description": "```\nethtool  Upstream  7.1\n```",
                }])
                .to_string(),
            )
            .create();
        server
            .mock("GET", format!("{path}/issues/2/notes").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        server
            .mock("GET", format!("{path}/labels").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body("[]")
            .create();
        let tag = "hyperscale9s-packages-main-testing";
        let step = |nvr: &str| {
            (
                nvr.to_string(),
                Stage::Testing,
                vec![TagChange::tag(tag, nvr)],
            )
        };
        let on = Branch {
            sig: Sig::Hyperscale,
            package: "ethtool",
            branch: "c9s-hs",
            checkout: &NoCheckout(&[]),
            dry_run: true,
            tool: "cbspkg",
        };
        let steps = [
            step("ethtool-7.1-1.hs.el9"),
            step("ethtool-7.0-3.hs.el9"),
            step("<the build in hyperscale9s-packages-main>"),
        ];
        let notices = plan(&server.url(), None, &on, &steps, &none);
        let nvrs: Vec<&str> = notices.iter().map(|n| n.nvr.as_str()).collect();
        assert_eq!(
            nvrs,
            [
                "ethtool-7.1-1.hs.el9",
                "<the build in hyperscale9s-packages-main>"
            ]
        );
        assert_eq!(notices[0].issue_url, "h2");
        assert_eq!(notices[0].at_least.as_deref(), Some("7.1"));
        assert!(!notices[0].note.contains("Built from"));
        // No stage label in the Hyperscale group yet: said, not set.
        assert_eq!(notices[0].missing_label, Some("stage::testing"));

        // The build turned out older than asked for: not posted.
        let posts = server
            .mock("POST", mockito::Matcher::Any)
            .with_status(201)
            .expect(0)
            .create();
        let older = Notice {
            nvr: "ethtool-7.0-4.hs.el9".to_string(),
            ..notices.into_iter().nth(1).unwrap()
        };
        let done = [(older.nvr.clone(), Event::Reached(Stage::Testing))];
        post(&server.url(), Some("t"), &[older], &done, &no_latest);
        posts.assert();

        // An untag is held to the same version: the 7.0 build's says
        // nothing to the issue asking for 7.1.
        let untags = [
            ("ethtool-7.0-3.hs.el9".to_string(), "t".to_string()),
            ("ethtool-7.1-1.hs.el9".to_string(), "t".to_string()),
        ];
        let notices = plan_untags(&server.url(), None, &on, &untags, &UntagReason::Retired);
        let nvrs: Vec<&str> = notices.iter().map(|n| n.nvr.as_str()).collect();
        assert_eq!(nvrs, ["ethtool-7.1-1.hs.el9"]);
    }

    /// ethtool 7.1 released to Hyperscale 10, with the checkout having
    /// branches for 9 and 10: the issue closes only once 9 has it too.
    #[test]
    fn a_hyperscale_release_closes_the_issue_once_every_release_has_the_version() {
        let r10 = "hyperscale10s-packages-main-release";
        let r9 = "hyperscale9s-packages-main-release";
        let checkout = NoCheckout(&[
            "hyperscale10s-packages-main-release",
            "hyperscale9s-packages-main-release",
        ]);

        let mut server = mockito::Server::new();
        let path = "/api/v4/projects/CentOS%2FHyperscale%2Frpms%2Fethtool";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 2, "title": "ethtool-7.1 is available", "state": "opened",
                    "web_url": "h2", "labels": ["rfe::new-version"],
                }])
                .to_string(),
            )
            .create();
        for list in ["issues/2/notes", "labels"] {
            server
                .mock("GET", format!("{path}/{list}").as_str())
                .match_query(mockito::Matcher::Any)
                .with_status(200)
                .with_body("[]")
                .create();
        }
        let nvr = "ethtool-7.1-1.hs.el10";
        let steps = [(
            nvr.to_string(),
            Stage::Release,
            vec![
                TagChange::tag(r10, nvr),
                TagChange::untag("hyperscale10s-packages-main-testing", nvr),
            ],
        )];
        let on = Branch {
            sig: Sig::Hyperscale,
            package: "ethtool",
            branch: "c10s-sig-hyperscale",
            checkout: &checkout,
            dry_run: true,
            tool: "cbspkg",
        };
        let notices = plan(&server.url(), Some("t"), &on, &steps, &none);
        assert_eq!(
            notices[0].closing,
            Some(Closing {
                package: "ethtool".into(),
                version: "7.1".into(),
                tags: vec![r10.into(), r9.into()],
            })
        );
        assert!(
            describe(&notices[0], true).contains(&format!(
                "and close it as Done if {r10}, {r9} then all have 7.1 or newer"
            )),
            "{}",
            describe(&notices[0], true)
        );
        assert!(!describe(&notices[0], false).contains("close it"));
        let done = [(nvr.to_string(), Event::Reached(Stage::Release))];

        // Hyperscale 9 is still at 7.0: commented on, left open.
        let behind = server
            .mock("POST", format!("{path}/issues/2/notes").as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "body": notices[0].note,
            })))
            .with_status(201)
            .with_body(r#"{"id": 10, "body": "b"}"#)
            .expect(4)
            .create();
        let graphql = server
            .mock("POST", "/api/graphql")
            .with_status(200)
            .with_body(
                r#"{"data": {"project": {"workItems": {"nodes": [{"id": "gid://gitlab/WorkItem/5", "widgets": [{"type": "STATUS", "status": {"name": "In progress"}}]}]}}}}"#,
            )
            .expect_at_least(1)
            .create();
        let close = server
            .mock("PUT", format!("{path}/issues/2").as_str())
            .match_body(mockito::Matcher::PartialJson(
                serde_json::json!({"state_event": "close"}),
            ))
            .with_status(200)
            .with_body(r#"{"iid": 2, "title": "t", "state": "closed", "web_url": "h2"}"#)
            .expect(1)
            .create();
        let latest = |have9: Option<&'static str>| {
            move |tags: &[String], package: &str| {
                assert_eq!(package, "ethtool");
                assert_eq!(tags.len(), 2);
                Ok(vec![
                    Some("ethtool-7.1-1.hs.el10".to_string()),
                    have9.map(String::from),
                ])
            }
        };
        post(
            &server.url(),
            Some("t"),
            &notices,
            &done,
            &latest(Some("ethtool-7.0-3.hs.el9")),
        );
        // Without a build in 9, or with the hub unreadable: the same.
        post(&server.url(), Some("t"), &notices, &done, &latest(None));
        post(
            &server.url(),
            Some("t"),
            &notices,
            &done,
            &|_: &[String], _: &str| Err("hub down".to_string()),
        );
        // An answer for one of the two releases only.
        post(
            &server.url(),
            Some("t"),
            &notices,
            &done,
            &|_: &[String], _: &str| Ok(vec![Some("ethtool-7.1-1.hs.el10".to_string())]),
        );
        behind.assert();

        // Hyperscale 9 has 7.2: the comment says so, and the issue is
        // set to Done and closed.
        let mut with_done = notices[0].note.clone();
        let marker = with_done.rfind("\n\n<!-- sandogasa:").unwrap();
        with_done.insert_str(
            marker,
            &format!(
                "\n\nEvery release now has 7.1 or newer, so this is done:\n\n\
                 - `{r10}`: `ethtool-7.1-1.hs.el10`\n- `{r9}`: `ethtool-7.2-1.hs.el9`"
            ),
        );
        let caught_up = server
            .mock("POST", format!("{path}/issues/2/notes").as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "body": with_done,
            })))
            .with_status(201)
            .with_body(r#"{"id": 11, "body": "b"}"#)
            .expect(1)
            .create();
        post(
            &server.url(),
            Some("t"),
            &notices,
            &done,
            &latest(Some("ethtool-7.2-1.hs.el9")),
        );
        caught_up.assert();
        close.assert();
        graphql.assert();
        assert_eq!(with_paragraph("no marker", "p"), "no marker\n\np");
    }

    #[test]
    fn an_unposted_comment_is_printed_to_post_by_hand() {
        let note = "Released.\n\n<!-- sandogasa:cbspkg -->";
        let closed = unposted("h2", true, &"HTTP 502", note);
        assert!(
            closed.starts_with(
                "warning: could not comment on h2 (HTTP 502); it was closed, \
                 and this comment says why, so post it by hand:\n\n"
            ),
            "{closed}"
        );
        // The whole comment, marker included, so a hand-posted one is
        // still recognized.
        assert!(closed.ends_with(&format!("\n\n{note}\n")), "{closed}");
        let open = unposted("h2", false, &"HTTP 502", note);
        assert!(open.contains("(HTTP 502); to post it by hand:"), "{open}");
    }

    #[test]
    fn of_several_new_version_issues_the_newest_the_build_reaches_is_told() {
        let issue = |iid: u64, version: &str| -> sandogasa_gitlab::Issue {
            serde_json::from_value(serde_json::json!({
                "iid": iid, "title": format!("ethtool-{version} is available"),
                "state": "opened", "web_url": format!("h{iid}"),
            }))
            .unwrap()
        };
        let open = || vec![issue(3, "7.2"), issue(1, "7.0"), issue(2, "7.1")];
        let pick = |nvrs: &[&str]| {
            pick_issue(open(), Sig::Hyperscale, "ethtool", nvrs)
                .unwrap()
                .iid
        };
        assert_eq!(pick(&["ethtool-7.1-1.hs.el9"]), 2);
        assert_eq!(pick(&["ethtool-7.2-1.hs.el9"]), 3);
        // Reaching none, or a build not made yet: the first.
        assert_eq!(pick(&["ethtool-6.9-1.hs.el9"]), 3);
        assert_eq!(pick(&["<the build in hyperscale9s-packages-main>"]), 3);
        assert!(pick_issue(Vec::new(), Sig::Hyperscale, "ethtool", &[]).is_none());
        assert!(covers(Sig::Hyperscale) && covers(Sig::ProposedUpdates));
    }
}
