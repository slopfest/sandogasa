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
use crate::patches::{Comparison, PatchFile};
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
    /// Why it does not count as landed although something might have
    /// suggested it: stock lacks a patch the update adds, or has them
    /// but not yet in a build past the SIG's.
    pub doubt: Option<String>,
    /// The update's patches against stock's, when it adds any.
    pub patches: Option<Comparison>,
}

/// The facts [`landed_by`] decides on.
#[derive(Debug, Default, Clone)]
pub struct Evidence<'a> {
    pub mr_state: Option<&'a str>,
    /// The update's patches against stock's; `None` when it adds none
    /// (a spec-only change) or they could not be read.
    pub patches: Option<&'a Comparison>,
    /// The SIG's build and stock's, as NVRs.
    pub sig_nvr: Option<&'a str>,
    pub stock_nvr: Option<&'a str>,
    /// A commit on stock's branch naming the change, described.
    pub stock_commit: Option<String>,
    /// Every RHEL issue fixed (#87), described; `None` otherwise.
    pub jira_fixed: Option<&'a str>,
}

/// What says a change is in stock, and what keeps it from counting,
/// in order of strength:
///
/// 1. the SIG's MR merged;
/// 2. the patches the update adds: all carried by stock, in a build at
///    or past the SIG's, is landed; any missing is *not*, whatever
///    Jira or stock's commit messages say (blktrace's Jira read Done
///    with nothing in stock);
/// 3. only when the update adds no patches to compare: a stock commit
///    naming the change, then every RHEL issue fixed.
pub fn landed_by(e: &Evidence) -> (Option<String>, Option<String>) {
    if e.mr_state == Some("merged") {
        return (Some("MR merged".to_string()), None);
    }
    match e.patches {
        Some(c) if c.all_carried() => {
            let carried: Vec<String> = c
                .matched
                .iter()
                .map(|(ours, theirs, how)| format!("{ours} as {theirs} ({how})"))
                .collect();
            if stock_at_or_past(e.stock_nvr, e.sig_nvr) {
                (
                    Some(format!(
                        "stock {} carries every patch the update adds: {}",
                        e.stock_nvr.unwrap_or("?"),
                        carried.join("; ")
                    )),
                    None,
                )
            } else {
                (
                    None,
                    Some(format!(
                        "stock carries every patch the update adds, but its build ({}) is not \
                         yet past the SIG's ({}): wait for it",
                        e.stock_nvr.unwrap_or("unknown"),
                        e.sig_nvr.unwrap_or("unknown")
                    )),
                )
            }
        }
        Some(c) => {
            let also = match (&e.stock_commit, e.jira_fixed) {
                (_, Some(j)) => format!(", though {j}"),
                (Some(s), None) => format!(", though {s}"),
                (None, None) => String::new(),
            };
            (
                None,
                Some(format!(
                    "stock does not carry {} of the patches the update adds{also}",
                    c.missing.join(", ")
                )),
            )
        }
        None => (
            e.stock_commit
                .clone()
                .or_else(|| e.jira_fixed.map(|j| format!("RHEL issue fixed: {j}"))),
            None,
        ),
    }
}

/// Whether stock's build is at or past the SIG's, the SIG's `~proposed`
/// sorting below the same version-release without it. A SIG build no
/// longer tagged leaves stock's alone to go by.
fn stock_at_or_past(stock: Option<&str>, sig: Option<&str>) -> bool {
    use std::cmp::Ordering;
    let vr = |nvr: &str| crate::status::vr_of_nvr(nvr);
    match (stock.and_then(vr), sig.map(vr)) {
        (Some(_), None) => true,
        (Some(s), Some(Some(g))) => sandogasa_rpmvercmp::compare_evr(&s, &g) != Ordering::Less,
        _ => false,
    }
}

