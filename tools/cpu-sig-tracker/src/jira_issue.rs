// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `jira` subcommand: find or file the RHEL Jira issue a Proposed
//! Update's merge request names, one per branch.
//!
//! CentOS Stream's merge request template asks for an approved Jira
//! ticket, and Red Hat wants one issue per branch, so each MR has its
//! own. This finds an open RHEL issue for the package and branch (and
//! CVE, when there is one) and, failing that, drafts one: the affected
//! versions Red Hat expects for the branch, a priority from the CVE's
//! severity, and a description. Red Hat's Jira cannot be edited once an
//! issue is filed, only commented on, so the draft is shown exactly as
//! it would be filed, and filing needs `--file` and a confirmation.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use sandogasa_cve::severity::{Level, Rating};
use sandogasa_jira::{Issue, NewIssue};

use crate::file_issue::IssueType;

/// The Jira project Proposed Updates file in.
const PROJECT: &str = "RHEL";

#[derive(clap::Args)]
pub struct JiraArgs {
    /// A CentOS Stream merge request URL (the package, branch and any
    /// CVE are read from it), or a package name with --branch
    pub target: String,

    /// CentOS Stream branch: c8s, c9s or c10s (default: the MR's
    /// target branch)
    #[arg(long, value_name = "BRANCH", help_heading = "Selection")]
    pub branch: Option<String>,

    /// CVE the update fixes (default: one named in the MR)
    #[arg(long, value_name = "CVE-ID", help_heading = "Issue")]
    pub cve: Option<String>,

    /// Text for the description, before the CVE details and links
    #[arg(long, value_name = "TEXT", help_heading = "Issue")]
    pub description: Option<String>,

    /// Read the description text from a file
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with = "description",
        help_heading = "Issue"
    )]
    pub description_file: Option<PathBuf>,

    /// Priority (default: from the CVE's severity)
    #[arg(
        long,
        value_name = "LEVEL",
        value_parser = ["Critical", "Major", "Normal", "Minor"],
        help_heading = "Issue"
    )]
    pub priority: Option<String>,

    /// Issue summary (default: the MR's title)
    #[arg(long, value_name = "TEXT", help_heading = "Issue")]
    pub summary: Option<String>,

    /// Kind of update; an enhancement's summary starts "RFE: "
    #[arg(long = "type", value_name = "TYPE", value_enum, help_heading = "Issue")]
    pub issue_type: Option<IssueType>,

    /// File the drafted issue (asked first at a terminal; with --yes,
    /// without asking)
    #[arg(long, help_heading = "Writing")]
    pub file: bool,

    /// Don't ask: keep explicit values, file only with --file
    #[arg(short = 'y', long, help_heading = "Writing")]
    pub yes: bool,

    /// Print what was found, drafted and filed as JSON
    #[arg(long, help_heading = "Output")]
    pub json: bool,

    /// Name each request on stderr
    #[arg(short, long, help_heading = "Output")]
    pub verbose: bool,
}

/// The Jira priority for a severity: one step per level, Blocker left
/// to Red Hat. Red Hat's `Important` is `High`, so it maps to Major.
pub fn priority_for(level: Level) -> &'static str {
    match level {
        Level::Critical => "Critical",
        Level::High => "Major",
        Level::Medium => "Normal",
        Level::Low => "Minor",
    }
}

/// The RHEL major a CentOS Stream branch builds for: `c9s` is 9.
pub fn branch_major(branch: &str) -> Result<u32, String> {
    branch
        .strip_prefix('c')
        .and_then(|b| b.strip_suffix('s'))
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("not a CentOS Stream branch: {branch} (expected c8s, c9s, c10s)"))
}

/// The affected version an issue for `branch` names, and is searched
/// by: the branch's CentOS Stream version, or `rhel-8.10` for c8s,
/// whose last minor stands in for it. One, because RHEL takes one: since
/// 2026-10 its "single-value policy" keeps only the first affected
/// version an issue lists, and Red Hat sets the RHEL versions itself.
pub fn branch_version(branch: &str) -> Result<String, String> {
    Ok(match branch_major(branch)? {
        8 => "rhel-8.10".to_string(),
        n => format!("CentOS Stream {n}"),
    })
}

