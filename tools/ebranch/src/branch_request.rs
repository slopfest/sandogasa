// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Branch-request filing and escalation.
//!
//! Ports the EPEL branch-request workflow from the old Python
//! `ebranch`: file a Bugzilla bug asking a maintainer to branch
//! and build a package for an EPEL release, then escalate
//! (`needinfo?`) requests that sit untouched.
//!
//! Three entry points back the CLI subcommands:
//!
//! - [`file_one`] — file a single request.
//! - [`file_batch`] — file requests for every missing package in
//!   a `check-crate --toml` report and link them together along
//!   the dependency graph (a package's request depends on the
//!   requests for the packages it needs).
//! - [`escalate`] — ping requests that have been NEW for at least
//!   a week.

use std::collections::{BTreeMap, BTreeSet};

use sandogasa_bugzilla::BzClient;

use crate::resolve::{self, BranchRequest, ResolveReport};

/// Tracker bug every branch request blocks, so the EPEL
/// Packagers SIG can see all outstanding requests at a glance.
pub const EPEL_SIG_TRACKER: &str = "EPELPackagersSIG";

/// Minimum age (days) before a NEW request is escalated.
pub const PING_MIN_DAYS: i64 = 7;

/// Options shared by the branch-request subcommands.
pub struct Options {
    pub bugzilla_url: String,
    pub api_key: String,
    pub branch: String,
    pub fas: Option<String>,
    pub sig: Option<String>,
    pub dry_run: bool,
    pub verbose: bool,
}

// ---- request / ping body templates (ported) ----

/// Build the request description, choosing the co-maintainer
/// offer based on whether a FAS and/or SIG was given. Returns an
/// error for the invalid SIG-without-FAS combination (you can't
/// volunteer a SIG to co-maintain without volunteering yourself).
pub fn request_description(
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
) -> Result<String, String> {
    let base = format!("Please branch and build {pkg} in {branch}.\n");
    match (fas, sig) {
        (None, None) => Ok(base),
        (Some(fas), None) => Ok(format!(
            "{base}\n\
             If you do not wish to maintain {pkg} in {branch},\n\
             or do not think you will be able to do this in a timely manner,\n\
             I would be happy to be a co-maintainer of the package (FAS: {fas});\n\
             please add me through https://src.fedoraproject.org/rpms/{pkg}/adduser\n"
        )),
        (Some(fas), Some(sig)) => Ok(format!(
            "{base}\n\
             If you do not wish to maintain {pkg} in {branch},\n\
             or do not think you will be able to do this in a timely manner,\n\
             the {sig} would be happy to be a co-maintainer of the package;\n\
             please add the {sig} group through\n\
             https://src.fedoraproject.org/rpms/{pkg}/addgroup\n\
             and grant it commit access, or collaborator access on epel* branches.\n\
             \n\
             Please also add me as a co-maintainer (FAS: {fas})\n\
             through https://src.fedoraproject.org/rpms/{pkg}/adduser\n"
        )),
        (None, Some(_)) => Err("cannot request a SIG be given access to a package without \
             signing up to co-maintain yourself (pass --fas as well)"
            .to_string()),
    }
}

/// Build the escalation (`needinfo?`) comment body.
pub fn ping_body(pkg: &str, branch: &str, fas: Option<&str>, sig: Option<&str>) -> String {
    let mut body = format!("Will you be able to branch and build {pkg} in {branch}?\n");
    match (fas, sig) {
        (Some(fas), Some(sig)) => {
            body.push_str(&format!(
                "\nThe {sig} would be happy to be a co-maintainer of the package\n\
                 if you do not wish to build it on {branch}.\n\
                 \n\
                 I would also be happy to be a co-maintainer (FAS: {fas}).\n"
            ));
        }
        (Some(fas), None) => {
            body.push_str(&format!(
                "\nI would be happy to be a co-maintainer if you do not wish\n\
                 to build it on {branch} (FAS: {fas}).\n"
            ));
        }
        (None, Some(sig)) => {
            body.push_str(&format!(
                "\nThe {sig} would be happy to be a co-maintainer of the package\n\
                 if you do not wish to build it on {branch}.\n"
            ));
        }
        (None, None) => {}
    }
    body
}

/// Outcome of deciding whether a request should be escalated.
#[derive(Debug, PartialEq, Eq)]
pub enum PingDecision {
    /// Escalate now.
    Ping,
    /// Already escalated previously.
    AlreadyPinged,
    /// Bug is closed — skip without comment.
    Closed,
    /// Bug is in a non-NEW open state; surface for manual review.
    NotNew(String),
    /// Bug is too recent; wait longer.
    TooNew(i64),
}