/// Whether the change of the tracking `issue` reached stock's `release`
/// branch of `package`, the SIG's build being `sig_nvr` and stock's
/// `stock_nvr`: the MR it names is read, its added patches compared
/// with stock's, and stock's history searched for a commit naming the
/// change; then `jira_fixed`. A lookup that fails counts as no
/// evidence, and stops a comparison rather than deciding it.
pub fn landed(
    issue: &gitlab::Issue,
    package: &str,
    release: &str,
    sig_nvr: Option<&str>,
    stock_nvr: Option<&str>,
    jira_fixed: Option<&str>,
    verbose: bool,
) -> Landed {
    let base = crate::utils::gitlab_base();
    let body = issue.description.as_deref().unwrap_or("");
    let mr_url = parse_mr_line(body)
        .map(|(u, _)| u)
        .or_else(|| scan_mr_url_in_body(body));
    let stock_project = format!("redhat/centos-stream/rpms/{package}");
    let mut mr_state = None;
    let mut mr_title = String::new();
    let mut mr_branch = String::new();
    let mut patches = None;
    if let Some(url) = &mr_url {
        match gitlab::parse_mr_url(url) {
            Ok((_, project, iid)) => match gitlab::reader(&base, &project) {
                Ok(client) => {
                    match client.merge_request(iid) {
                        Ok(mr) => {
                            mr_state = Some(mr.state.clone());
                            mr_title = mr.title.clone();
                            mr_branch = mr.source_branch.clone();
                        }
                        Err(e) if verbose => eprintln!("[cpu-sig-tracker] cannot read {url}: {e}"),
                        Err(_) => {}
                    }
                    patches =
                        compare_patches(&client, iid, &base, &stock_project, release, verbose);
                }
                Err(e) if verbose => eprintln!("[cpu-sig-tracker] cannot read {url}: {e}"),
                Err(_) => {}
            },
            Err(e) if verbose => eprintln!("[cpu-sig-tracker] {url}: {e}"),
            Err(_) => {}
        }
    }
    let stock_commit = if mr_state.as_deref() == Some("merged") {
        None
    } else {
        let tokens = evidence_tokens(body, &issue.title, &mr_title, &mr_branch);
        match gitlab::reader(&base, &stock_project).and_then(|c| c.branch_commits(release)) {
            Ok(commits) => find_evidence(&commits, &tokens),
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read stock history: {e}");
                }
                None
            }
        }
    };
    let (by, doubt) = landed_by(&Evidence {
        mr_state: mr_state.as_deref(),
        patches: patches.as_ref(),
        sig_nvr,
        stock_nvr,
        stock_commit,
        jira_fixed,
    });
    Landed {
        mr_url,
        mr_state,
        by,
        doubt,
        patches,
    }
}