/// The JQL that finds an open issue for `package` on `branch`, about
/// `cve` when there is one.
pub fn search_jql(package: &str, branch: &str, cve: Option<&str>) -> Result<String, String> {
    let mut jql = format!(
        "project = {PROJECT} AND component = \"{package}\" AND affectedVersion = \"{}\" \
         AND statusCategory != Done",
        branch_version(branch)?
    );
    if let Some(cve) = cve {
        jql.push_str(&format!(" AND text ~ \"{cve}\""));
    }
    jql.push_str(" ORDER BY created DESC");
    Ok(jql)
}

/// The first CVE id in `text`, upper-cased.
pub fn extract_cve(text: &str) -> Option<String> {
    crate::sync_updates::cve_ids(text).into_iter().next()
}

/// The issue's description: the submitter's own text first, then the
/// CVE's severity by every source that rated it, its description and
/// NVD link, then the merge request.
pub fn compose_description(
    text: Option<&str>,
    cve: Option<&str>,
    ratings: &[Rating],
    cve_description: Option<&str>,
    mr_url: Option<&str>,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(t.to_string());
    }
    if let Some(cve) = cve {
        let mut block = Vec::new();
        if let Some((first, rest)) = ratings.split_first() {
            block.push(format!("Severity: {} ({first})", first.level.as_str()));
            if !rest.is_empty() {
                let others: Vec<String> = rest.iter().map(Rating::to_string).collect();
                block.push(format!("Also rated: {}", others.join("; ")));
            }
        }
        if let Some(d) = cve_description {
            block.push(String::new());
            block.push(d.trim().to_string());
        }
        block.push(String::new());
        block.push(format!("https://nvd.nist.gov/vuln/detail/{cve}"));
        parts.push(block.join("\n"));
    }
    if let Some(url) = mr_url {
        parts.push(format!("Merge request: {url}"));
    }
    parts.join("\n\n")
}

/// The draft exactly as it would be filed, field by field.
pub fn render_draft(issue: &NewIssue, priority_note: Option<&str>) -> String {
    let mut o = String::new();
    o.push_str(&format!("  project:     {}\n", issue.project));
    o.push_str(&format!("  type:        {}\n", issue.issue_type));
    o.push_str(&format!("  summary:     {}\n", issue.summary));
    o.push_str(&format!("  component:   {}\n", issue.components.join(", ")));
    o.push_str(&format!("  versions:    {}\n", issue.versions.join(", ")));
    match (&issue.priority, priority_note) {
        (Some(p), Some(n)) => o.push_str(&format!("  priority:    {p} ({n})\n")),
        (Some(p), None) => o.push_str(&format!("  priority:    {p}\n")),
        (None, _) => o.push_str("  priority:    (Jira's default)\n"),
    }
    o.push_str("  description:\n");
    for line in issue.description.lines() {
        if line.is_empty() {
            o.push('\n');
        } else {
            o.push_str(&format!("    {line}\n"));
        }
    }
    o
}

