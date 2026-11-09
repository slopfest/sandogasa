// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `jira` subcommand: find or file the RHEL Jira issue a Proposed
//! Update's merge request names, one per branch.
//!
//! CentOS Stream's merge request template asks for an approved Jira
//! ticket, named in the description, and Red Hat wants one issue per
//! branch, so the issue comes before the MR: the usual target is a
//! package and branch, and an MR URL serves one already open. This finds the open RHEL issues for the package (and CVE, when
//! there is one), sorts out those for the branch's RHEL major, and,
//! when there are none, says what that can mean (Product Security's
//! trackers are restricted, so Red Hat's security data is asked whether
//! one is likely) and drafts one: the affected version Red Hat expects
//! for the branch, a priority from the CVE's severity, and a
//! description. Red Hat's Jira cannot be edited once an
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
    /// A package name, with --branch; or the URL of a CentOS Stream
    /// merge request already open (package, branch, CVE read from it)
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

    /// Issue summary (default: the MR's title, else the CVE's)
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

/// The JQL that finds open issues for `package`, about `cve` when there
/// is one, on any release: Product Security's trackers name RHEL
/// versions (`rhel-9.6`), not CentOS Stream's, so the release is sorted
/// out afterwards by [`for_major`] rather than filtered on here.
pub fn search_jql(package: &str, cve: Option<&str>) -> String {
    let mut jql =
        format!("project = {PROJECT} AND component = \"{package}\" AND statusCategory != Done");
    if let Some(cve) = cve {
        jql.push_str(&format!(" AND text ~ \"{cve}\""));
    }
    jql.push_str(" ORDER BY created DESC");
    jql
}

/// The RHEL major an affected version is for: `CentOS Stream 9`,
/// `rhel-9.6` and `rhel-9.6.z` are all 9.
pub fn version_major(name: &str) -> Option<u32> {
    let rest = name
        .strip_prefix("CentOS Stream ")
        .or_else(|| name.strip_prefix("rhel-"))?;
    rest.split('.').next()?.parse().ok()
}

/// Whether `issue` names an affected version of RHEL `major`, CentOS
/// Stream's or a minor's.
pub fn for_major(issue: &Issue, major: u32) -> bool {
    issue
        .fields
        .versions
        .iter()
        .any(|v| version_major(&v.name) == Some(major))
}

/// Where Red Hat's security data puts one package in one RHEL major.
#[derive(Debug, PartialEq)]
pub enum RhelState {
    /// Fixed, by this advisory.
    Fixed(String),
    /// Not fixed yet, as Red Hat words it: `Affected`, `Fix deferred`,
    /// `Will not fix`, `Not affected`, `Out of support scope`, …
    Listed(String),
}

impl RhelState {
    /// Whether Red Hat means to fix it, and so has probably filed a
    /// tracker for it.
    pub fn pending(&self) -> bool {
        matches!(self, RhelState::Listed(s)
            if matches!(s.as_str(), "Affected" | "Fix deferred" | "Under investigation"))
    }
}

/// Where Red Hat's security data (`json`, from
/// [`sandogasa_cve::severity::redhat_url`]) puts `package` in RHEL
/// `major`: fixed by an advisory for the major itself (not an EUS or
/// other minor stream), else its `package_state`. `None` when Red Hat
/// says nothing about the package there.
pub fn rhel_state(json: &str, package: &str, major: u32) -> Option<RhelState> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let product = format!("Red Hat Enterprise Linux {major}");
    let is_build_of = |nevr: &str| {
        nevr.strip_prefix(package)
            .and_then(|r| r.strip_prefix('-'))
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    };
    let fixed = v["affected_release"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|a| {
            a["product_name"].as_str() == Some(product.as_str())
                && a["package"].as_str().is_some_and(is_build_of)
        });
    if let Some(a) = fixed {
        let advisory = a["advisory"].as_str().unwrap_or("an advisory");
        return Some(RhelState::Fixed(advisory.to_string()));
    }
    v["package_state"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|p| {
            p["product_name"].as_str() == Some(product.as_str())
                && p["package_name"].as_str() == Some(package)
        })
        .and_then(|p| p["fix_state"].as_str())
        .map(|s| RhelState::Listed(s.to_string()))
}

