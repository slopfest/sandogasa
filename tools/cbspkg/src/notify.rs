// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Telling a build's tracking issue what happened to it in CBS (#144):
//! a comment, the `stage::` label and, from `To do`, the `In progress`
//! status. Planned before the CBS write is confirmed, so the comment is
//! part of what is confirmed, and posted once the write is done. A
//! failure here is a warning: the build has moved either way.
//!
//! The tracking issues are Proposed Updates', which cpu-sig-tracker
//! files per merge request, and Hyperscale's `rfe::new-version` ones,
//! which hs-relmon files per package; see `sandogasa_cbs::tracking` for
//! how one is found.

use std::path::Path;
use std::process::Command;

use std::cmp::Ordering;

use sandogasa_cbs::tracking::{
    Provenance, proposed_release, requested_version, stage_label, stage_note, stage_noted,
    tracked_mr_url, tracks_proposed_branch,
};
use sandogasa_cbs::{Sig, Stage};
use sandogasa_gitlab::{Client, IssueUpdate};

/// One comment to post, and the label to set with it.
#[derive(Debug, PartialEq)]
pub struct Notice {
    /// The build and stage it tells of, to post it only once the build
    /// got there.
    pub nvr: String,
    pub stage: Stage,
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
    /// The version the issue asks for, for a Hyperscale one: a build of
    /// an older version is not what it is about, and gets no comment.
    pub at_least: Option<String>,
    /// For a Hyperscale release: when to close the issue.
    pub closing: Option<Closing>,
}

/// When a Hyperscale issue is done: hs-relmon files one per package,
/// asking for a version, and it is done once every release the SIG
/// carries the package for has that version or a newer one released.
/// The release being promoted counts once it is tagged; a release still
/// behind leaves the issue open for hs-relmon's next scan to close.
#[derive(Debug, PartialEq)]
pub struct Closing {
    pub package: String,
    pub version: String,
    /// The `-release` tag of each release the package has a SIG branch
    /// for.
    pub tags: Vec<String>,
}

/// The newest build of a package in each of some tags, one entry per
/// tag: [`sandogasa_cbs::packages::latest_in_tags`], or a stand-in.
pub type LatestIn<'a> = &'a dyn Fn(&[String], &str) -> Result<Vec<Option<String>>, String>;

/// The SIG branch of a package being promoted, and its checkout.
#[derive(Clone, Copy)]
pub struct Branch<'a> {
    pub sig: Sig,
    pub package: &'a str,
    pub branch: &'a str,
    pub repo: &'a Path,
    /// A dry run: nothing is fetched into the checkout, so what a build
    /// was made from is told only from what it has.
    pub dry_run: bool,
}