/// Decide whether to escalate a request given its current state.
pub fn ping_decision(status: &str, days_since_created: i64, already_pinged: bool) -> PingDecision {
    if already_pinged {
        return PingDecision::AlreadyPinged;
    }
    match status {
        "CLOSED" => PingDecision::Closed,
        "NEW" if days_since_created < PING_MIN_DAYS => PingDecision::TooNew(days_since_created),
        "NEW" => PingDecision::Ping,
        other => PingDecision::NotNew(other.to_string()),
    }
}

// ---- single file-request ----

/// File one branch request. Tries `Fedora EPEL`/`<branch>` first
/// and falls back to `Fedora`/`rawhide` when the component isn't
/// in EPEL. Returns the new bug ID.
pub async fn file_one(
    bz: &BzClient,
    pkg: &str,
    branch: &str,
    fas: Option<&str>,
    sig: Option<&str>,
    blocks: &[u64],
    depends_on: &[u64],
) -> Result<u64, String> {
    let summary = format!("Please branch and build {pkg} in {branch}");
    let description = request_description(pkg, branch, fas, sig)?;

    let epel = serde_json::json!({
        "product": "Fedora EPEL",
        "version": branch,
        "component": pkg,
        "summary": summary,
        "description": description,
        "blocks": blocks,
        "depends_on": depends_on,
    });
    let resp = bz
        .create(&epel)
        .await
        .map_err(|e| format!("Bugzilla create for {pkg} (EPEL) failed: {e}"))?;
    if let Some(id) = resp.id {
        return Ok(id);
    }

    // Component not in EPEL → request the Fedora branch instead.
    let fedora = serde_json::json!({
        "product": "Fedora",
        "version": "rawhide",
        "component": pkg,
        "summary": summary,
        "description": description,
        "blocks": blocks,
        "depends_on": depends_on,
    });
    let resp2 = bz
        .create(&fedora)
        .await
        .map_err(|e| format!("Bugzilla create for {pkg} (Fedora) failed: {e}"))?;
    if let Some(id) = resp2.id {
        return Ok(id);
    }

    Err(format!(
        "could not file request for {pkg}: EPEL: {}; Fedora: {}",
        resp.message.unwrap_or_else(|| "unknown error".into()),
        resp2.message.unwrap_or_else(|| "unknown error".into()),
    ))
}

/// Resolve a list of blocker/dependency tokens (numeric IDs or
/// Bugzilla aliases like `EPELPackagersSIG`) to bug IDs.
pub async fn resolve_refs(bz: &BzClient, tokens: &[String]) -> Result<Vec<u64>, String> {
    let mut ids = Vec::new();
    for tok in tokens {
        if let Ok(id) = tok.parse::<u64>() {
            ids.push(id);
        } else {
            let bug = bz
                .bug_by_alias(tok)
                .await
                .map_err(|e| format!("could not resolve bug alias '{tok}': {e}"))?;
            ids.push(bug.id);
        }
    }
    Ok(ids)
}

// ---- batch file-requests with linking ----

/// File requests for every package in `report` that doesn't
/// already have one, recording each in `report.branch_requests`,
/// then link them: a package's request `depends_on` the requests
/// for the packages it needs (following `report.edges`).
pub async fn file_batch(report: &mut ResolveReport, opts: &Options) -> Result<bool, String> {
    let bz = BzClient::new(&opts.bugzilla_url).with_api_key(opts.api_key.clone());

    if report.packages.is_empty() {
        println!("No packages in report to file requests for.");
        return Ok(false);
    }

    // Packages without a recorded request yet.
    let to_file: Vec<String> = report
        .packages
        .iter()
        .filter(|p| !report.branch_requests.contains_key(*p))
        .cloned()
        .collect();

    if opts.dry_run {
        for pkg in &to_file {
            println!("would file branch request for {pkg} in {}", opts.branch);
        }
        // Links among already-recorded requests (a re-run);
        // newly-filed ones don't have IDs to preview yet.
        preview_links(&report.edges, &report.branch_requests);
        return Ok(false);
    }

    let tracker = resolve_refs(&bz, &[EPEL_SIG_TRACKER.to_string()]).await?;

    let mut changed = false;
    for pkg in &to_file {
        let rhbz = file_one(
            &bz,
            pkg,
            &opts.branch,
            opts.fas.as_deref(),
            opts.sig.as_deref(),
            &tracker,
            &[],
        )
        .await?;
        println!("filed {pkg}: rhbz#{rhbz}");
        report.branch_requests.insert(
            pkg.clone(),
            BranchRequest {
                rhbz,
                pinged: false,
            },
        );
        changed = true;
    }

    // Link: each package's request depends on its dependencies'
    // requests. Done after filing so every rhbz exists.
    link_requests(&bz, &report.edges, &report.branch_requests, opts.verbose).await?;

    Ok(changed)
}