pub fn run(args: &JiraArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Where the update is: the package, the branch, the CVE, and the MR
/// when one was given.
struct Target {
    package: String,
    branch: String,
    cve: Option<String>,
    mr_url: Option<String>,
    mr_title: Option<String>,
}

fn resolve_target(args: &JiraArgs) -> Result<Target, Box<dyn std::error::Error>> {
    if !args.target.contains("://") {
        let branch = args
            .branch
            .clone()
            .ok_or("a package name needs --branch (c8s, c9s, c10s)")?;
        return Ok(Target {
            package: args.target.clone(),
            branch,
            cve: args.cve.as_deref().map(str::to_ascii_uppercase),
            mr_url: None,
            mr_title: None,
        });
    }
    let (_, project, iid) = crate::gitlab::parse_mr_url(&args.target)?;
    let package = crate::file_issue::package_from_project(&project)
        .ok_or_else(|| format!("could not read a package name from '{project}'"))?
        .to_string();
    if args.verbose {
        eprintln!("[cpu-sig-tracker] reading MR {iid} from {project}");
    }
    let mr = crate::gitlab::reader(&crate::utils::gitlab_base(), &project)?.merge_request(iid)?;
    let cve = args
        .cve
        .as_deref()
        .map(str::to_ascii_uppercase)
        .or_else(|| extract_cve(&mr.title))
        .or_else(|| mr.description.as_deref().and_then(extract_cve));
    Ok(Target {
        package,
        branch: args.branch.clone().unwrap_or(mr.target_branch),
        cve,
        mr_url: Some(mr.web_url),
        mr_title: Some(mr.title),
    })
}

/// The priority to file with, and why, following the workspace rule:
/// the severity fills it when no flag does; a flag that disagrees is
/// confirmed at a terminal, defaulting to the severity's, and kept with
/// a note otherwise.
fn settle_priority(
    flag: Option<&str>,
    ratings: &[Rating],
    cve: Option<&str>,
    ask: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let derived = ratings.first().map(|r| {
        (
            priority_for(r.level),
            format!("{} rates {} {}", r.source, cve.unwrap_or("it"), r.detail),
        )
    });
    match (flag, derived) {
        (None, Some((p, why))) => Ok((Some(p.to_string()), Some(why))),
        (None, None) => Ok((None, None)),
        (Some(f), None) => Ok((Some(f.to_string()), Some("as given".to_string()))),
        (Some(f), Some((p, _))) if f == p => {
            Ok((Some(f.to_string()), Some("as given".to_string())))
        }
        (Some(f), Some((p, why))) => {
            let mismatch = format!("{why}, which maps to {p}");
            if ask {
                eprintln!("\n{mismatch}.");
                let keep = sandogasa_cli::confirm(&format!("File as {f} anyway?"), false)
                    .map_err(|e| e.to_string())?;
                if keep {
                    Ok((Some(f.to_string()), Some(format!("as given; {mismatch}"))))
                } else {
                    Ok((Some(p.to_string()), Some(why)))
                }
            } else {
                eprintln!("note: {mismatch}; filing as {f}, as given");
                Ok((Some(f.to_string()), Some(format!("as given; {mismatch}"))))
            }
        }
    }
}

fn run_inner(args: &JiraArgs) -> Result<(), Box<dyn std::error::Error>> {
    let target = resolve_target(args)?;
    let ask = !args.yes && !args.json && std::io::stdin().is_terminal();
    let runtime = crate::jira::runtime()?;
    let client = runtime.block_on(crate::jira::client())?;

    // An issue may exist already: Product Security files its own for
    // many CVEs, and a duplicate is closed as one.
    let jql = search_jql(&target.package, &target.branch, target.cve.as_deref())?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] searching {PROJECT}: {jql}");
    }
    let found: Vec<Issue> = runtime.block_on(client.search(
        &jql,
        &["summary", "status", "versions", "priority"],
        20,
    ))?;

    if !found.is_empty() {
        return report_found(args, &target, &found);
    }

    // Nothing open: draft one.
    let affected = vec![branch_version(&target.branch)?];

    let (ratings, cve_description) = match &target.cve {
        Some(cve) => runtime.block_on(cve_facts(cve, args.verbose)),
        None => (Vec::new(), None),
    };
    let (priority, priority_note) = settle_priority(
        args.priority.as_deref(),
        &ratings,
        target.cve.as_deref(),
        ask,
    )?;

    let text = match &args.description_file {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read --description-file {}: {e}", p.display()))?,
        ),
        None => args.description.clone(),
    };
    let summary = summary_for(args, &target)?;

    let mut issue = NewIssue::new(PROJECT, "Bug", &summary);
    issue.components = vec![target.package.clone()];
    issue.versions = affected;
    issue.priority = priority;
    issue.description = compose_description(
        text.as_deref(),
        target.cve.as_deref(),
        &ratings,
        cve_description.as_deref(),
        target.mr_url.as_deref(),
    );

    let checks = runtime.block_on(check_against_metadata(&client, &issue, args.verbose));

    let filed = if args.file {
        if !args.json {
            println!("Draft for {} on {}:\n", target.package, target.branch);
            print!("{}", render_draft(&issue, priority_note.as_deref()));
            for c in &checks {
                println!("  ! {c}");
            }
        }
        let go = if args.yes {
            true
        } else if ask {
            sandogasa_cli::confirm(
                "\nFile this issue? It cannot be edited afterwards, only commented on",
                false,
            )?
        } else {
            return Err("filing needs a terminal to confirm at, or --yes with --file".into());
        };
        if go {
            let created = runtime.block_on(client.create_issue(&issue))?;
            Some(created.key)
        } else {
            None
        }
    } else {
        None
    };

    if args.json {
        let out = serde_json::json!({
            "package": target.package,
            "branch": target.branch,
            "cve": target.cve,
            "found": [],
            "draft": issue.to_json()["fields"],
            "priority_from": priority_note,
            "checks": checks,
            "filed": filed,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    match filed {
        Some(key) => {
            println!("\nFiled {}", issue_url(&key));
            println!("For the merge request's description:\n  Resolves: {key}");
        }
        None if args.file => println!("\nNot filed."),
        None => {
            println!(
                "No open {PROJECT} issue for {} on {}{}. Draft:\n",
                target.package,
                target.branch,
                target
                    .cve
                    .as_deref()
                    .map(|c| format!(" about {c}"))
                    .unwrap_or_default()
            );
            print!("{}", render_draft(&issue, priority_note.as_deref()));
            for c in &checks {
                println!("  ! {c}");
            }
            println!("\nNot filed: pass --file to file it.");
        }
    }
    Ok(())
}

/// The summary: `--summary`, or the MR's title with the CVE in front
/// when the title does not name it, and `RFE: ` in front for an
/// enhancement, as Red Hat's RFEs read. An explicit summary is never
/// rewritten; one for an enhancement without the prefix gets a note.
/// Without either there is nothing to call the issue.
fn summary_for(args: &JiraArgs, target: &Target) -> Result<String, String> {
    let rfe = matches!(args.issue_type, Some(IssueType::Enhancement));
    let has_rfe = |s: &str| s.to_ascii_uppercase().starts_with("RFE:");
    if let Some(s) = &args.summary {
        if rfe && !has_rfe(s) {
            eprintln!("note: Red Hat's enhancement requests start \"RFE: \"; --summary does not");
        }
        return Ok(s.clone());
    }
    let title = target
        .mr_title
        .clone()
        .ok_or("a package name needs --summary: there is no MR title to use")?;
    Ok(derived_summary(&title, target.cve.as_deref(), rfe))
}

/// A summary made from an MR title: the CVE in front when the title
/// does not name it, `RFE: ` for an enhancement.
pub fn derived_summary(title: &str, cve: Option<&str>, rfe: bool) -> String {
    let mut s = match cve {
        Some(cve) if !title.to_ascii_uppercase().contains(cve) => format!("{cve}: {title}"),
        _ => title.to_string(),
    };
    if rfe && !s.to_ascii_uppercase().starts_with("RFE:") {
        s = format!("RFE: {s}");
    }
    s
}

/// Every severity rating of `cve`, and NVD's description of it.
async fn cve_facts(cve: &str, verbose: bool) -> (Vec<Rating>, Option<String>) {
    if verbose {
        eprintln!("[cpu-sig-tracker] rating {cve}: Red Hat, NVD, GitHub");
    }
    let http = match sandogasa_cli::http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()
    {
        Ok(h) => h,
        Err(e) => {
            eprintln!("warning: no HTTP client for the severity lookup ({e})");
            return (Vec::new(), None);
        }
    };
    let cache = sandogasa_cli::cache::DiskCache::new(
        "cpu-sig-tracker",
        "advisories",
        Some(sandogasa_cve::cache::NVD_TTL),
        false,
    );
    let token = std::env::var("GITHUB_TOKEN").ok();
    let sources = sandogasa_cve::sources::Sources::new(&http, &cache, token.as_deref());
    let mut nvd = sandogasa_cve::NvdCache::new("cpu-sig-tracker", String::new(), verbose, false);
    let ratings = sandogasa_cve::severity::ratings(cve, &mut nvd, &sources).await;
    let description = nvd
        .get(cve, |r| r)
        .await
        .and_then(|r| r.english_description().map(str::to_string));
    (ratings, description)
}

/// What the create metadata says is wrong with the draft: a required
/// field it does not set, a version the project does not offer. Empty
/// when nothing is, or when the metadata cannot be read (it needs Jira
/// credentials), which is said instead.
async fn check_against_metadata(
    client: &sandogasa_jira::JiraClient,
    issue: &NewIssue,
    verbose: bool,
) -> Vec<String> {
    if verbose {
        eprintln!("[cpu-sig-tracker] reading {PROJECT}'s create metadata for Bug");
    }
    let fields = match client.create_metadata(PROJECT, &issue.issue_type).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return vec![format!(
                "{PROJECT} offers no {} issue type",
                issue.issue_type
            )];
        }
        Err(_) => {
            return vec![
                "not checked against Jira's required fields (needs Jira credentials: \
                 cpu-sig-tracker config)"
                    .to_string(),
            ];
        }
    };
    let set = [
        "project",
        "issuetype",
        "summary",
        "description",
        "components",
        "versions",
        "priority",
        "reporter",
    ];
    let mut out: Vec<String> = fields
        .iter()
        .filter(|f| f.required && !f.has_default_value && !set.contains(&f.field_id.as_str()))
        .map(|f| format!("Jira also requires {} ({})", f.name, f.field_id))
        .collect();
    if let Some(v) = fields.iter().find(|f| f.field_id == "versions")
        && !v.allowed_values.is_empty()
    {
        out.extend(
            issue
                .versions
                .iter()
                .filter(|name| !v.allowed_values.contains(name))
                .map(|name| format!("{PROJECT} does not offer the version {name}")),
        );
    }
    out
}

