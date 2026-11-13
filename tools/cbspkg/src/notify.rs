// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Telling a build's tracking issue what happened to it in CBS (#144):
//! a comment, the `stage::` label and, from `To do`, the `In progress`
//! status. Planned before the CBS write is confirmed, so the comment is
//! part of what is confirmed, and posted once the write is done. A
//! failure here is a warning: the build has moved either way.
//!
//! For now, Proposed Updates' tracking issues, which cpu-sig-tracker
//! files; see `sandogasa_cbs::tracking` for how one is found.

use sandogasa_cbs::tracking::{
    proposed_release, stage_label, stage_note, stage_noted, tracks_proposed_branch,
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

/// What to tell the tracking issue of `branch` of `package`, per
/// promotion in `steps` (`(nvr, stage, commands)`), leaving out what it
/// has been told already. Nothing for a SIG this does not cover yet, or
/// without an open issue for the branch, and a warning when GitLab
/// cannot be read.
pub fn plan(
    base: &str,
    token: Option<&str>,
    sig: Sig,
    package: &str,
    branch: &str,
    steps: &[(String, Stage, Vec<Vec<String>>)],
) -> Vec<Notice> {
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
    let mut notices = Vec::new();
    for (nvr, stage, argvs) in steps {
        if stage_noted(notes.iter().map(|n| n.body.as_str()), nvr, *stage) {
            println!(
                "{} says {nvr} reached -{} already.",
                issue.web_url,
                stage.as_str()
            );
            continue;
        }
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
            note: stage_note(nvr, *stage, &tag, untagged),
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
            stage_note(NVR, Stage::Testing, TESTING, None)
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
            Sig::ProposedUpdates,
            "git",
            branch,
            &steps,
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
            Sig::ProposedUpdates,
            "git",
            branch,
            &[untag_only],
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
                Sig::ProposedUpdates,
                "git",
                "c10s-proposed-RHEL-1",
                &steps
            )
            .is_empty()
        );
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                Sig::Hyperscale,
                "git",
                branch,
                &steps
            )
            .is_empty()
        );
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                Sig::ProposedUpdates,
                "git",
                branch,
                &[]
            )
            .is_empty()
        );
        // GitLab unreadable: a warning, nothing to tell.
        assert!(
            plan(
                "http://127.0.0.1:9",
                None,
                Sig::ProposedUpdates,
                "git",
                branch,
                &steps
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
            Sig::ProposedUpdates,
            "git",
            branch,
            &steps,
        );
        assert_eq!(notices[0].label, Some("stage::testing"));
        assert!(describe(&notices[0], true).contains("and label it stage::testing"));

        let comment = server
            .mock("POST", format!("{PATH}/issues/1/notes").as_str())
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "body": stage_note(NVR, Stage::Testing, TESTING, None),
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
}