/// Add `depends_on` links between filed requests following the
/// package dependency edges. Only links packages that both have
/// a recorded request. Existing links aren't removed.
async fn link_requests(
    bz: &BzClient,
    pkg_edges: &BTreeMap<String, BTreeSet<String>>,
    requests: &BTreeMap<String, BranchRequest>,
    verbose: bool,
) -> Result<(), String> {
    for (pkg, deps) in pkg_edges {
        let Some(req) = requests.get(pkg) else {
            continue;
        };
        let dep_ids: Vec<u64> = deps
            .iter()
            .filter_map(|d| requests.get(d).map(|r| r.rhbz))
            .collect();
        if dep_ids.is_empty() {
            continue;
        }
        if verbose {
            eprintln!(
                "[file-requests] linking rhbz#{} depends_on {:?}",
                req.rhbz, dep_ids
            );
        }
        // Additive update so we never clobber unrelated links.
        let body = serde_json::json!({ "depends_on": { "add": dep_ids } });
        bz.update(req.rhbz, &body)
            .await
            .map_err(|e| format!("failed to link rhbz#{}: {e}", req.rhbz))?;
    }
    Ok(())
}

fn preview_links(
    pkg_edges: &BTreeMap<String, BTreeSet<String>>,
    requests: &BTreeMap<String, BranchRequest>,
) {
    for (pkg, deps) in pkg_edges {
        let Some(req) = requests.get(pkg) else {
            continue;
        };
        let dep_ids: Vec<u64> = deps
            .iter()
            .filter_map(|d| requests.get(d).map(|r| r.rhbz))
            .collect();
        if !dep_ids.is_empty() {
            println!("would link rhbz#{} depends_on {dep_ids:?}", req.rhbz);
        }
    }
}

// ---- escalation ----

/// Escalate stale requests recorded in `report`. Pings each NEW
/// request older than [`PING_MIN_DAYS`] that hasn't been pinged,
/// marking it pinged. Returns whether the report changed.
pub async fn escalate(report: &mut ResolveReport, opts: &Options) -> Result<bool, String> {
    let bz = BzClient::new(&opts.bugzilla_url).with_api_key(opts.api_key.clone());
    let now = chrono::Utc::now();
    let mut changed = false;

    for (pkg, req) in report.branch_requests.iter_mut() {
        let bug = bz
            .bug(req.rhbz)
            .await
            .map_err(|e| format!("failed to fetch rhbz#{} for {pkg}: {e}", req.rhbz))?;
        let days = (now - bug.creation_time).num_days();
        match ping_decision(&bug.status, days, req.pinged) {
            PingDecision::AlreadyPinged => {
                if opts.verbose {
                    eprintln!("[escalate] {pkg} rhbz#{} already pinged", req.rhbz);
                }
            }
            PingDecision::Closed => {
                if opts.verbose {
                    eprintln!("[escalate] {pkg} rhbz#{} closed, skipping", req.rhbz);
                }
            }
            PingDecision::NotNew(status) => {
                println!("{pkg} rhbz#{} is {status}, please check", req.rhbz);
            }
            PingDecision::TooNew(d) => {
                println!(
                    "{pkg} rhbz#{} created {d} day(s) ago, waiting until {PING_MIN_DAYS}",
                    req.rhbz
                );
            }
            PingDecision::Ping => {
                let body = ping_body(pkg, &opts.branch, opts.fas.as_deref(), opts.sig.as_deref());
                if opts.dry_run {
                    println!("would ping {pkg} rhbz#{} ({days} days old)", req.rhbz);
                    continue;
                }
                let update = serde_json::json!({
                    "comment": { "body": body },
                    "flags": [{
                        "name": "needinfo",
                        "status": "?",
                        "requestee": bug.assigned_to,
                    }],
                });
                bz.update(req.rhbz, &update)
                    .await
                    .map_err(|e| format!("failed to ping rhbz#{}: {e}", req.rhbz))?;
                println!("pinged {pkg} rhbz#{}", req.rhbz);
                req.pinged = true;
                changed = true;
            }
        }
    }
    Ok(changed)
}

// ---- sync entry points (own runtime, like review_deps) ----