/// What to tell the tracking issue of `branch` of `package`, per
/// promotion in `steps` (`(nvr, stage, commands)`), leaving out what it
/// has been told already. Nothing for a SIG this does not cover yet, or
/// without an open issue for the branch, and a warning when GitLab
/// cannot be read.
///
/// Each comment says what the build was made from: `commit_of` gives
/// the commit CBS built (`sandogasa_cbs::packages::build_commit`), and
/// the checkout at `on.repo` compares it with the merge request's head, so
/// SIG-only commits on top are listed.
pub fn plan(
    base: &str,
    token: Option<&str>,
    on: &Branch,
    steps: &[(String, Stage, Vec<Vec<String>>)],
    commit_of: &dyn Fn(&str) -> Result<Option<String>, String>,
) -> Vec<Notice> {
    let Branch {
        sig,
        package,
        branch,
        repo,
        dry_run,
    } = *on;
    if steps.is_empty() {
        return Vec::new();
    }
    // Which issues to read, and which of them is this branch's.
    let labels = match sig {
        // The release's issues only: GitLab ANDs the labels.
        Sig::ProposedUpdates => match proposed_release(branch) {
            Some(release) => format!("cpu-sig-tracker,{release}"),
            None => return Vec::new(),
        },
        Sig::Hyperscale => "rfe::new-version".to_string(),
        // A SIG whose tracking issues this does not know.
        _ => return Vec::new(),
    };
    let tracks = |i: &sandogasa_gitlab::Issue| match sig {
        Sig::ProposedUpdates => {
            tracks_proposed_branch(&i.labels, i.description.as_deref().unwrap_or(""), branch)
        }
        Sig::Hyperscale => requested_version(&i.title, package).is_some(),
        _ => false,
    };
    let path = format!("{}/{package}", sig.gitlab_group());
    let nvrs: Vec<&str> = steps.iter().map(|(nvr, _, _)| nvr.as_str()).collect();
    let read = Client::new(base, &path, token.unwrap_or("")).and_then(|client| {
        let issues: Vec<sandogasa_gitlab::Issue> = client
            .list_issues(&labels, Some("opened"))?
            .into_iter()
            .filter(|i| tracks(i))
            .collect();
        let Some(issue) = pick_issue(issues, sig, package, &nvrs) else {
            return Ok(None);
        };
        let notes = client.issue_notes(issue.iid)?;
        Ok(Some((client, issue, notes)))
    });
    let (client, issue, notes) = match read {
        Ok(Some(found)) => found,
        Ok(None) => {
            println!("{path} has no open tracking issue for {branch}: no comment to post.");
            return Vec::new();
        }
        Err(e) => {
            eprintln!(
                "warning: cannot read {path}'s tracking issues ({e}); no comment will be posted"
            );
            return Vec::new();
        }
    };
    let at_least = match sig {
        Sig::Hyperscale => requested_version(&issue.title, package).map(str::to_string),
        _ => None,
    };
    let pending: Vec<&(String, Stage, Vec<Vec<String>>)> = steps
        .iter()
        .filter(|(nvr, stage, _)| {
            // A build's NVR is a stand-in until it is built.
            if let Some(v) = &at_least
                && !nvr.starts_with('<')
                && !reaches(nvr, v)
            {
                println!(
                    "{nvr} is older than the {v} {} asks for: no comment to post.",
                    issue.web_url
                );
                return false;
            }
            let told = stage_noted(notes.iter().map(|n| n.body.as_str()), nvr, *stage);
            if told {
                println!(
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
    let commits: Vec<Option<String>> = pending
        .iter()
        .map(|(nvr, _, _)| match commit_of(nvr) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("warning: cannot tell what {nvr} was built from ({e})");
                None
            }
        })
        .collect();
    if let Some(mr) = &mr
        && !dry_run
    {
        fetch_missing(repo, mr, &commits);
    }
    let earlier: Vec<&str> = notes.iter().map(|n| n.body.as_str()).collect();
    // The releases a Hyperscale release closes the issue for, read once.
    let release_tags = if at_least.is_some() && pending.iter().any(|(_, s, _)| *s == Stage::Release)
    {
        hyperscale_release_tags(repo)
    } else {
        Vec::new()
    };
    let mut notices = Vec::new();
    for ((nvr, stage, argvs), commit) in pending.into_iter().zip(&commits) {
        let (tag, untagged) = tags_of_step(argvs);
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
            stage: *stage,
            path: path.clone(),
            iid: issue.iid,
            issue_url: issue.web_url.clone(),
            note: stage_note(
                nvr,
                *stage,
                &tag,
                untagged,
                &match (&mr, commit) {
                    (Some(mr), Some(commit)) => provenance(repo, &mr.head, commit),
                    _ => Provenance::Unknown,
                },
                &earlier,
            ),
            label: stage_label(*stage),
            missing_label: None,
            closing: match (&at_least, stage) {
                (Some(version), Stage::Release) if !release_tags.is_empty() => Some(Closing {
                    package: package.to_string(),
                    version: version.clone(),
                    tags: release_tags.clone(),
                }),
                _ => None,
            },
            at_least: at_least.clone(),
        });
    }
    // Which stage labels exist, read only when one is wanted.
    if notices.iter().any(|n| n.label.is_some()) {
        let labels = client.label_names().unwrap_or_else(|e| {
            eprintln!("warning: cannot read {path}'s labels ({e}); no stage label is set");
            Vec::new()
        });
        for n in &mut notices {
            if let Some(wanted) = n.label
                && !labels.iter().any(|l| l == wanted)
            {
                n.label = None;
                n.missing_label = Some(wanted);
            }
        }
    }
    notices
}

