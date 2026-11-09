// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether a Proposed Update's change has reached stock: the one answer
//! `untag` and `retire` both act on.
//!
//! `untag` removes the SIG's build because stock carries the change;
//! `retire` closes the tracking issue as landed for the same reason. If
//! each worked it out for itself, one could act on evidence the other
//! refuses: `untag` judged by Jira alone while `retire` also read stock's
//! history, so PackageKit's c10s update, fixed in stock by Red Hat's own
//! commit but with its RHEL issue closed `Not a Bug`, could be retired
//! but not untagged. So both call [`landed`], and a test holds their
//! checks to it.

use crate::gitlab;
use crate::jira::JiraCheck;
use crate::ping::{evidence_tokens, find_evidence};
use crate::status::{parse_mr_line, scan_mr_url_in_body};

/// What is known about whether the change reached stock, and the MR it
/// was proposed in.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Landed {
    /// The upstream MR the tracking issue names, and its state as GitLab
    /// reports it (`None` when it could not be read).
    pub mr_url: Option<String>,
    pub mr_state: Option<String>,
    /// What says the change is in stock, when something does.
    pub by: Option<String>,
}

/// What says a change is in stock, given what was read, in order of
/// strength: the SIG's MR merged; a commit on stock's branch naming the
/// change (`stock`, from [`find_evidence`]); every RHEL issue fixed,
/// with a fix version or as `Done-Errata` (#87). `None` when nothing
/// does.
pub fn landed_by(
    mr_state: Option<&str>,
    stock: Option<String>,
    jira: &JiraCheck,
) -> Option<String> {
    if mr_state == Some("merged") {
        return Some("MR merged".to_string());
    }
    stock.or_else(|| {
        jira.fixed
            .then(|| format!("RHEL issue fixed: {}", jira.check.detail()))
    })
}

/// Whether the change of the tracking `issue` reached stock's `release`
/// branch of `package`: the MR it names is read, and, unless it merged,
/// stock's history searched for a commit naming the change (its RHEL
/// key, a CVE, the MR's title or branch), then `jira` consulted. A
/// lookup that fails counts as no evidence.
pub fn landed(
    issue: &gitlab::Issue,
    package: &str,
    release: &str,
    jira: &JiraCheck,
    verbose: bool,
) -> Landed {
    let base = crate::utils::gitlab_base();
    let body = issue.description.as_deref().unwrap_or("");
    let mr_url = parse_mr_line(body)
        .map(|(u, _)| u)
        .or_else(|| scan_mr_url_in_body(body));
    let mut mr_state = None;
    let mut mr_title = String::new();
    let mut mr_branch = String::new();
    if let Some(url) = &mr_url {
        let fetched: Result<sandogasa_gitlab::MergeRequest, Box<dyn std::error::Error>> =
            match gitlab::parse_mr_url(url) {
                Ok((_, project, iid)) => {
                    gitlab::client(&base, &project).and_then(|c| c.merge_request(iid))
                }
                Err(e) => Err(e.into()),
            };
        match fetched {
            Ok(mr) => {
                mr_state = Some(mr.state.clone());
                mr_title = mr.title.clone();
                mr_branch = mr.source_branch.clone();
            }
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read {url}: {e}");
                }
            }
        }
    }
    let stock = if mr_state.as_deref() == Some("merged") {
        None
    } else {
        let tokens = evidence_tokens(body, &issue.title, &mr_title, &mr_branch);
        match gitlab::client(&base, &format!("redhat/centos-stream/rpms/{package}"))
            .and_then(|c| c.branch_commits(release))
        {
            Ok(commits) => find_evidence(&commits, &tokens),
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read stock history: {e}");
                }
                None
            }
        }
    };
    Landed {
        by: landed_by(mr_state.as_deref(), stock, jira),
        mr_url,
        mr_state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::Check;

    fn jira(fixed: bool) -> JiraCheck {
        JiraCheck {
            check: if fixed {
                Check::Pass("RHEL-1 — Done, fixed in rhel-10.3".into())
            } else {
                Check::Fail("RHEL-1 was closed Not a Bug: not fixed in RHEL".into())
            },
            resolution_name: None,
            resolution_date: None,
            fixed,
            declined: !fixed,
            unanswered: false,
        }
    }

    #[test]
    fn the_strongest_evidence_names_why_it_landed() {
        let stock = || Some("stock commit 99e0f170 names CVE-2026-41651".to_string());
        assert_eq!(
            landed_by(Some("merged"), stock(), &jira(true)).as_deref(),
            Some("MR merged")
        );
        // PackageKit c10s: MR closed, RHEL issue Not a Bug, but stock
        // carries Red Hat's own fix.
        assert_eq!(landed_by(Some("closed"), stock(), &jira(false)), stock());
        assert_eq!(
            landed_by(Some("opened"), None, &jira(true)).as_deref(),
            Some("RHEL issue fixed: RHEL-1 — Done, fixed in rhel-10.3")
        );
        // blktrace: nothing on record.
        assert_eq!(landed_by(Some("opened"), None, &jira(false)), None);
    }

    /// untag and retire act on the same answer: for every kind of
    /// evidence, or none, untag's precondition passes exactly when
    /// retire's check for "landed" does. A change to either side's
    /// judgement fails here until the other follows.
    #[test]
    fn untag_and_retire_agree_on_what_landed() {
        let cases = [
            landed_by(Some("merged"), None, &jira(false)),
            landed_by(
                Some("closed"),
                Some("stock commit names it".into()),
                &jira(false),
            ),
            landed_by(Some("opened"), None, &jira(true)),
            landed_by(Some("opened"), None, &jira(false)),
            landed_by(None, None, &jira(false)),
        ];
        for by in cases {
            let landed = Landed {
                by: by.clone(),
                ..Default::default()
            };
            let facts = crate::retire::RetireFacts {
                landed_by: by.clone(),
                ..Default::default()
            };
            let untag = matches!(crate::untag::precondition(&landed), Check::Pass(_));
            let retire = matches!(
                crate::retire::reason_check(crate::retire::Reason::Landed, &facts),
                Check::Pass(_)
            );
            assert_eq!(untag, retire, "untag and retire disagree on {by:?}");
            assert_eq!(untag, by.is_some());
        }
    }
}