/// `file-request` for a single package. When `report_path` is
/// set, the new bug id is recorded in that report.
pub fn run_file_request(
    pkg: &str,
    blocked: &[String],
    dependson: &[String],
    report_path: Option<&str>,
    opts: &Options,
) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        // Validate up front (catches SIG-without-FAS) — no network.
        request_description(pkg, &opts.branch, opts.fas.as_deref(), opts.sig.as_deref())?;
        if opts.dry_run {
            println!("would file branch request for {pkg} in {}", opts.branch);
            return Ok(());
        }

        let bz = BzClient::new(&opts.bugzilla_url).with_api_key(opts.api_key.clone());
        let mut blocks = resolve_refs(&bz, blocked).await?;
        if blocks.is_empty() {
            blocks = resolve_refs(&bz, &[EPEL_SIG_TRACKER.to_string()]).await?;
        }
        let depends_on = resolve_refs(&bz, dependson).await?;

        let rhbz = file_one(
            &bz,
            pkg,
            &opts.branch,
            opts.fas.as_deref(),
            opts.sig.as_deref(),
            &blocks,
            &depends_on,
        )
        .await?;
        println!("filed {pkg}: rhbz#{rhbz}");

        if let Some(path) = report_path {
            let mut report = resolve::load_report(path)?;
            report.branch_requests.insert(
                pkg.to_string(),
                BranchRequest {
                    rhbz,
                    pinged: false,
                },
            );
            resolve::write_report(&report, path)?;
        }
        Ok(())
    })
}

/// `file-requests` — batch over a resolve report file.
pub fn run_file_requests(report_path: &str, opts: &Options) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        let mut report = resolve::load_report(report_path)?;
        let changed = file_batch(&mut report, opts).await?;
        if changed && !opts.dry_run {
            resolve::write_report(&report, report_path)?;
        }
        Ok(())
    })
}

/// `escalate` — ping stale requests in a resolve report file.
pub fn run_escalate(report_path: &str, opts: &Options) -> Result<(), String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("failed to create async runtime: {e}"))?;
    rt.block_on(async {
        let mut report = resolve::load_report(report_path)?;
        if report.branch_requests.is_empty() {
            println!("No branch requests recorded in {report_path}.");
            return Ok(());
        }
        let changed = escalate(&mut report, opts).await?;
        if changed && !opts.dry_run {
            resolve::write_report(&report, report_path)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_plain() {
        let d = request_description("foo", "epel9", None, None).unwrap();
        assert_eq!(d, "Please branch and build foo in epel9.\n");
    }

    #[test]
    fn description_fas_offers_comaintainer() {
        let d = request_description("foo", "epel9", Some("alice"), None).unwrap();
        assert!(d.contains("co-maintainer of the package (FAS: alice)"));
        assert!(d.contains("/rpms/foo/adduser"));
        assert!(!d.contains("addgroup"));
    }

    #[test]
    fn description_fas_sig_offers_both() {
        let d =
            request_description("foo", "epel9", Some("alice"), Some("EPEL Packagers SIG")).unwrap();
        assert!(d.contains("the EPEL Packagers SIG would be happy"));
        assert!(d.contains("/rpms/foo/addgroup"));
        assert!(d.contains("Please also add me as a co-maintainer (FAS: alice)"));
    }

    #[test]
    fn description_sig_without_fas_errors() {
        let err = request_description("foo", "epel9", None, Some("SIG")).unwrap_err();
        assert!(err.contains("without"));
    }

    #[test]
    fn ping_body_plain_and_offers() {
        assert_eq!(
            ping_body("foo", "epel9", None, None),
            "Will you be able to branch and build foo in epel9?\n"
        );
        assert!(ping_body("foo", "epel9", Some("alice"), None).contains("FAS: alice"));
        let both = ping_body("foo", "epel9", Some("alice"), Some("EPEL Packagers SIG"));
        assert!(both.contains("EPEL Packagers SIG"));
        assert!(both.contains("FAS: alice"));
    }

    #[test]
    fn ping_decision_rules() {
        assert_eq!(ping_decision("NEW", 10, true), PingDecision::AlreadyPinged);
        assert_eq!(ping_decision("CLOSED", 30, false), PingDecision::Closed);
        assert_eq!(ping_decision("NEW", 10, false), PingDecision::Ping);
        assert_eq!(ping_decision("NEW", 3, false), PingDecision::TooNew(3));
        assert_eq!(
            ping_decision("ASSIGNED", 30, false),
            PingDecision::NotNew("ASSIGNED".to_string())
        );
        // Exactly at the threshold pings.
        assert_eq!(
            ping_decision("NEW", PING_MIN_DAYS, false),
            PingDecision::Ping
        );
    }
}
