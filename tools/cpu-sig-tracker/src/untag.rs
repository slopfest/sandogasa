// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `untag` subcommand.
//!
//! Removes a proposed_updates build from its CBS `-release`
//! tag after verifying the JIRA on the corresponding tracking
//! issue is resolved. Typical flow: `status --refresh` flags a
//! package as `untag-candidate` → run `untag <nvr> --release
//! c10s` → optionally follow with `retire` to close the
//! tracking issue once the build is gone.
//!
//! Its steps are koji commands a person could run, so `--explain`
//! narrates each, shows the exact command and pauses before running it;
//! `--dry-run` shows them and runs only the reads (#91). The check that
//! the change is in stock has no one command, so it is narrated as a
//! step with its result.

use std::process::ExitCode;

use sandogasa_cli::explain::Ui;

use sandogasa_koji::{list_tagged_nvrs, parse_nvr_name, untag_build};

use crate::dump_inventory::{proposed_updates_tag, proposed_updates_testing_tag};
use crate::gitlab;

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const TRACKING_LABEL: &str = "cpu-sig-tracker";
use crate::utils::{Check, gitlab_base, parse_jira_keys_from_body, report_check};
const KOJI_PROFILE: &str = "cbs";

#[derive(clap::Args)]
pub struct UntagArgs {
    /// Either a package name (e.g. `xz` — the tool discovers
    /// the currently-tagged NVR in `-release` and `-testing`)
    /// or a specific NVR (e.g. `xz-5.6.4-1~proposed.el10`).
    pub target: String,

    /// Show each step and its koji command, untagging nothing (the
    /// read-only lookups still run)
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its koji command, pausing before
    /// each
    #[arg(long)]
    pub explain: bool,

    /// Skip the landed-in-stock precondition. Use when the
    /// tracking issue can't be located or you're sure the
    /// change is in stock.
    #[arg(long)]
    pub force: bool,

    /// CentOS release whose proposed_updates tags to untag from
    /// (`c9s`, `c10s`, …).
    #[arg(long)]
    pub release: String,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,

