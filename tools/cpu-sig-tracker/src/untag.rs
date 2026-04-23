// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `untag` subcommand.
//!
//! Removes a proposed_updates build from its CBS `-release`
//! tag after verifying the JIRA on the corresponding tracking
//! issue is resolved. Typical flow: `status --refresh` flags a
//! package as `untag-candidate` → run `untag <nvr> --release
//! c10s` → optionally follow with `retire` to close the
//! tracking issue once the build is gone.

use std::io::{BufRead, Write};
use std::process::ExitCode;

use sandogasa_koji::{list_tagged_nvrs, parse_nvr_name, untag_build};

use crate::dump_inventory::{proposed_updates_tag, proposed_updates_testing_tag};
use crate::{gitlab, jira};

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
const GITLAB_BASE: &str = "https://gitlab.com";
const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct UntagArgs {
    /// Either a package name (e.g. `xz` — the tool discovers
    /// the currently-tagged NVR in `-release` and `-testing`)
    /// or a specific NVR (e.g. `xz-5.6.4-1~proposed.el10`).
    pub target: String,

    /// CentOS release whose proposed_updates tags to untag from
    /// (`c9s`, `c10s`, …).
    #[arg(long)]
    pub release: String,

    /// Skip the interactive confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,

    /// Skip the JIRA-resolved precondition. Use when the
    /// tracking issue can't be located or you're sure the
    /// check would pass.
    #[arg(long)]
    pub force: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

pub fn run(args: &UntagArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &UntagArgs) -> Result<(), Box<dyn std::error::Error>> {
    let release_tag = proposed_updates_tag(&args.release)?;
    let testing_tag = proposed_updates_testing_tag(&args.release)?;

    if args.verbose {
        eprintln!("[cpu-sig-tracker] listing tagged NVRs in {release_tag}");
    }
    let release_nvrs = list_tagged_nvrs(&release_tag, Some(KOJI_PROFILE))?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] listing tagged NVRs in {testing_tag}");
    }
    let testing_nvrs = list_tagged_nvrs(&testing_tag, Some(KOJI_PROFILE))?;

    let resolved = resolve_target(
        &args.target,
        &release_tag,
        &release_nvrs,
        &testing_tag,
        &testing_nvrs,
    )?;

    let jira_check = check_tracking_issue_jira(&resolved.package, &args.release, args.verbose);
    report_check("JIRA resolved", &jira_check);

    if !matches!(jira_check, Check::Pass(_)) && !args.force {
        return Err(
            "precondition not met; re-run with --force to override, or fix the \
             underlying state (e.g. close the JIRA before untagging)"
                .into(),
        );
    }

    println!();
    println!("Will untag:");
    for (tag, nvr) in &resolved.targets {
        println!("  {nvr} from {tag}");
    }
    println!("  package: {}", resolved.package);
    println!("  release: {}", args.release);
    if !args.yes && !confirm("Proceed?")? {
        eprintln!("aborted.");
        return Ok(());
    }

    let mut errors = 0usize;
    for (tag, nvr) in &resolved.targets {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] koji untag-build {tag} {nvr}");
        }
        if let Err(e) = untag_build(tag, nvr, Some(KOJI_PROFILE)) {
            eprintln!("error: untag-build {tag} {nvr}: {e}");
            errors += 1;
        } else {
            eprintln!("untagged {nvr} from {tag}");
        }
    }
    if errors > 0 {
        return Err(format!("{errors} untag operation(s) failed").into());
    }
    Ok(())
}

/// Result of resolving a user-supplied target to concrete
/// (tag, NVR) pairs to untag.
#[derive(Debug)]
struct Resolved {
    package: String,
    targets: Vec<(String, String)>,
}

/// Decide whether `target` names a specific NVR or a package,
/// and list every (tag, NVR) pair we'd untag. Exact NVR match
/// wins over package match; package match sweeps both tags.
fn resolve_target(
    target: &str,
    release_tag: &str,
    release_nvrs: &[String],
    testing_tag: &str,
    testing_nvrs: &[String],
) -> Result<Resolved, String> {
    let mut targets: Vec<(String, String)> = Vec::new();
    if release_nvrs.iter().any(|n| n == target) {
        targets.push((release_tag.to_string(), target.to_string()));
    }
    if testing_nvrs.iter().any(|n| n == target) {
        targets.push((testing_tag.to_string(), target.to_string()));
    }
    if !targets.is_empty() {
        let package = parse_nvr_name(target)
            .ok_or_else(|| format!("could not parse package name out of NVR '{target}'"))?
            .to_string();
        return Ok(Resolved { package, targets });
    }

    // No exact NVR match — try as a package name.
    for nvr in release_nvrs {
        if parse_nvr_name(nvr) == Some(target) {
            targets.push((release_tag.to_string(), nvr.clone()));
        }
    }
    for nvr in testing_nvrs {
        if parse_nvr_name(nvr) == Some(target) {
            targets.push((testing_tag.to_string(), nvr.clone()));
        }
    }
    if targets.is_empty() {
        return Err(format!(
            "'{target}' is neither a currently-tagged NVR nor a \
             package name with builds in {release_tag} or {testing_tag}",
        ));
    }
    Ok(Resolved {
        package: target.to_string(),
        targets,
    })
}