/// Where a found or filed issue is browsed.
fn issue_url(key: &str) -> String {
    format!("{}/browse/{key}", crate::utils::jira_base())
}

/// Report open issues found for the update, and the line its MR needs.
fn report_found(
    args: &JiraArgs,
    target: &Target,
    found: &[Issue],
) -> Result<(), Box<dyn std::error::Error>> {
    if args.json {
        let issues: Vec<_> = found
            .iter()
            .map(|i| {
                serde_json::json!({
                    "key": i.key,
                    "status": i.status(),
                    "summary": i.summary(),
                    "url": issue_url(&i.key),
                })
            })
            .collect();
        let out = serde_json::json!({
            "package": target.package,
            "branch": target.branch,
            "cve": target.cve,
            "found": issues,
            "draft": null,
            "filed": null,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    println!(
        "Open {PROJECT} issue(s) for {} on {}{}:",
        target.package,
        target.branch,
        target
            .cve
            .as_deref()
            .map(|c| format!(" about {c}"))
            .unwrap_or_default()
    );
    for i in found {
        println!("  {} [{}] {}", issue_url(&i.key), i.status(), i.summary());
    }
    println!(
        "Nothing filed. For the merge request's description:\n  Resolves: {}",
        found[0].key
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_names_one_affected_version() {
        assert_eq!(branch_version("c9s").unwrap(), "CentOS Stream 9");
        assert_eq!(branch_version("c10s").unwrap(), "CentOS Stream 10");
        // c8s is past its last minor, which stands in for it.
        assert_eq!(branch_version("c8s").unwrap(), "rhel-8.10");
        assert!(branch_version("f45").is_err());
    }

    #[test]
    fn priority_steps_down_one_level_per_severity() {
        assert_eq!(priority_for(Level::Critical), "Critical");
        assert_eq!(priority_for(Level::High), "Major");
        assert_eq!(priority_for(Level::Medium), "Normal");
        assert_eq!(priority_for(Level::Low), "Minor");
    }

    #[test]
    fn the_search_is_by_package_branch_and_cve() {
        assert_eq!(
            search_jql("freetype", "c9s", Some("CVE-2025-27363")).unwrap(),
            "project = RHEL AND component = \"freetype\" AND affectedVersion = \
             \"CentOS Stream 9\" AND statusCategory != Done AND text ~ \"CVE-2025-27363\" \
             ORDER BY created DESC"
        );
        assert!(
            search_jql("freetype", "c8s", None)
                .unwrap()
                .contains("affectedVersion = \"rhel-8.10\"")
        );
    }

    #[test]
    fn a_cve_is_read_out_of_an_mr() {
        assert_eq!(
            extract_cve("OOB write fixes for CVE-2025-27363").as_deref(),
            Some("CVE-2025-27363")
        );
        assert_eq!(
            extract_cve("see cve-2026-114583.").as_deref(),
            Some("CVE-2026-114583")
        );
        assert_eq!(extract_cve("CVE-26-1 and none"), None);
        assert_eq!(extract_cve("no id here"), None);
    }

    #[test]
    fn the_description_carries_every_rating_and_the_links() {
        let rating = |level, source: &str, detail: &str| Rating {
            level,
            source: source.to_string(),
            detail: detail.to_string(),
        };
        let ratings = [
            rating(Level::High, "Red Hat", "Important (CVSS 3: 8.1)"),
            rating(Level::High, "NVD", "CVSS 3.1 8.1 high (nvd@nist.gov)"),
        ];
        let d = compose_description(
            Some("Backport of the upstream fix, tested against the reproducer."),
            Some("CVE-2025-27363"),
            &ratings,
            Some("An out of bounds write exists in FreeType versions 2.13.0 and below."),
            Some("https://gitlab.com/redhat/centos-stream/rpms/freetype/-/merge_requests/8"),
        );
        assert_eq!(
            d,
            "Backport of the upstream fix, tested against the reproducer.\n\n\
             Severity: high (Red Hat: Important (CVSS 3: 8.1))\n\
             Also rated: NVD: CVSS 3.1 8.1 high (nvd@nist.gov)\n\n\
             An out of bounds write exists in FreeType versions 2.13.0 and below.\n\n\
             https://nvd.nist.gov/vuln/detail/CVE-2025-27363\n\n\
             Merge request: https://gitlab.com/redhat/centos-stream/rpms/freetype/-/merge_requests/8"
        );
        // A non-CVE update is only the text and the MR.
        assert_eq!(
            compose_description(Some("RFE: ship pcilmr"), None, &[], None, None),
            "RFE: ship pcilmr"
        );
    }

    #[test]
    fn a_disagreeing_priority_is_kept_with_a_note_when_nobody_can_be_asked() {
        let ratings = [Rating {
            level: Level::High,
            source: "Red Hat".to_string(),
            detail: "Important".to_string(),
        }];
        // No flag: the severity's.
        let (p, why) = settle_priority(None, &ratings, Some("CVE-1"), false).unwrap();
        assert_eq!(p.as_deref(), Some("Major"));
        assert_eq!(why.as_deref(), Some("Red Hat rates CVE-1 Important"));
        // A flag that agrees, or with nothing to compare against.
        assert_eq!(
            settle_priority(Some("Major"), &ratings, Some("CVE-1"), false)
                .unwrap()
                .0
                .as_deref(),
            Some("Major")
        );
        assert_eq!(
            settle_priority(Some("Minor"), &[], None, false)
                .unwrap()
                .0
                .as_deref(),
            Some("Minor")
        );
        // A flag that disagrees, unattended: kept, with why it differs.
        let (p, why) = settle_priority(Some("Critical"), &ratings, Some("CVE-1"), false).unwrap();
        assert_eq!(p.as_deref(), Some("Critical"));
        assert!(why.unwrap().contains("which maps to Major"));
        // Nothing to go on: Jira's default.
        assert_eq!(
            settle_priority(None, &[], None, false).unwrap(),
            (None, None)
        );
    }

    #[test]
    fn a_summary_from_the_mr_names_the_cve_and_marks_an_rfe() {
        assert_eq!(
            derived_summary(
                "OOB write fixes for CVE-2025-27363",
                Some("CVE-2025-27363"),
                false
            ),
            "OOB write fixes for CVE-2025-27363"
        );
        assert_eq!(
            derived_summary("OOB write fixes", Some("CVE-2025-27363"), false),
            "CVE-2025-27363: OOB write fixes"
        );
        assert_eq!(
            derived_summary("ship pcilmr", None, true),
            "RFE: ship pcilmr"
        );
        assert_eq!(
            derived_summary("RFE: ship pcilmr", None, true),
            "RFE: ship pcilmr"
        );
    }

    #[test]
    fn the_draft_shows_every_field_as_filed() {
        let mut issue = NewIssue::new("RHEL", "Bug", "CVE-2025-27363: OOB write fixes");
        issue.components = vec!["freetype".into()];
        issue.versions = vec!["CentOS Stream 9".into()];
        issue.priority = Some("Major".into());
        issue.description = "line one\n\nline two".into();
        let shown = render_draft(&issue, Some("Red Hat rates CVE-2025-27363 Important"));
        assert!(
            shown.contains("summary:     CVE-2025-27363: OOB write fixes"),
            "{shown}"
        );
        assert!(shown.contains("versions:    CentOS Stream 9\n"), "{shown}");
        assert!(
            shown.contains("priority:    Major (Red Hat rates CVE-2025-27363 Important)"),
            "{shown}"
        );
        assert!(shown.contains("    line one\n\n    line two\n"), "{shown}");
    }
}
