// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Telling a build's tracking issue what happened to it in CBS (#144):
//! a comment, the `stage::` label and, from `To do`, the `In progress`
//! status. Planned before the CBS write is confirmed, so the comment is
//! part of what is confirmed, and posted once the write is done. A
//! failure here is a warning: the build has moved either way.
//!
//! For now, Proposed Updates' tracking issues, which cpu-sig-tracker
//! files; see `sandogasa_cbs::tracking` for how one is found.

use std::path::Path;
use std::process::Command;

use sandogasa_cbs::tracking::{
    Provenance, proposed_release, stage_label, stage_note, stage_noted, tracked_mr_url,
    tracks_proposed_branch,
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
}

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
    if sig != Sig::ProposedUpdates || steps.is_empty() {
        return Vec::new();
    }
    let Some(release) = proposed_release(branch) else {
        return Vec::new();
    };
    let path = format!("{}/{package}", sig.gitlab_group());
    let read = Client::new(base, &path, token.unwrap_or("")).and_then(|client| {
        // The release's issues only: GitLab ANDs the labels.
        let issues = client.list_issues(&format!("cpu-sig-tracker,{release}"), Some("opened"))?;
        let Some(issue) = issues.into_iter().find(|i| {
            tracks_proposed_branch(&i.labels, i.description.as_deref().unwrap_or(""), branch)
        }) else {
            return Ok(None);
        };
        let notes = client.issue_notes(issue.iid)?;
        Ok(Some((client, issue, notes)))
    });
    let (client, issue, notes) = match read {
        Ok(Some(found)) => found,
        Ok(None) => {
            println!("{path} has no open tracking issue for {branch}: nothing to tell.");
            return Vec::new();
        }
        Err(e) => {
            eprintln!("warning: cannot read {path}'s tracking issues ({e}); nothing will be told");
            return Vec::new();
        }
    };
    let pending: Vec<&(String, Stage, Vec<Vec<String>>)> = steps
        .iter()
        .filter(|(nvr, stage, _)| {
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
            ),
            label: stage_label(*stage),
            missing_label: None,
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
        format!("Then tell {}:\n", notice.issue_url)
    } else {
        format!(
            "Without a GitLab token (`cbspkg config`, or GITLAB_TOKEN), {} is not told:\n",
            notice.issue_url
        )
    };
    for line in notice.note.lines() {
        out.push_str(&format!("  > {line}\n"));
    }
    if let Some(label) = notice.label {
        out.push_str(&format!("  and label it {label}\n"));
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
/// `done` says, warning about each part that fails.
pub fn post(base: &str, token: Option<&str>, notices: &[Notice], done: &[(String, Stage)]) {
    let notices: Vec<&Notice> = notices
        .iter()
        .filter(|n| {
            done.iter()
                .any(|(nvr, stage)| *nvr == n.nvr && *stage == n.stage)
        })
        .collect();
    if notices.is_empty() {
        return;
    }
    let Some(token) = token else {
        println!(
            "No GitLab token, so the tracking issue is not told: \
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
        match client.add_note(notice.iid, &notice.note) {
            Ok(()) => println!("Told {}.", notice.issue_url),
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
            stage_note(NVR, Stage::Testing, TESTING, None, &Provenance::Unknown)
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
            shown.starts_with("Then tell u1:\n  > `git-2.52.1-1~proposed.el10` is released"),
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
                "body": stage_note(NVR, Stage::Testing, TESTING, None, &Provenance::Unknown),
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
        post(&server.url(), Some("t"), &notices, &[]);
        let done = [(NVR.to_string(), Stage::Testing)];
        post(&server.url(), Some("t"), &notices, &done);
        comment.assert();
        label.assert();
        graphql.assert();

        // Without a token nothing is posted.
        post(&server.url(), None, &notices, &done);
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
}