/// Locate the tracking issue for (package, release) in the
/// `cpu-sig-tracker`-labeled group and check whether its JIRA
/// is resolved.
fn check_tracking_issue_jira(package: &str, release: &str, verbose: bool) -> Check {
    let group_client = match gitlab::GroupClient::new(GITLAB_BASE, PROPOSED_UPDATES_GROUP) {
        Ok(c) => c,
        Err(e) => return Check::Skipped(format!("GitLab group client: {e}")),
    };
    let label = format!("{TRACKING_LABEL},{release}");
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching tracking issues for label {label}");
    }
    // Untag commonly runs after retire (which closes the
    // tracking issue), so scan all states — a closed issue is
    // still the source of truth for which JIRA to verify.
    let issues = match group_client.list_issues(&label, None) {
        Ok(v) => v,
        Err(e) => return Check::Skipped(format!("list_issues({label}): {e}")),
    };
    let issue = match issues
        .into_iter()
        .find(|i| gitlab::package_from_issue_url(&i.web_url) == Some(package))
    {
        Some(i) => i,
        None => {
            return Check::Fail(format!(
                "no tracking issue found for {package} in {release}",
            ));
        }
    };
    let body = issue.description.as_deref().unwrap_or("");
    let Some(key) = parse_jira_key_from_body(body) else {
        return Check::Fail(format!(
            "tracking issue {} has no JIRA key in body",
            issue.web_url,
        ));
    };
    fetch_jira_resolved(&key, verbose)
}

fn fetch_jira_resolved(key: &str, verbose: bool) -> Check {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => return Check::Skipped(format!("tokio runtime init failed: {e}")),
    };
    let jira_client = jira::client();
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching JIRA {key}");
    }
    match runtime.block_on(jira_client.issue(key)) {
        Ok(Some(issue)) if issue.is_resolved() => {
            let summary = match issue.resolution() {
                Some(r) => format!("{} ({})", issue.status(), r),
                None => issue.status().to_string(),
            };
            Check::Pass(format!("{key} — {summary}"))
        }
        Ok(Some(issue)) => Check::Fail(format!("{key} is {} (not resolved)", issue.status())),
        Ok(None) => Check::Skipped(format!("JIRA {key} not visible")),
        Err(e) => Check::Skipped(format!("JIRA {key} fetch failed: {e}")),
    }
}

/// Find the `RHEL-\d+` key in `- **JIRA**: [KEY](...)`.
fn parse_jira_key_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(end) = rest.find(']')
        {
            return Some(rest[..end].to_string());
        }
    }
    None
}

enum Check {
    Pass(String),
    Fail(String),
    Skipped(String),
}

impl Check {
    fn label(&self) -> &'static str {
        match self {
            Check::Pass(_) => "ok",
            Check::Fail(_) => "FAIL",
            Check::Skipped(_) => "skipped",
        }
    }

    fn detail(&self) -> &str {
        match self {
            Check::Pass(d) | Check::Fail(d) | Check::Skipped(d) => d,
        }
    }
}

fn report_check(name: &str, check: &Check) {
    println!("check {name}: {} — {}", check.label(), check.detail());
}

fn confirm(prompt: &str) -> Result<bool, Box<dyn std::error::Error>> {
    eprint!("{prompt} [y/N]: ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim().eq_ignore_ascii_case("y"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_jira_key_from_standard_body() {
        let body = "- **JIRA**: [RHEL-1](https://example/) — Closed (Done)\n";
        assert_eq!(parse_jira_key_from_body(body).as_deref(), Some("RHEL-1"));
    }

    #[test]
    fn parse_jira_key_missing() {
        assert_eq!(parse_jira_key_from_body("no jira"), None);
    }

    #[test]
    fn resolve_exact_nvr_in_release_only() {
        let r = resolve_target(
            "xz-5.6.4-1.el10",
            "proposed_updates10s-packages-main-release",
            &["xz-5.6.4-1.el10".to_string()],
            "proposed_updates10s-packages-main-testing",
            &[],
        )
        .unwrap();
        assert_eq!(r.package, "xz");
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].1, "xz-5.6.4-1.el10");
        assert!(r.targets[0].0.ends_with("-release"));
    }

    #[test]
    fn resolve_exact_nvr_in_both_tags() {
        let r = resolve_target(
            "xz-5.6.4-1.el10",
            "proposed_updates10s-packages-main-release",
            &["xz-5.6.4-1.el10".to_string()],
            "proposed_updates10s-packages-main-testing",
            &["xz-5.6.4-1.el10".to_string()],
        )
        .unwrap();
        assert_eq!(r.targets.len(), 2);
    }

    #[test]
    fn resolve_package_name_picks_up_both_tags() {
        let r = resolve_target(
            "xz",
            "proposed_updates10s-packages-main-release",
            &["xz-5.6.4-1.el10".to_string()],
            "proposed_updates10s-packages-main-testing",
            &["xz-5.6.5-1.el10".to_string()],
        )
        .unwrap();
        assert_eq!(r.package, "xz");
        assert_eq!(r.targets.len(), 2);
        // Release first, testing second (matches iteration order).
        assert_eq!(r.targets[0].1, "xz-5.6.4-1.el10");
        assert_eq!(r.targets[1].1, "xz-5.6.5-1.el10");
    }

    #[test]
    fn resolve_hyphenated_package_name() {
        // `intel-gpu-tools` looks like an NVR to a naive rsplit
        // parser, so the exact-match path must miss it and the
        // package-match path must still find it.
        let r = resolve_target(
            "intel-gpu-tools",
            "proposed_updates10s-packages-main-release",
            &["intel-gpu-tools-1.28-2.el10".to_string()],
            "proposed_updates10s-packages-main-testing",
            &[],
        )
        .unwrap();
        assert_eq!(r.package, "intel-gpu-tools");
        assert_eq!(r.targets.len(), 1);
        assert_eq!(r.targets[0].1, "intel-gpu-tools-1.28-2.el10");
    }

    #[test]
    fn resolve_nothing_tagged_errors() {
        let err = resolve_target(
            "xz",
            "proposed_updates10s-packages-main-release",
            &[],
            "proposed_updates10s-packages-main-testing",
            &[],
        )
        .unwrap_err();
        assert!(err.contains("neither"));
    }
}