    /// Skip the interactive confirmation prompt.
    #[arg(short, long)]
    pub yes: bool,
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

/// A koji command against CBS, as `untag` runs it.
fn koji_argv(args: &[&str]) -> Vec<String> {
    ["koji", "--profile", KOJI_PROFILE]
        .iter()
        .chain(args)
        .map(|s| s.to_string())
        .collect()
}

pub(crate) fn run_inner(args: &UntagArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let release_tag = proposed_updates_tag(&args.release)?;
    let testing_tag = proposed_updates_testing_tag(&args.release)?;

    // The login first: one cheap call, before anything else, so a
    // missing certificate is found and explained (and, at a terminal,
    // fixed) up front (#90).
    ui.step("Check the CBS login (it uses the TLS certificate the cbs profile names)");
    ui.show_command(&koji_argv(&["moshimoshi"]));
    ui.pause_if_explain();
    sandogasa_cbs::login::ensure_login("cpu-sig-tracker", !args.yes, args.verbose)?;

    ui.step("Find the builds in the SIG's -release and -testing tags");
    ui.show_command(&koji_argv(&["list-tagged", "--quiet", "--", &release_tag]));
    ui.show_command(&koji_argv(&["list-tagged", "--quiet", "--", &testing_tag]));
    ui.pause_if_explain();
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

    // No single command answers this: GitLab, stock's patches and Jira.
    ui.step(
        "Check the change is in stock: the patches its MR adds against stock's, then its \
         RHEL issues",
    );
    let sig_nvr = resolved.targets.first().map(|(_, nvr)| nvr.as_str());
    let landed_check = check_landed(&resolved.package, &args.release, sig_nvr, args.verbose);
    report_check("landed in stock", &landed_check);

    if !matches!(landed_check, Check::Pass(_)) && !args.force {
        return Err(
            "precondition not met: nothing says the change is in stock; re-run with --force \
             to override, or retire the update for another reason first"
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
    if !args.dry_run && !args.yes && !sandogasa_cli::confirm("Proceed?", false)? {
        eprintln!("aborted.");
        return Ok(());
    }

    let mut errors = 0usize;
    for (tag, nvr) in &resolved.targets {
        ui.step(&format!("Remove {nvr} from {tag}"));
        ui.show_command(&koji_argv(&["untag-build", "--", tag, nvr]));
        if args.dry_run {
            continue;
        }
        ui.pause_if_explain();
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
    if args.dry_run {
        println!("\nDry run: nothing untagged.");
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

/// Whether the update's change is in stock, the precondition for
/// removing its build: the same evidence `retire` takes as landed, from
/// [`crate::landed::landed`], so the two cannot disagree (a test holds
/// [`precondition`] to `retire`'s check). The tracking issue names the
/// MR and the RHEL issues; untag often runs after retire, so closed
/// issues count.
fn check_landed(package: &str, release: &str, sig_nvr: Option<&str>, verbose: bool) -> Check {
    let group_client = match gitlab::group_client(&gitlab_base(), PROPOSED_UPDATES_GROUP) {
        Ok(c) => c,
        Err(e) => return Check::Skipped(format!("GitLab group client: {e}")),
    };
    let label = format!("{TRACKING_LABEL},{release}");
    if verbose {
        eprintln!("[cpu-sig-tracker] fetching tracking issues for label {label}");
    }
    let issues = match group_client.list_issues(&label, None) {
        Ok(v) => v,
        Err(e) => return Check::Skipped(format!("list_issues({label}): {e}")),
    };
    let Some(issue) = issues
        .into_iter()
        .find(|i| gitlab::package_from_issue_url(&i.web_url) == Some(package))
    else {
        return Check::Fail(format!(
            "no tracking issue found for {package} in {release}",
        ));
    };
    let body = issue.description.as_deref().unwrap_or("");
    let jira = crate::jira::check_all_resolved(&parse_jira_keys_from_body(body), verbose);
    // Context, not the gate: whether the change landed is decided
    // below, and stock's patches outrank Jira (#89), so a declined or
    // unverified issue is no failure here.
    println!("{}", crate::landed::jira_note(jira.check.detail()));
    let stock_nvr =
        crate::status::fetch_stream_nvrs(release, &[package.to_string()], verbose).remove(package);
    precondition(&crate::landed::landed(
        &issue,
        package,
        release,
        sig_nvr,
        stock_nvr.as_deref(),
        crate::landed::JiraFacts {
            fixed: jira.fixed.then(|| jira.check.detail()),
            on: jira.resolution_date,
        },
        verbose,
    ))
}

/// The untag precondition from what [`crate::landed::landed`] found:
/// passed when something says the change is in stock.
pub fn precondition(landed: &crate::landed::Landed) -> Check {
    match (&landed.by, &landed.doubt) {
        (Some(why), _) => Check::Pass(why.clone()),
        (None, Some(doubt)) => Check::Fail(doubt.clone()),
        (None, None) => Check::Fail(
            "nothing says the change is in stock: the MR is not merged, no stock commit \
             names it, and no RHEL issue shows a fix"
                .to_string(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    // ---- end-to-end wiremock + fake-koji test ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wiremock_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[serial_test::serial]
    fn untag_end_to_end_with_passing_preconditions() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // Tracking-issue lookup (no state filter).
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/groups/CentOS%2Fproposed_updates%2Frpms/issues",
                ))
                .and(query_param("labels", "cpu-sig-tracker,c10s"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {
                        "iid": 5,
                        "title": "xz untag test",
                        "description": "- **JIRA**: [RHEL-9](https://example/) — Closed (Done)",
                        "state": "closed",
                        "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/5",
                        "assignees": [],
                    }
                ])))
                .mount(&server)
                .await;

            // JIRA lookup — fixed: resolved Done, with a fix version
            // (a Done without one is not taken as a fix, #87).
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-9"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-9",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "Closed" },
                        "resolution": { "name": "Done" },
                        "fixVersions": [{ "name": "CentOS Stream 10" }]
                    }
                })))
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        // Stream's own xz, as fedrq answers it, so the run never asks the
        // real fedrq, which would fetch CentOS Stream's repositories.
        install_fake_bin(
            dir.path(),
            "fedrq",
            &[("pkgs --src", "xz : 5.6.2 : 5.el10")],
        );
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                // The CBS login check (#90).
                ("moshimoshi", "olá, tester!"),
                // NVR currently tagged in both -release and -testing.
                (
                    "list-tagged --quiet -- proposed_updates10s-packages-main-release",
                    "xz-5.6.4-1~proposed.el10\n",
                ),
                (
                    "list-tagged --quiet -- proposed_updates10s-packages-main-testing",
                    "xz-5.6.4-1~proposed.el10\n",
                ),
                // untag-build succeeds silently.
                (
                    "untag-build -- proposed_updates10s-packages-main-release",
                    "",
                ),
                (
                    "untag-build -- proposed_updates10s-packages-main-testing",
                    "",
                ),
            ],
        );
        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{existing_path}", dir.path().display());
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            // The developer's own config must not leak into a test.
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
            ("PATH", &new_path),
        ]);

        let args = UntagArgs {
            target: "xz".to_string(),
            release: "c10s".to_string(),
            yes: true,
            force: false,
            verbose: false,
            dry_run: false,
            explain: false,
        };
        run_inner(&args).expect("untag succeeds");
    }

    #[test]
    fn the_commands_shown_are_the_ones_run() {
        // sandogasa_koji::untag_build runs exactly this argv.
        assert_eq!(
            koji_argv(&["untag-build", "--", "t", "x-1-1.el10"]),
            [
                "koji",
                "--profile",
                "cbs",
                "untag-build",
                "--",
                "t",
                "x-1-1.el10"
            ]
        );
    }
}