/// How the plan shows a notice: the issue, the comment indented, and
/// the label; without a token to post with, that it will not be told.
pub fn describe(notice: &Notice, postable: bool) -> String {
    let mut out = if postable {
        format!("Then comment on {}:\n", notice.issue_url)
    } else {
        format!(
            "Without a GitLab token (`cbspkg config`, or GITLAB_TOKEN), no comment is posted on {}:\n",
            notice.issue_url
        )
    };
    for line in notice.note.lines() {
        out.push_str(&format!("  > {line}\n"));
    }
    if let Some(label) = notice.label {
        out.push_str(&format!("  and label it {label}\n"));
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
            "  (no {label} label exists in the project or the SIG's group: \
             create one in the group for the issue to get it)\n"
        ));
    }
    out
}

/// Post the notices of `notices` whose build reached its stage, as
/// `done` says, warning about each part that fails. A Hyperscale
/// release closes its issue when every release has the version
/// (`latest` reads what they have).
pub fn post(
    base: &str,
    token: Option<&str>,
    notices: &[Notice],
    done: &[(String, Stage)],
    latest: LatestIn,
) {
    let notices: Vec<&Notice> = notices
        .iter()
        .filter(|n| {
            done.iter()
                .any(|(nvr, stage)| *nvr == n.nvr && *stage == n.stage)
        })
        .filter(|n| match &n.at_least {
            Some(v) if !reaches(&n.nvr, v) => {
                println!(
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
        println!(
            "No GitLab token, so the tracking issue is not updated: \
             set one with `cbspkg config` or GITLAB_TOKEN."
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
            Ok(()) => println!("Commented on {}.", notice.issue_url),
            Err(e) => {
                eprintln!("warning: could not comment on {} ({e})", notice.issue_url);
                continue;
            }
        }
        if let Some(label) = notice.label {
            let update = IssueUpdate {
                add_labels: Some(label.to_string()),
                ..Default::default()
            };
            if let Err(e) = client.edit_issue(notice.iid, &update) {
                eprintln!(
                    "warning: could not label {} {label} ({e})",
                    notice.issue_url
                );
            }
        }
        if done_note.is_some() {
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
    sandogasa_cbs::packages::latest_in_tags(tags, package)
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
struct MergeRequest {
    repo_url: String,
    mr_ref: String,
    head: String,
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

/// Fetch, once each, what the checkout at `repo` lacks to compare
/// `commits` with the merge request: its head from CentOS Stream, the
/// builds' commits from the checkout's remotes.
fn fetch_missing(repo: &Path, mr: &MergeRequest, commits: &[Option<String>]) {
    if !has_commit(repo, &mr.head) {
        let _ = git(repo, &["fetch", "--quiet", &mr.repo_url, &mr.mr_ref]);
    }
    if commits.iter().flatten().any(|c| !has_commit(repo, c)) {
        let _ = git(repo, &["fetch", "--quiet", "--all"]);
    }
}

/// Whether the checkout at `repo` has `commit`.
fn has_commit(repo: &Path, commit: &str) -> bool {
    git(repo, &["cat-file", "-e", &format!("{commit}^{{commit}}")]).is_ok()
}

/// What `commit` is, against the merge request's `head`, in the checkout
/// at `repo`, from what it has: `Unknown`, with a warning, when it lacks
/// either, or when what it carries cannot be listed.
pub fn provenance(repo: &Path, head: &str, commit: &str) -> Provenance {
    if !(has_commit(repo, head) && has_commit(repo, commit)) {
        eprintln!(
            "warning: the checkout lacks {} or {}; the comment cannot say what the build carries",
            short(head),
            short(commit)
        );
        return Provenance::Unknown;
    }
    let id = git(repo, &["rev-parse", "--short", commit]).unwrap_or_else(|_| short(commit));
    let full = |c: &str| git(repo, &["rev-parse", c]).ok();
    if full(head) == full(commit) {
        return Provenance::Head(id);
    }
    if git(repo, &["merge-base", "--is-ancestor", head, commit]).is_err() {
        return Provenance::Apart(id);
    }
    match git(
        repo,
        &["log", "--format=%h %s", &format!("{head}..{commit}")],
    ) {
        Ok(carried) if !carried.is_empty() => {
            Provenance::Carried(id, carried.lines().map(str::to_string).collect())
        }
        _ => Provenance::Unknown,
    }
}

/// The first seven characters of a commit id.
fn short(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// Run `git` in `repo` to read something, its output trimmed.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
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
                println!(
                    "{issue_url} stays open: {tag} has {nvr}, older than {}.",
                    closing.version
                );
                return None;
            }
            None => {
                println!(
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
    println!("Closed {}.", notice.issue_url);
    if let Err(e) = client.set_work_item_status(notice.iid, "Done") {
        eprintln!("warning: could not set {} to Done ({e})", notice.issue_url);
    }
    true
}

/// The `-release` tags of the Hyperscale releases the checkout at `repo`
/// has a SIG branch for (`c10s-sig-hyperscale`, `c9s-hs`), in its own
/// branches or its remotes', newest release first; only releases the
/// SIG still releases for, so an EOL branch does not keep issues open.
fn hyperscale_release_tags(repo: &Path) -> Vec<String> {
    let refs = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:lstrip=2)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .unwrap_or_default();
    let mut majors: Vec<u32> = refs
        .lines()
        // `origin/c10s-sig-hyperscale` is the branch after the remote.
        .map(|r| r.rsplit('/').next().unwrap_or(r))
        .filter_map(|b| Sig::Hyperscale.own_branch_major(b))
        .filter(|m| sandogasa_cbs::HYPERSCALE_MAJORS.contains(m))
        .collect();
    majors.sort_unstable_by(|a, b| b.cmp(a));
    majors.dedup();
    majors
        .into_iter()
        .map(|m| sandogasa_cbs::Collection::new("hyperscale", m, true, "main").tag(Stage::Release))
        .collect()
}

/// The tag a promotion's commands tag the build into, and the one they
/// untag it from, if any: `cbs tag-build <tag> <nvr>`,
/// `cbs untag-build <tag> <nvr>`.
fn tags_of_step(argvs: &[Vec<String>]) -> (Option<&str>, Option<&str>) {
    let tag = |verb: &str| {
        argvs
            .iter()
            .find(|a| a.get(1).map(String::as_str) == Some(verb))
            .and_then(|a| a.get(2))
            .map(String::as_str)
    };
    (tag("tag-build"), tag("untag-build"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// git's branch `branch` in `sig`, from a checkout that is not there.
    fn on<'a>(sig: Sig, branch: &'a str) -> Branch<'a> {
        Branch {
            sig,
            package: "git",
            branch,
            repo: Path::new("."),
            dry_run: true,
        }
    }

    /// A build whose source names no commit.
    fn none(_: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn no_latest(_: &[String], _: &str) -> Result<Vec<Option<String>>, String> {
        panic!("only a Hyperscale release reads the release tags")
    }

    const PATH: &str = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fgit";
    const NVR: &str = "git-2.52.1-1~proposed.el10";
    const TESTING: &str = "proposed_updates10s-packages-main-testing";
    const RELEASE: &str = "proposed_updates10s-packages-main-release";

    fn step(stage: Stage) -> (String, Stage, Vec<Vec<String>>) {
        let argv = |verb: &str, tag: &str| ["cbs", verb, tag, NVR].map(String::from).to_vec();
        let argvs = match stage {
            Stage::Release => vec![argv("tag-build", RELEASE), argv("untag-build", TESTING)],
            _ => vec![argv("tag-build", TESTING)],
        };
        (NVR.to_string(), stage, argvs)
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
            vec![
                ["cbs", "untag-build", TESTING, NVR]
                    .map(String::from)
                    .to_vec(),
            ],
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
    fn a_notice_is_posted_labelled_and_moves_the_issue_into_progress() {
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
        let done = [(NVR.to_string(), Stage::Testing)];
        post(&server.url(), Some("t"), &notices, &done, &no_latest);
        comment.assert();
        label.assert();
        graphql.assert();

        // Without a token nothing is posted.
        post(&server.url(), None, &notices, &done, &no_latest);
        comment.assert();
    }

    #[test]
    fn a_builds_commit_is_the_head_carries_sig_only_commits_or_is_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| crate::test_support::git(repo, args);
        git(&["init", "--quiet"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Update git",
        ]);
        let head = git(&["rev-parse", "HEAD"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Pin Ruby 3 for CBS's el9 buildroot",
        ]);
        let pinned = git(&["rev-parse", "HEAD"]);
        let short_head = git(&["rev-parse", "--short", &head]);
        let short_pinned = git(&["rev-parse", "--short", &pinned]);
        assert_eq!(
            provenance(repo, &head, &head),
            Provenance::Head(short_head.clone())
        );
        assert_eq!(
            provenance(repo, &head, &pinned),
            Provenance::Carried(
                short_pinned.clone(),
                vec![format!("{short_pinned} Pin Ruby 3 for CBS's el9 buildroot")]
            )
        );
        assert_eq!(
            provenance(repo, &pinned, &head),
            Provenance::Apart(short_head)
        );
        assert_eq!(
            provenance(repo, &head, "0123456789abcdef0123456789abcdef01234567"),
            Provenance::Unknown
        );
    }

    /// git's c9s build with the Ruby pin on top of merge request !33,
    /// whose head the checkout has never fetched: it is fetched from
    /// CentOS Stream's merge request ref, and the comment lists the pin.
    #[test]
    fn a_comment_lists_the_sig_only_commits_the_build_carries() {
        let tmp = tempfile::tempdir().unwrap();
        let git = |dir: &Path, args: &[&str]| crate::test_support::git(dir, args);
        let commit = |dir: &Path, m: &str| {
            git(
                dir,
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "--quiet",
                    "--allow-empty",
                    "-m",
                    m,
                ],
            );
            git(dir, &["rev-parse", "HEAD"])
        };
        // CentOS Stream's repo, with the merge request's head.
        let mut server = mockito::Server::new();
        let stream_root = tmp.path().join("stream");
        let stream = stream_root.join("redhat/centos-stream/rpms/git.git");
        std::fs::create_dir_all(&stream).unwrap();
        git(&stream, &["init", "--quiet", "--bare"]);
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git(&work, &["init", "--quiet"]);
        let head = commit(&work, "Update git to 2.52.1");
        git(
            &work,
            &[
                "push",
                "--quiet",
                &stream.display().to_string(),
                "HEAD:refs/merge-requests/33/head",
            ],
        );
        // The checkout: the pin on top, without the head's ref.
        let pinned = commit(&work, "Pin Ruby 3 for CBS's el9 buildroot");
        let checkout = tmp.path().join("checkout");
        git(tmp.path(), &["init", "--quiet", "checkout"]);
        git(
            &checkout,
            &[
                "config",
                &format!("url.{}/.insteadOf", stream_root.display()),
                &format!("{}/", server.url()),
            ],
        );
        git(
            &checkout,
            &[
                "fetch",
                "--quiet",
                &work.display().to_string(),
                &format!("{pinned}:refs/heads/c9s-proposed-RHEL-306262"),
            ],
        );
        let url = server.url();
        let mr_url = format!("{url}/redhat/centos-stream/rpms/git/-/merge_requests/33");
        let serve_mr = |server: &mut mockito::Server, sha: &str| {
            server
                .mock(
                    "GET",
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fgit/merge_requests/33",
                )
                .with_status(200)
                .with_body(
                    serde_json::json!({
                        "iid": 33, "title": "Update git to 2.52.1", "state": "opened",
                        "web_url": mr_url, "source_branch": "a", "target_branch": "c9s",
                        "sha": sha,
                    })
                    .to_string(),
                )
                .create()
        };
        let mr = serve_mr(&mut server, &head);
        let path = "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fgit";
        server
            .mock("GET", format!("{path}/issues").as_str())
            .match_query(mockito::Matcher::Any)
            .with_status(200)
            .with_body(
                serde_json::json!([{
                    "iid": 2, "title": "git c9s", "state": "opened", "web_url": "u2",
                    "labels": ["c9s", "cpu-sig-tracker"],
                    "description": format!("- **MR**: [Update git to 2.52.1]({mr_url}) — opened\n- **JIRA**: [RHEL-306262](j) — New"),
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
            .with_body(r#"[{"name": "stage::testing"}]"#)
            .create();
        let nvr = "git-2.52.1-1~proposed.el9";
        let steps = [(
            nvr.to_string(),
            Stage::Testing,
            vec![
                [
                    "cbs",
                    "tag-build",
                    "proposed_updates9s-packages-main-testing",
                    nvr,
                ]
                .map(String::from)
                .to_vec(),
            ],
        )];
        let on = Branch {
            sig: Sig::ProposedUpdates,
            package: "git",
            branch: "c9s-proposed-RHEL-306262",
            repo: &checkout,
            dry_run: false,
        };
        let built = |_: &str| Ok(Some(pinned.clone()));
        let notices = plan(&url, Some("t"), &on, &steps, &built);
        let note = &notices[0].note;
        assert!(
            note.contains("the merge request's head and these SIG-only commits"),
            "{note}"
        );
        assert!(
            note.contains("Pin Ruby 3 for CBS's el9 buildroot"),
            "{note}"
        );

        // The merge request moves on to a head only CentOS Stream has: it
        // is fetched from the merge request's ref, and the build, made
        // before, does not contain it.
        git(&work, &["checkout", "--quiet", "--detach", &head]);
        let moved = commit(&work, "Update git to 2.52.1, take 2");
        git(
            &work,
            &[
                "push",
                "--quiet",
                "--force",
                &stream.display().to_string(),
                "HEAD:refs/merge-requests/33/head",
            ],
        );
        assert!(!has_commit(&checkout, &moved));
        mr.remove();
        serve_mr(&mut server, &moved);
        let notices = plan(&url, Some("t"), &on, &steps, &built);
        assert!(has_commit(&checkout, &moved));
        let note = &notices[0].note;
        assert!(
            note.contains("does not contain the merge request's current head"),
            "{note}"
        );
        // A dry run fetches nothing: without the head, nothing is said.
        let fresh = tmp.path().join("fresh");
        git(tmp.path(), &["init", "--quiet", "fresh"]);
        let dry = Branch {
            repo: &fresh,
            dry_run: true,
            ..on
        };
        let notices = plan(&url, Some("t"), &dry, &steps, &built);
        assert!(!notices[0].note.contains("Built from"));
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
                vec![["cbs", "tag-build", tag, nvr].map(String::from).to_vec()],
            )
        };
        let on = Branch {
            sig: Sig::Hyperscale,
            package: "ethtool",
            branch: "c9s-hs",
            repo: Path::new("."),
            dry_run: true,
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
        let done = [(older.nvr.clone(), Stage::Testing)];
        post(&server.url(), Some("t"), &[older], &done, &no_latest);
        posts.assert();
    }

    /// ethtool 7.1 released to Hyperscale 10, with the checkout having
    /// branches for 9 and 10: the issue closes only once 9 has it too.
    #[test]
    fn a_hyperscale_release_closes_the_issue_once_every_release_has_the_version() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let git = |args: &[&str]| crate::test_support::git(repo, args);
        git(&["init", "--quiet", "-b", "c10s-sig-hyperscale"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "Update to 7.1",
        ]);
        // A remote's branch counts, a mirrored Stream one does not.
        git(&["update-ref", "refs/remotes/origin/c9s-hs", "HEAD"]);
        git(&["branch", "c10s"]);
        git(&["branch", "c8s-sig-hyperscale"]);
        let r10 = "hyperscale10s-packages-main-release";
        let r9 = "hyperscale9s-packages-main-release";
        assert_eq!(hyperscale_release_tags(repo), [r10, r9]);
        assert!(hyperscale_release_tags(Path::new("/nonexistent")).is_empty());

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
                ["cbs", "tag-build", r10, nvr].map(String::from).to_vec(),
                [
                    "cbs",
                    "untag-build",
                    "hyperscale10s-packages-main-testing",
                    nvr,
                ]
                .map(String::from)
                .to_vec(),
            ],
        )];
        let on = Branch {
            sig: Sig::Hyperscale,
            package: "ethtool",
            branch: "c10s-sig-hyperscale",
            repo,
            dry_run: true,
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
        let done = [(nvr.to_string(), Stage::Release)];

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