/// The patches merge request `iid` adds against those on stock's
/// `release` branch; `None` when it adds none, or either side cannot be
/// read.
fn compare_patches(
    mr_project: &gitlab::Client,
    iid: u64,
    base: &str,
    stock_project: &str,
    release: &str,
    verbose: bool,
) -> Option<Comparison> {
    let diffs = mr_project
        .merge_request_diffs(iid)
        .map_err(|e| {
            if verbose {
                eprintln!("[cpu-sig-tracker] cannot read the MR's changes: {e}");
            }
        })
        .ok()?;
    let ours = crate::patches::added_patches(&diffs);
    let our_spec = crate::patches::added_spec_shas(&diffs);
    if ours.is_empty() && our_spec.is_empty() {
        return None;
    }
    let stock = gitlab::reader(base, stock_project).ok()?;
    let tree = stock.tree(release).ok()?;
    let mut stock_patches = Vec::new();
    let mut stock_spec = Vec::new();
    for entry in tree.iter().filter(|e| e.kind == "blob") {
        if entry.name.ends_with(".patch") {
            let text = stock.raw_file(&entry.path, release).ok()??;
            stock_patches.push(PatchFile {
                name: entry.name.clone(),
                text,
            });
        } else if entry.name.ends_with(".spec") {
            let text = stock.raw_file(&entry.path, release).ok()??;
            stock_spec.extend(crate::patches::spec_patch_shas(&text));
        }
    }
    crate::patches::compare(&ours, &our_spec, &stock_patches, &stock_spec)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::Check;

    fn carried() -> Comparison {
        Comparison {
            matched: vec![(
                "PK_x.patch".into(),
                "0001-x.patch".into(),
                "upstream ac49526d".into(),
            )],
            missing: vec![],
        }
    }

    fn missing() -> Comparison {
        Comparison {
            matched: vec![],
            missing: vec!["fix.patch".into()],
        }
    }

    #[test]
    fn the_strongest_evidence_names_why_it_landed() {
        let stock_commit = || Some("stock commit 99e0f170 names CVE-2026-41651".to_string());
        let fixed = Some("RHEL-1 — Done, fixed in rhel-10.3");
        assert_eq!(
            landed_by(&Evidence {
                mr_state: Some("merged"),
                ..Default::default()
            })
            .0
            .as_deref(),
            Some("MR merged")
        );
        // PackageKit c10s: MR closed, RHEL issue Not a Bug, but stock's
        // build carries the patch under Red Hat's own name.
        let c = carried();
        let (by, doubt) = landed_by(&Evidence {
            mr_state: Some("closed"),
            patches: Some(&c),
            sig_nvr: Some("PackageKit-1.2.8-9~proposed.el10"),
            stock_nvr: Some("PackageKit-1.2.8-9.el10"),
            ..Default::default()
        });
        assert!(
            by.unwrap()
                .contains("PK_x.patch as 0001-x.patch (upstream ac49526d)")
        );
        assert_eq!(doubt, None);
        // Carried, but stock not yet built past the SIG: wait.
        let (by, doubt) = landed_by(&Evidence {
            patches: Some(&c),
            sig_nvr: Some("x-1.2-3~proposed.el10"),
            stock_nvr: Some("x-1.2-2.el10"),
            ..Default::default()
        });
        assert!(by.is_none() && doubt.unwrap().contains("not yet past"));
        // A patch missing in stock outweighs Jira and commit messages.
        let m = missing();
        let (by, doubt) = landed_by(&Evidence {
            patches: Some(&m),
            stock_commit: stock_commit(),
            jira_fixed: fixed,
            ..Default::default()
        });
        assert!(by.is_none());
        assert!(doubt.unwrap().contains("does not carry fix.patch"));
        // Nothing to compare (a spec-only change): the weaker evidence.
        assert_eq!(
            landed_by(&Evidence {
                stock_commit: stock_commit(),
                ..Default::default()
            })
            .0,
            stock_commit()
        );
        assert_eq!(
            landed_by(&Evidence {
                jira_fixed: fixed,
                ..Default::default()
            })
            .0
            .as_deref(),
            Some("RHEL issue fixed: RHEL-1 — Done, fixed in rhel-10.3")
        );
        // blktrace: nothing on record.
        assert_eq!(landed_by(&Evidence::default()), (None, None));
    }

    /// untag and retire act on the same answer: for every kind of
    /// evidence, or none, untag's precondition passes exactly when
    /// retire's check for "landed" does. A change to either side's
    /// judgement fails here until the other follows.
    #[test]
    fn untag_and_retire_agree_on_what_landed() {
        let (c, m) = (carried(), missing());
        let cases = [
            Evidence {
                mr_state: Some("merged"),
                ..Default::default()
            },
            Evidence {
                patches: Some(&c),
                stock_nvr: Some("x-1-2.el10"),
                sig_nvr: Some("x-1-2~proposed.el10"),
                ..Default::default()
            },
            Evidence {
                patches: Some(&c),
                stock_nvr: Some("x-1-1.el10"),
                sig_nvr: Some("x-1-2~proposed.el10"),
                ..Default::default()
            },
            Evidence {
                patches: Some(&m),
                jira_fixed: Some("RHEL-1"),
                ..Default::default()
            },
            Evidence {
                stock_commit: Some("stock commit names it".into()),
                ..Default::default()
            },
            Evidence {
                jira_fixed: Some("RHEL-1"),
                ..Default::default()
            },
            Evidence::default(),
        ];
        for e in cases {
            let (by, doubt) = landed_by(&e);
            let landed = Landed {
                by: by.clone(),
                doubt: doubt.clone(),
                ..Default::default()
            };
            let facts = crate::retire::RetireFacts {
                landed_by: by.clone(),
                landed_doubt: doubt,
                ..Default::default()
            };
            let untag = matches!(crate::untag::precondition(&landed), Check::Pass(_));
            let retire = matches!(
                crate::retire::reason_check(crate::retire::Reason::Landed, &facts),
                Check::Pass(_)
            );
            assert_eq!(untag, retire, "untag and retire disagree on {e:?}");
            assert_eq!(untag, by.is_some());
        }
    }
}