/// What an empty search can and cannot mean. Product Security's CVE
/// trackers are restricted, hidden from anonymous searches and from
/// ordinary community accounts alike, so finding nothing does not mean
/// nothing was filed; Red Hat's security data says whether a tracker is
/// likely.
pub fn empty_search_notes(
    package: &str,
    cve: Option<&str>,
    major: u32,
    state: Option<&RhelState>,
    logged_in: bool,
) -> Vec<String> {
    let mut notes = Vec::new();
    if !logged_in {
        notes.push(
            "Searched anonymously: configure Jira credentials (cpu-sig-tracker config) \
             to see issues only logged-in accounts can"
                .to_string(),
        );
    }
    let Some(cve) = cve else {
        return notes;
    };
    notes.push(match state {
        Some(s @ RhelState::Listed(word)) if s.pending() => format!(
            "Red Hat lists RHEL {major} as {word} by {cve}, so Product Security has \
             probably filed a tracker, restricted and so unlikely to show here. Filing \
             may duplicate it, which Red Hat will link; name {cve} in the merge request \
             so the two can be matched"
        ),
        Some(RhelState::Listed(word)) => {
            format!("Red Hat lists RHEL {major} as {word} for {cve}")
        }
        Some(RhelState::Fixed(advisory)) => format!(
            "Red Hat fixed {cve} in RHEL {major} with {advisory}: check whether \
             CentOS Stream already has the fix"
        ),
        None => format!("Red Hat's security data says nothing of {package} in RHEL {major}"),
    });
    notes
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
    let major = branch_major(&target.branch)?;
    let logged_in = crate::jira::logged_in();
    let jql = search_jql(&target.package, target.cve.as_deref());
    if args.verbose {
        let who = if logged_in {
            "logged in"
        } else {
            "anonymously"
        };
        eprintln!("[cpu-sig-tracker] searching {PROJECT} {who}: {jql}");
    }
    let found: Vec<Issue> = runtime.block_on(client.search(
        &jql,
        &["summary", "status", "versions", "priority"],
        20,
    ))?;
    let (here, elsewhere): (Vec<Issue>, Vec<Issue>) =
        found.into_iter().partition(|i| for_major(i, major));

    if !here.is_empty() {
        return report_found(args, &target, &here, &elsewhere);
    }

    // Nothing open for this release: draft one.
    let affected = vec![branch_version(&target.branch)?];

    let facts = match &target.cve {
        Some(cve) => runtime.block_on(cve_facts(cve, args.verbose)),
        None => CveFacts::default(),
    };
    let (ratings, cve_description) = (facts.ratings, facts.description);
    let cve_title = target.cve.as_deref().and_then(|cve| {
        cve_title(
            &target.package,
            cve,
            facts.redhat.as_deref(),
            facts.ghsa.as_deref(),
        )
    });
    let state = facts
        .redhat
        .as_deref()
        .and_then(|j| rhel_state(j, &target.package, major));
    let notes = empty_search_notes(
        &target.package,
        target.cve.as_deref(),
        major,
        state.as_ref(),
        logged_in,
    );
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
    let Some(summary) = summary_for(args, &target, cve_title.as_deref()) else {
        // Nothing to call an issue by: the search is the whole answer.
        if args.file {
            return Err("a package name without a CVE needs --summary to file an issue".into());
        }
        if args.json {
            let out = serde_json::json!({
                "package": target.package,
                "branch": target.branch,
                "cve": target.cve,
                "logged_in": logged_in,
                "found": [],
                "other_releases": elsewhere.iter().map(issue_json).collect::<Vec<_>>(),
                "notes": notes,
                "draft": null,
                "filed": null,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            print!(
                "{}",
                render_empty_search(&target, logged_in, &elsewhere, &notes)
            );
            println!("No draft: pass --summary (or --cve) to draft an issue.");
        }
        return Ok(());
    };

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

    if !args.json {
        print!(
            "{}",
            render_empty_search(&target, logged_in, &elsewhere, &notes)
        );
    }

    let filed = if args.file {
        if !args.json {
            println!("Draft:\n");
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
            "logged_in": logged_in,
            "found": [],
            "other_releases": elsewhere.iter().map(issue_json).collect::<Vec<_>>(),
            "redhat_state": state.as_ref().map(|s| match s {
                RhelState::Fixed(a) => format!("Fixed ({a})"),
                RhelState::Listed(w) => w.clone(),
            }),
            "notes": notes,
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
            println!("Draft:\n");
            print!("{}", render_draft(&issue, priority_note.as_deref()));
            for c in &checks {
                println!("  ! {c}");
            }
            println!("\nNot filed: pass --file to file it.");
        }
    }
    Ok(())
}

/// The summary: `--summary`, else the MR's title with the CVE in front
/// when the title does not name it, else the CVE's own title
/// ([`cve_title`]), with `RFE: ` in front for an enhancement, as Red
/// Hat's RFEs read. An explicit summary is never rewritten; one for an
/// enhancement without the prefix gets a note. `None` when there is
/// nothing to call the issue.
fn summary_for(args: &JiraArgs, target: &Target, cve_title: Option<&str>) -> Option<String> {
    let rfe = matches!(args.issue_type, Some(IssueType::Enhancement));
    let has_rfe = |s: &str| s.to_ascii_uppercase().starts_with("RFE:");
    if let Some(s) = &args.summary {
        if rfe && !has_rfe(s) {
            eprintln!("note: Red Hat's enhancement requests start \"RFE: \"; --summary does not");
        }
        return Some(s.clone());
    }
    let title = target.mr_title.as_deref().or(cve_title)?;
    Some(derived_summary(title, target.cve.as_deref(), rfe))
}

/// A CVE's title for an issue summary, as Red Hat's bug for it reads:
/// `<package>: <what>  (<CVE>)`. Red Hat's security data names the bug
/// (`bugzilla.description`), often with the package said twice
/// (`rsync: rsync: Denial of Service …`), which is collapsed; else the
/// summary of GitHub's first advisory for it. NVD has no title.
pub fn cve_title(
    package: &str,
    cve: &str,
    redhat: Option<&str>,
    ghsa: Option<&str>,
) -> Option<String> {
    let parse = |j: Option<&str>| j.and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok());
    let from_redhat =
        parse(redhat).and_then(|v| v["bugzilla"]["description"].as_str().map(str::to_string));
    let from_ghsa = || {
        parse(ghsa).and_then(|v| {
            v.as_array()?
                .iter()
                .find_map(|a| a["summary"].as_str().map(str::to_string))
        })
    };
    let raw = from_redhat.or_else(from_ghsa)?;
    let prefix = format!("{}: ", package.to_ascii_lowercase());
    let mut what = raw.trim();
    while what.to_ascii_lowercase().starts_with(&prefix) {
        what = what[prefix.len()..].trim_start();
    }
    if what.is_empty() {
        return None;
    }
    let mut title = format!("{package}: {what}");
    if !title.to_ascii_uppercase().contains(cve) {
        title.push_str(&format!(" ({cve})"));
    }
    Some(title)
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

/// What the sources say about a CVE.
#[derive(Default)]
struct CveFacts {
    /// Every severity rating, Red Hat's first.
    ratings: Vec<Rating>,
    /// NVD's description.
    description: Option<String>,
    /// Red Hat's security data, as JSON.
    redhat: Option<String>,
    /// GitHub's advisories for it, as JSON.
    ghsa: Option<String>,
}

/// Every severity rating of `cve`, NVD's description of it, and Red
/// Hat's security data on it.
async fn cve_facts(cve: &str, verbose: bool) -> CveFacts {
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
            return CveFacts::default();
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
    // Already fetched for the rating, so read from the cache.
    let redhat = sources.redhat_cve(cve).await;
    let ghsa = sources.ghsa_json(cve).await;
    CveFacts {
        ratings,
        description,
        redhat,
        ghsa,
    }
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

/// An issue's affected versions, comma-separated.
fn versions_of(issue: &Issue) -> String {
    let names: Vec<&str> = issue
        .fields
        .versions
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    if names.is_empty() {
        "no version".to_string()
    } else {
        names.join(", ")
    }
}

/// One found issue, as a line of the report.
fn issue_line(issue: &Issue) -> String {
    format!(
        "  {} [{}] {} ({})\n",
        issue_url(&issue.key),
        issue.status(),
        issue.summary(),
        versions_of(issue)
    )
}

/// One found issue, for `--json`.
fn issue_json(issue: &Issue) -> serde_json::Value {
    serde_json::json!({
        "key": issue.key,
        "status": issue.status(),
        "summary": issue.summary(),
        "versions": issue.fields.versions.iter().map(|v| &v.name).collect::<Vec<_>>(),
        "url": issue_url(&issue.key),
    })
}

/// " about CVE-…", or nothing.
fn about(cve: Option<&str>) -> String {
    cve.map(|c| format!(" about {c}")).unwrap_or_default()
}

/// What a search with no issue for the branch's release found: whom it
/// searched as, the issues open for other releases only, and what
/// finding nothing may mean.
fn render_empty_search(
    target: &Target,
    logged_in: bool,
    elsewhere: &[Issue],
    notes: &[String],
) -> String {
    let who = if logged_in {
        "to this account"
    } else {
        "anonymously"
    };
    let mut o = format!(
        "No open {PROJECT} issue for {} on {}{} is visible {who}.\n",
        target.package,
        target.branch,
        about(target.cve.as_deref())
    );
    if !elsewhere.is_empty() {
        o.push_str("Open for other releases only:\n");
        for i in elsewhere {
            o.push_str(&issue_line(i));
        }
    }
    for n in notes {
        o.push_str(&format!("note: {n}\n"));
    }
    o.push('\n');
    o
}

/// Report open issues found for the update's release, and the line its
/// MR needs; issues for other releases are listed after them.
fn report_found(
    args: &JiraArgs,
    target: &Target,
    here: &[Issue],
    elsewhere: &[Issue],
) -> Result<(), Box<dyn std::error::Error>> {
    if args.json {
        let out = serde_json::json!({
            "package": target.package,
            "branch": target.branch,
            "cve": target.cve,
            "found": here.iter().map(issue_json).collect::<Vec<_>>(),
            "other_releases": elsewhere.iter().map(issue_json).collect::<Vec<_>>(),
            "draft": null,
            "filed": null,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    print!("{}", render_found(target, here, elsewhere));
    Ok(())
}

/// The text of [`report_found`].
fn render_found(target: &Target, here: &[Issue], elsewhere: &[Issue]) -> String {
    let mut o = format!(
        "Open {PROJECT} issue(s) for {} on {}{}:\n",
        target.package,
        target.branch,
        about(target.cve.as_deref())
    );
    for i in here {
        o.push_str(&issue_line(i));
    }
    if !elsewhere.is_empty() {
        o.push_str("Also open, for other releases:\n");
        for i in elsewhere {
            o.push_str(&issue_line(i));
        }
    }
    o.push_str(&format!(
        "Nothing filed. For the merge request's description:\n  Resolves: {}\n",
        here[0].key
    ));
    o
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
    fn the_search_is_by_package_and_cve_on_any_release() {
        assert_eq!(
            search_jql("freetype", Some("CVE-2025-27363")),
            "project = RHEL AND component = \"freetype\" AND statusCategory != Done \
             AND text ~ \"CVE-2025-27363\" ORDER BY created DESC"
        );
        assert!(!search_jql("freetype", None).contains("affectedVersion"));
    }

    /// An issue as Jira's search returns it.
    fn issue(key: &str, versions: &[&str]) -> Issue {
        serde_json::from_value(serde_json::json!({
            "key": key,
            "fields": {
                "summary": format!("{key} summary"),
                "status": {"name": "New"},
                "versions": versions.iter().map(|v| serde_json::json!({"name": v}))
                    .collect::<Vec<_>>(),
            }
        }))
        .unwrap()
    }

    #[test]
    fn stream_and_minor_versions_belong_to_their_major() {
        assert_eq!(version_major("CentOS Stream 9"), Some(9));
        assert_eq!(version_major("CentOS Stream 10"), Some(10));
        assert_eq!(version_major("rhel-9.6"), Some(9));
        assert_eq!(version_major("rhel-10.1.z"), Some(10));
        assert_eq!(version_major("rhel-8.10"), Some(8));
        assert_eq!(version_major("something else"), None);
        // Product Security's tracker names a minor, and still counts.
        assert!(for_major(&issue("RHEL-1", &["rhel-9.6"]), 9));
        assert!(!for_major(&issue("RHEL-2", &["rhel-10.1"]), 9));
        assert!(!for_major(&issue("RHEL-3", &[]), 9));
    }

    /// Red Hat's security data, trimmed to what is read.
    const REDHAT: &str = r#"{
        "affected_release": [
            {"product_name": "Red Hat Enterprise Linux 8", "advisory": "RHSA-2025:8292",
             "package": "mingw-freetype-0:2.8-3.el8_10.1"},
            {"product_name": "Red Hat Enterprise Linux 8.6 Extended Update Support",
             "advisory": "RHSA-2025:3385", "package": "freetype-0:2.9.1-6.el8_6.3"},
            {"product_name": "Red Hat Enterprise Linux 9", "advisory": "RHSA-2025:3407",
             "package": "freetype-0:2.10.4-10.el9_5"}
        ],
        "package_state": [
            {"product_name": "Red Hat Enterprise Linux 10", "fix_state": "Affected",
             "package_name": "freetype"},
            {"product_name": "Red Hat Enterprise Linux 8", "fix_state": "Will not fix",
             "package_name": "freetype"},
            {"product_name": "Red Hat Enterprise Linux 9", "fix_state": "Not affected",
             "package_name": "java-21-openjdk"}
        ]
    }"#;

    #[test]
    fn red_hat_s_state_is_read_for_the_package_in_the_major() {
        assert_eq!(
            rhel_state(REDHAT, "freetype", 9),
            Some(RhelState::Fixed("RHSA-2025:3407".into()))
        );
        assert_eq!(
            rhel_state(REDHAT, "freetype", 10),
            Some(RhelState::Listed("Affected".into()))
        );
        // mingw-freetype and an EUS stream's fix are not the major's.
        assert_eq!(
            rhel_state(REDHAT, "freetype", 8),
            Some(RhelState::Listed("Will not fix".into()))
        );
        assert_eq!(rhel_state(REDHAT, "rsync", 9), None);
        assert_eq!(rhel_state("not json", "freetype", 9), None);
        assert!(RhelState::Listed("Affected".into()).pending());
        assert!(RhelState::Listed("Fix deferred".into()).pending());
        assert!(!RhelState::Listed("Will not fix".into()).pending());
        assert!(!RhelState::Fixed("RHSA-1".into()).pending());
    }

    #[test]
    fn an_empty_search_says_what_it_may_be_missing() {
        let affected = RhelState::Listed("Affected".into());
        let notes = empty_search_notes("rsync", Some("CVE-1"), 9, Some(&affected), false);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("Searched anonymously"), "{notes:?}");
        assert!(
            notes[1].starts_with("Red Hat lists RHEL 9 as Affected by CVE-1")
                && notes[1].contains("probably filed a tracker"),
            "{notes:?}"
        );
        // Logged in, without a CVE: nothing to add.
        assert!(empty_search_notes("rsync", None, 9, None, true).is_empty());
        // Other answers are stated as they are.
        let wont = RhelState::Listed("Will not fix".into());
        assert_eq!(
            empty_search_notes("rsync", Some("CVE-1"), 9, Some(&wont), true),
            ["Red Hat lists RHEL 9 as Will not fix for CVE-1"]
        );
        let fixed = RhelState::Fixed("RHSA-2025:3407".into());
        assert!(
            empty_search_notes("rsync", Some("CVE-1"), 9, Some(&fixed), true)[0]
                .contains("with RHSA-2025:3407")
        );
        assert_eq!(
            empty_search_notes("rsync", Some("CVE-1"), 9, None, true),
            ["Red Hat's security data says nothing of rsync in RHEL 9"]
        );
    }

    fn target(cve: Option<&str>) -> Target {
        Target {
            package: "rsync".into(),
            branch: "c9s".into(),
            cve: cve.map(str::to_string),
            mr_url: None,
            mr_title: None,
        }
    }

    #[test]
    fn issues_for_other_releases_are_shown_but_not_taken() {
        let here = [issue("RHEL-9", &["rhel-9.6"])];
        let elsewhere = [issue("RHEL-10", &["rhel-10.1", "CentOS Stream 10"])];
        let shown = render_found(&target(Some("CVE-1")), &here, &elsewhere);
        assert!(
            shown.starts_with("Open RHEL issue(s) for rsync on c9s about CVE-1:\n"),
            "{shown}"
        );
        assert!(
            shown.contains("Also open, for other releases:\n") && shown.contains("RHEL-10 ["),
            "{shown}"
        );
        assert!(shown.contains("(rhel-10.1, CentOS Stream 10)"), "{shown}");
        assert!(shown.ends_with("  Resolves: RHEL-9\n"), "{shown}");

        let empty =
            render_empty_search(&target(None), true, &elsewhere, &["something".to_string()]);
        assert!(
            empty.starts_with("No open RHEL issue for rsync on c9s is visible to this account.\n"),
            "{empty}"
        );
        assert!(empty.contains("Open for other releases only:\n"), "{empty}");
        assert!(empty.contains("note: something\n"), "{empty}");
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
    fn a_cve_names_the_issue_as_red_hat_s_bug_does() {
        let redhat = r#"{"bugzilla": {"description":
            "rsync: rsync: Denial of Service via crafted checksums during delta transfer"}}"#;
        assert_eq!(
            cve_title("rsync", "CVE-2026-119741", Some(redhat), None).as_deref(),
            Some(
                "rsync: Denial of Service via crafted checksums during delta transfer \
                 (CVE-2026-119741)"
            )
        );
        // GitHub's advisory when Red Hat has none.
        let ghsa = r#"[{"ghsa_id": "GHSA-1", "summary": "FreeType OOB write"}]"#;
        assert_eq!(
            cve_title("freetype", "CVE-2025-27363", Some("{}"), Some(ghsa)).as_deref(),
            Some("freetype: FreeType OOB write (CVE-2025-27363)")
        );
        // A title naming the CVE already is not given it twice.
        let named = r#"{"bugzilla": {"description": "xz: CVE-2024-3094 backdoor"}}"#;
        assert_eq!(
            cve_title("xz", "CVE-2024-3094", Some(named), None).as_deref(),
            Some("xz: CVE-2024-3094 backdoor")
        );
        assert_eq!(cve_title("rsync", "CVE-1", None, None), None);
    }

    #[test]
    fn the_summary_comes_from_the_flag_the_mr_or_the_cve() {
        let args = |summary: Option<&str>| {
            let mut a = vec!["jira", "rsync", "--branch", "c9s"];
            if let Some(s) = summary {
                a.extend(["--summary", s]);
            }
            #[derive(clap::Parser)]
            struct P {
                #[command(flatten)]
                a: JiraArgs,
            }
            <P as clap::Parser>::parse_from(a).a
        };
        let mut t = target(Some("CVE-1"));
        let title = Some("rsync: from the CVE (CVE-1)");
        assert_eq!(
            summary_for(&args(Some("mine")), &t, title).as_deref(),
            Some("mine")
        );
        assert_eq!(summary_for(&args(None), &t, title).as_deref(), title);
        assert_eq!(summary_for(&args(None), &t, None), None);
        t.mr_title = Some("MR title".into());
        assert_eq!(
            summary_for(&args(None), &t, title).as_deref(),
            Some("CVE-1: MR title")
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
