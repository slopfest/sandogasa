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
use crate::ping::{evidence_tokens, find_evidence_commit};
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
    /// When the change landed, by the evidence that decided it (#92),
    /// and that evidence in a few words: the stock commit that added the
    /// matched patch, the stock commit naming the change, the MR's
    /// merge, or Jira's resolution when only Jira decided.
    pub on: Option<chrono::NaiveDate>,
    pub summary: Option<String>,
}

/// What [`landed_by`] decided: what says the change is in stock, what
/// keeps it from counting, and when it landed, by the same evidence.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Decision {
    pub by: Option<String>,
    pub doubt: Option<String>,
    pub on: Option<chrono::NaiveDate>,
    pub summary: Option<String>,
}

/// What Jira says, as [`landed`] weighs it: every RHEL issue fixed
/// (described), and when the last was resolved.
#[derive(Debug, Default, Clone, Copy)]
pub struct JiraFacts<'a> {
    pub fixed: Option<&'a str>,
    pub on: Option<chrono::NaiveDate>,
}

/// A dated piece of evidence: when, and what in a few words.
#[derive(Debug, Clone)]
pub struct Dated {
    pub on: Option<chrono::NaiveDate>,
    pub summary: String,
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
    /// A commit on stock's branch naming the change, described, and the
    /// same commit dated.
    pub stock_commit: Option<String>,
    pub stock_commit_dated: Option<Dated>,
    /// The stock commit that added the patch the update's first patch
    /// matched, dated.
    pub patch_added: Option<Dated>,
    /// When the MR merged.
    pub merged_on: Option<chrono::NaiveDate>,
    /// Every RHEL issue fixed (#87), described; `None` otherwise; and
    /// when the last was resolved.
    pub jira_fixed: Option<&'a str>,
    pub jira_on: Option<chrono::NaiveDate>,
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
pub fn landed_by(e: &Evidence) -> Decision {
    if e.mr_state == Some("merged") {
        return Decision {
            by: Some("MR merged".to_string()),
            on: e.merged_on,
            summary: Some("the SIG's MR merged".to_string()),
            ..Default::default()
        };
    }
    match e.patches {
        Some(c) if c.all_carried() => {
            let carried: Vec<String> = c
                .matched
                .iter()
                .map(|(ours, theirs, how)| format!("{ours} as {theirs} ({how})"))
                .collect();
            if stock_at_or_past(e.stock_nvr, e.sig_nvr) {
                Decision {
                    by: Some(format!(
                        "stock {} carries every patch the update adds: {}",
                        e.stock_nvr.unwrap_or("?"),
                        carried.join("; ")
                    )),
                    on: e.patch_added.as_ref().and_then(|d| d.on),
                    summary: e.patch_added.as_ref().map(|d| d.summary.clone()),
                    ..Default::default()
                }
            } else {
                Decision {
                    doubt: Some(format!(
                        "stock carries every patch the update adds, but its build ({}) is not \
                         yet past the SIG's ({}): wait for it",
                        e.stock_nvr.unwrap_or("unknown"),
                        e.sig_nvr.unwrap_or("unknown")
                    )),
                    ..Default::default()
                }
            }
        }
        Some(c) => {
            let also = match (&e.stock_commit, e.jira_fixed) {
                (_, Some(j)) => format!(", though {j}"),
                (Some(s), None) => format!(", though {s}"),
                (None, None) => String::new(),
            };
            Decision {
                doubt: Some(format!(
                    "stock does not carry {} of the patches the update adds{also}",
                    c.missing.join(", ")
                )),
                ..Default::default()
            }
        }
        None => match (&e.stock_commit, e.jira_fixed) {
            (Some(s), _) => Decision {
                by: Some(s.clone()),
                on: e.stock_commit_dated.as_ref().and_then(|d| d.on),
                summary: e.stock_commit_dated.as_ref().map(|d| d.summary.clone()),
                ..Default::default()
            },
            (None, Some(j)) => Decision {
                by: Some(format!("RHEL issue fixed: {j}")),
                on: e.jira_on,
                summary: Some(format!("RHEL issue fixed: {j}")),
                ..Default::default()
            },
            (None, None) => Decision::default(),
        },
    }
}

/// The RHEL issues' state as `untag` and `retire` both print it:
/// context, not a check, since stock's evidence decides (#89, #92).
pub fn jira_note(detail: &str) -> String {
    format!("JIRA (for the record): {detail}")
}

/// What Jira recorded, set beside the evidence so its dates are not
/// mistaken for when the change landed: the SIG's issue can be closed
/// months after the fix reached stock (RHEL-170526 was closed `Not a
/// Bug` on 2026-10-23, the fix in stock since 2026-04-27). Its verdict
/// when it shows no fix, or its date when that differs from the
/// evidence's; `None` when they agree.
pub fn jira_disagreement(
    keys: &[String],
    jira_fixed: bool,
    resolution: Option<&str>,
    resolved_on: Option<chrono::NaiveDate>,
    landed_on: Option<chrono::NaiveDate>,
) -> Option<String> {
    let keys = keys.join(", ");
    let when = resolved_on.map(|d| format!(" on {d}")).unwrap_or_default();
    if !jira_fixed {
        return resolution.map(|r| format!("Jira closed {keys} as {r}{when}"));
    }
    match (resolved_on, landed_on) {
        (Some(j), Some(l)) if j != l => Some(format!("Jira marked {keys} fixed on {j}")),
        _ => None,
    }
}

/// The `- **Stock fix**:` line `retire` records for a change the
/// evidence shows in stock: the build, when it landed and by what, and
/// what Jira says when it disagrees.
pub fn stock_fix_line(
    stock_nvr: &str,
    on: chrono::NaiveDate,
    summary: &str,
    disagreement: Option<&str>,
) -> String {
    let also = disagreement.map(|d| format!("; {d}")).unwrap_or_default();
    format!("- **Stock fix**: {stock_nvr} — in stock on {on} ({summary}){also}")
}

/// The day a `- **Stock fix**:` line says the change was in stock.
pub fn stock_fix_on(body: &str) -> Option<chrono::NaiveDate> {
    let (_, note) = crate::utils::parse_stock_fix_line(body)?;
    let rest = note.split_once("in stock on ")?.1;
    chrono::NaiveDate::parse_from_str(rest.get(..10)?, "%Y-%m-%d").ok()
}

/// A commit's date, from GitLab's RFC 3339 `committed_date`.
fn commit_day(c: &sandogasa_gitlab::RepoCommit) -> Option<chrono::NaiveDate> {
    crate::utils::parse_iso_date(c.committed_date.as_deref()?)
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
    jira: JiraFacts,
    verbose: bool,
) -> Landed {
    let JiraFacts {
        fixed: jira_fixed,
        on: jira_on,
    } = jira;
    let base = crate::utils::gitlab_base();
    let body = issue.description.as_deref().unwrap_or("");
    let mr_url = parse_mr_line(body)
        .map(|(u, _)| u)
        .or_else(|| scan_mr_url_in_body(body));
    let stock_project = format!("redhat/centos-stream/rpms/{package}");
    let mut mr_state = None;
    let mut mr_title = String::new();
    let mut mr_branch = String::new();
    let mut merged_on = None;
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
                            merged_on = mr
                                .merged_at
                                .as_deref()
                                .and_then(crate::utils::parse_iso_date);
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
    let (stock_commit, stock_commit_dated) = if mr_state.as_deref() == Some("merged") {
        (None, None)
    } else {
        let tokens = evidence_tokens(body, &issue.title, &mr_title, &mr_branch);
        match gitlab::reader(&base, &stock_project).and_then(|c| c.branch_commits(release)) {
            Ok(commits) => match find_evidence_commit(&commits, &tokens) {
                Some((c, token)) => {
                    let short = &c.id[..c.id.len().min(8)];
                    (
                        Some(format!(
                            "stock commit {short} ({}) names {token}",
                            c.title.trim()
                        )),
                        Some(Dated {
                            on: commit_day(c),
                            summary: format!("stock commit {short} names {token}"),
                        }),
                    )
                }
                None => (None, None),
            },
            Err(e) => {
                if verbose {
                    eprintln!("[cpu-sig-tracker] cannot read stock history: {e}");
                }
                (None, None)
            }
        }
    };
    // When stock got the matched patch: the oldest commit touching it.
    let patch_added = patches
        .as_ref()
        .filter(|c| c.all_carried())
        .and_then(|c| c.matched.first())
        .and_then(|(_, theirs, _)| {
            let commits = gitlab::reader(&base, &stock_project)
                .and_then(|c| c.path_commits(release, theirs))
                .ok()?;
            let first = commits.last()?;
            Some(Dated {
                on: commit_day(first),
                summary: format!(
                    "stock commit {} added {theirs}",
                    &first.id[..first.id.len().min(8)]
                ),
            })
        });
    let d = landed_by(&Evidence {
        mr_state: mr_state.as_deref(),
        patches: patches.as_ref(),
        sig_nvr,
        stock_nvr,
        stock_commit,
        stock_commit_dated,
        patch_added,
        merged_on,
        jira_fixed,
        jira_on,
    });
    Landed {
        mr_url,
        mr_state,
        by: d.by,
        doubt: d.doubt,
        patches,
        on: d.on,
        summary: d.summary,
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
            .by
            .as_deref(),
            Some("MR merged")
        );
        // PackageKit c10s: MR closed, RHEL issue Not a Bug, but stock's
        // build carries the patch under Red Hat's own name.
        let c = carried();
        let Decision { by, doubt, .. } = landed_by(&Evidence {
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
        let Decision { by, doubt, .. } = landed_by(&Evidence {
            patches: Some(&c),
            sig_nvr: Some("x-1.2-3~proposed.el10"),
            stock_nvr: Some("x-1.2-2.el10"),
            ..Default::default()
        });
        assert!(by.is_none() && doubt.unwrap().contains("not yet past"));
        // A patch missing in stock outweighs Jira and commit messages.
        let m = missing();
        let Decision { by, doubt, .. } = landed_by(&Evidence {
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
            .by,
            stock_commit()
        );
        assert_eq!(
            landed_by(&Evidence {
                jira_fixed: fixed,
                ..Default::default()
            })
            .by
            .as_deref(),
            Some("RHEL issue fixed: RHEL-1 — Done, fixed in rhel-10.3")
        );
        // blktrace: nothing on record.
        assert_eq!(landed_by(&Evidence::default()), Decision::default());
    }

    fn day(m: u32, d: u32) -> Option<chrono::NaiveDate> {
        chrono::NaiveDate::from_ymd_opt(2026, m, d)
    }

    #[test]
    fn the_date_comes_from_the_evidence_that_decided() {
        let c = carried();
        // PackageKit c10s: stock's commit added the patch on 04-27; Jira
        // closed the SIG's issue on 10-23.
        let d = landed_by(&Evidence {
            patches: Some(&c),
            stock_nvr: Some("PackageKit-1.2.8-9.el10"),
            patch_added: Some(Dated {
                on: day(4, 27),
                summary: "stock commit 99e0f170 added 0001-x.patch".into(),
            }),
            jira_on: day(10, 23),
            ..Default::default()
        });
        assert_eq!(d.on, day(4, 27));
        assert_eq!(
            d.summary.as_deref(),
            Some("stock commit 99e0f170 added 0001-x.patch")
        );
        // Only Jira decided: its date.
        let d = landed_by(&Evidence {
            jira_fixed: Some("RHEL-1 — Done, fixed in rhel-10.3"),
            jira_on: day(6, 1),
            ..Default::default()
        });
        assert_eq!(d.on, day(6, 1));
        // The MR merged: its merge date.
        let d = landed_by(&Evidence {
            mr_state: Some("merged"),
            merged_on: day(3, 2),
            jira_on: day(9, 9),
            ..Default::default()
        });
        assert_eq!(d.on, day(3, 2));
    }

    #[test]
    fn jira_s_record_is_kept_beside_the_evidence() {
        let keys = ["RHEL-170526".to_string()];
        assert_eq!(
            jira_disagreement(&keys, false, Some("Not a Bug"), day(10, 23), day(4, 27)).as_deref(),
            Some("Jira closed RHEL-170526 as Not a Bug on 2026-10-23")
        );
        assert_eq!(
            jira_disagreement(&keys, true, Some("Done"), day(5, 1), day(4, 27)).as_deref(),
            Some("Jira marked RHEL-170526 fixed on 2026-05-01")
        );
        assert_eq!(
            jira_disagreement(&keys, true, Some("Done"), day(4, 27), day(4, 27)),
            None
        );
        let line = stock_fix_line(
            "PackageKit-1.2.8-9.el10",
            day(4, 27).unwrap(),
            "stock commit 99e0f170 added 0001-x.patch",
            Some("Jira closed RHEL-170526 as Not a Bug on 2026-10-23"),
        );
        assert_eq!(
            line,
            "- **Stock fix**: PackageKit-1.2.8-9.el10 — in stock on 2026-04-27 (stock commit \
             99e0f170 added 0001-x.patch); Jira closed RHEL-170526 as Not a Bug on 2026-10-23"
        );
        // A refresh reads the day back from the issue.
        let body = format!("- **Release**: c10s\n{line}\n");
        assert_eq!(stock_fix_on(&body), day(4, 27));
        // An operator's line carries the day it was verified, not a fix
        // date, and is left to Jira.
        assert_eq!(
            stock_fix_on("- **Stock fix**: x-1-1.el10 — verified by salimma on 2026-09-24\n"),
            None
        );
        assert_eq!(
            jira_note("RHEL-1 was closed Not a Bug: not fixed in RHEL"),
            "JIRA (for the record): RHEL-1 was closed Not a Bug: not fixed in RHEL"
        );
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
            let Decision { by, doubt, .. } = landed_by(&e);
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
