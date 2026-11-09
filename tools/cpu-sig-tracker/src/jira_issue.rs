// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `jira` subcommand: find or file the RHEL Jira issue a Proposed
//! Update's merge request names, one per branch.
//!
//! CentOS Stream's merge request template asks for an approved Jira
//! ticket, named in the description, and Red Hat wants one issue per
//! branch, so the issue comes before the MR: the usual target is a
//! package and branch, and an MR URL serves one already open. This
//! finds the open RHEL issues for the package, per CVE when there are
//! any, sorts out those for the branch's RHEL major, and, for the CVEs
//! with none, says what that can mean (Product Security's trackers are
//! restricted, so Red Hat's security data is asked whether one is
//! likely) and drafts one issue covering them: the affected version Red
//! Hat expects for the branch, a priority from the most severe CVE, and
//! a description. Red Hat's Jira cannot be edited once an issue is
//! filed, only commented on, so the draft is shown exactly as it would
//! be filed, and filing needs `--file` and a confirmation.

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

    /// CVEs the update fixes, repeated or comma-separated (default:
    /// those the MR fixes)
    #[arg(
        long,
        value_name = "CVE-ID",
        value_delimiter = ',',
        help_heading = "Issue"
    )]
    pub cve: Vec<String>,

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

    /// Upstream commit, issue or merge request the fix comes from, for
    /// the description (repeatable)
    #[arg(long, value_name = "URL", help_heading = "Issue")]
    pub upstream: Vec<String>,

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
/// 2026-10 its "single-value policy" allows a single affected version,
/// Red Hat's bot trims issues listing more, and which of them survives
/// is not predictable (RHEL-123605 and RHEL-123606 kept a RHEL minor
/// beside CentOS Stream). Red Hat sets the RHEL versions itself.
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

    /// Whether Red Hat assesses the CVE as not applicable to the
    /// release: the code is not built or not reachable there, or the
    /// platform it needs is not one RHEL runs on. The fix may still be
    /// worth carrying for uses Red Hat does not consider, but it is a
    /// harder case to make, so it does not set the issue's priority.
    pub fn not_applicable(&self) -> bool {
        matches!(self, RhelState::Listed(s) if s == "Not affected")
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

/// What finding no issue for a CVE can and cannot mean, one note per
/// CVE in `states` (each with where Red Hat puts the package in RHEL
/// `major`), after a note on searching anonymously. Product Security's
/// CVE trackers are restricted, hidden from anonymous searches and from
/// ordinary community accounts alike, so finding nothing does not mean
/// nothing was filed; Red Hat's security data says whether a tracker is
/// likely.
pub fn empty_search_notes(
    package: &str,
    states: &[(&str, Option<&RhelState>)],
    major: u32,
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
    for (cve, state) in states {
        notes.push(match state {
            Some(s @ RhelState::Listed(word)) if s.pending() => format!(
                "Red Hat lists RHEL {major} as {word} by {cve}, so Product Security has \
                 probably filed a tracker, restricted and so unlikely to show here. \
                 Filing may duplicate it, which Red Hat will link; name {cve} in the \
                 merge request so the two can be matched"
            ),
            Some(s @ RhelState::Listed(_)) if s.not_applicable() => format!(
                "Red Hat assesses {cve} as not applicable to RHEL {major} (Not affected), \
                 so it does not count toward the priority"
            ),
            Some(RhelState::Listed(word)) => {
                format!("Red Hat lists RHEL {major} as {word} for {cve}")
            }
            Some(RhelState::Fixed(advisory)) => format!(
                "Red Hat fixed {cve} in RHEL {major} with {advisory}: check whether \
                 CentOS Stream already has the fix"
            ),
            None => format!(
                "Red Hat's security data says nothing of {package} in RHEL {major} \
                 for {cve}"
            ),
        });
    }
    notes
}

/// Every CVE id in `texts`, upper-cased, each once, in order.
pub fn extract_cves(texts: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for id in texts.iter().flat_map(|t| crate::sync_updates::cve_ids(t)) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// One CVE's part of an issue's description.
pub struct CveBlock<'a> {
    pub cve: &'a str,
    /// Every source's rating, the one to trust first.
    pub ratings: &'a [Rating],
    /// NVD's description.
    pub description: Option<&'a str>,
    /// The RHEL major Red Hat assesses the CVE as not applicable to.
    pub not_applicable_in: Option<u32>,
}

/// The issue's description: the submitter's own text first, then for
/// each CVE its severity by every source that rated it, its description
/// and NVD link, then the merge request.
/// The submitter's text with the upstream links the fix comes from
/// after it, as a list, so a reader can check the fix against its
/// source. `None` when there is neither.
pub fn with_upstream(text: Option<&str>, upstream: &[String]) -> Option<String> {
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    if upstream.is_empty() {
        return text.map(str::to_string);
    }
    let links: Vec<String> = upstream.iter().map(|u| format!("- {u}")).collect();
    let block = format!("Upstream:\n{}", links.join("\n"));
    Some(match text {
        Some(t) => format!("{t}\n\n{block}"),
        None => block,
    })
}

pub fn compose_description(text: Option<&str>, cves: &[CveBlock], mr_url: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = text.map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(t.to_string());
    }
    let several = cves.len() > 1;
    for c in cves {
        let mut block = Vec::new();
        if several {
            block.push(format!("{}:", c.cve));
        }
        if let Some((first, rest)) = c.ratings.split_first() {
            block.push(format!("Severity: {} ({first})", first.level.as_str()));
            if !rest.is_empty() {
                let others: Vec<String> = rest.iter().map(Rating::to_string).collect();
                block.push(format!("Also rated: {}", others.join("; ")));
            }
        }
        if let Some(major) = c.not_applicable_in {
            block.push(format!(
                "Red Hat's assessment: not applicable to RHEL {major} (Not affected), {}",
                redhat_cve_page(c.cve)
            ));
        }
        if let Some(d) = c.description {
            block.push(String::new());
            block.push(d.trim().to_string());
        }
        block.push(String::new());
        block.push(format!("https://nvd.nist.gov/vuln/detail/{}", c.cve));
        parts.push(block.join("\n"));
    }
    if let Some(url) = mr_url {
        parts.push(format!("Merge request: {url}"));
    }
    parts.join("\n\n")
}

/// Red Hat's public page for a CVE.
pub fn redhat_cve_page(cve: &str) -> String {
    format!(
        "https://access.redhat.com/security/cve/{}",
        cve.to_ascii_lowercase()
    )
}

/// The CVE whose most trusted rating is the most severe, with that
/// rating: what a draft covering several CVEs takes its priority from.
/// On a tie, the first.
pub fn strongest<'a>(cves: &[(&'a str, &'a [Rating])]) -> Option<(&'a str, &'a Rating)> {
    let mut best: Option<(&str, &Rating)> = None;
    for (cve, ratings) in cves {
        if let Some(r) = ratings.first()
            && best.is_none_or(|(_, b)| r.level > b.level)
        {
            best = Some((cve, r));
        }
    }
    best
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
        (None, Some(n)) => o.push_str(&format!("  priority:    (Jira's default; {n})\n")),
        (None, None) => o.push_str("  priority:    (Jira's default)\n"),
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

/// Where the update is: the package, the branch, the CVEs, and the MR
/// when one was given.
struct Target {
    package: String,
    branch: String,
    cves: Vec<String>,
    mr_url: Option<String>,
    mr_title: Option<String>,
}

/// What the search found for one CVE, or for the package when the
/// update names none.
struct Lookup {
    cve: Option<String>,
    /// Open issues for the branch's RHEL major.
    here: Vec<Issue>,
    /// Open issues for other releases only.
    elsewhere: Vec<Issue>,
}

fn resolve_target(args: &JiraArgs) -> Result<Target, Box<dyn std::error::Error>> {
    let given: Vec<&str> = args.cve.iter().map(String::as_str).collect();
    if !args.target.contains("://") {
        let branch = args
            .branch
            .clone()
            .ok_or("a package name needs --branch (c8s, c9s, c10s)")?;
        return Ok(Target {
            package: args.target.clone(),
            branch,
            cves: extract_cves(&given),
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
    let cves = if given.is_empty() {
        extract_cves(&[&mr.title, mr.description.as_deref().unwrap_or("")])
    } else {
        extract_cves(&given)
    };
    Ok(Target {
        package,
        branch: args.branch.clone().unwrap_or(mr.target_branch),
        cves,
        mr_url: Some(mr.web_url),
        mr_title: Some(mr.title),
    })
}

/// The priority to file with, and why, following the workspace rule:
/// the severity (of `strongest`, the most severe CVE's most trusted
/// rating) fills it when no flag does; a flag that disagrees is
/// confirmed at a terminal, defaulting to the severity's, and kept with
/// a note otherwise.
fn settle_priority(
    flag: Option<&str>,
    strongest: Option<(&str, &Rating)>,
    ask: bool,
) -> Result<(Option<String>, Option<String>), String> {
    let derived = strongest.map(|(cve, r)| {
        (
            priority_for(r.level),
            format!("{} rates {cve} {}", r.source, r.detail),
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
    // many CVEs, one per CVE and release, and a duplicate is closed as
    // one. So each CVE is searched on its own.
    let major = branch_major(&target.branch)?;
    let logged_in = crate::jira::logged_in();
    let queries: Vec<Option<String>> = if target.cves.is_empty() {
        vec![None]
    } else {
        target.cves.iter().cloned().map(Some).collect()
    };
    let mut lookups = Vec::new();
    for cve in queries {
        let jql = search_jql(&target.package, cve.as_deref());
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
        let (here, elsewhere) = found.into_iter().partition(|i| for_major(i, major));
        lookups.push(Lookup {
            cve,
            here,
            elsewhere,
        });
    }
    let resolves = found_keys(&lookups);

    // What no issue covers on this release: those CVEs, or the update
    // itself when it names none.
    let uncovered: Vec<&str> = lookups
        .iter()
        .filter(|l| l.here.is_empty())
        .filter_map(|l| l.cve.as_deref())
        .collect();
    // Without a CVE the open issues are only possibly related, so the
    // update always gets its draft.
    let needs_draft = !uncovered.is_empty() || target.cves.is_empty();
    if !needs_draft {
        return report_found(args, &target, &lookups, logged_in, &resolves);
    }

    let facts: Vec<(&str, CveFacts)> = uncovered
        .iter()
        .map(|cve| (*cve, runtime.block_on(cve_facts(cve, args.verbose))))
        .collect();
    let states: Vec<Option<RhelState>> = facts
        .iter()
        .map(|(_, f)| {
            f.redhat
                .as_deref()
                .and_then(|j| rhel_state(j, &target.package, major))
        })
        .collect();
    let text = match &args.description_file {
        Some(p) => Some(
            std::fs::read_to_string(p)
                .map_err(|e| format!("cannot read --description-file {}: {e}", p.display()))?,
        ),
        None => args.description.clone(),
    };
    // The CVEs Red Hat assesses as not applicable to this release: still
    // fixed and described, but not what the priority is set by.
    let not_applicable: Vec<&str> = facts
        .iter()
        .zip(&states)
        .filter(|(_, s)| s.as_ref().is_some_and(RhelState::not_applicable))
        .map(|((cve, _), _)| *cve)
        .collect();
    let mut notes = {
        let pairs: Vec<(&str, Option<&RhelState>)> = facts
            .iter()
            .zip(&states)
            .map(|((cve, _), s)| (*cve, s.as_ref()))
            .collect();
        empty_search_notes(&target.package, &pairs, major, logged_in)
    };
    if target.cves.is_empty() && lookups.iter().any(|l| !l.here.is_empty()) {
        notes.push(
            "without a CVE there is nothing to match an issue on: the open issues \
             above are only possibly related, so check them before filing"
                .to_string(),
        );
    }
    if !not_applicable.is_empty() && text.is_none() {
        notes.push(format!(
            "say in --description why {} applies here: it is what Red Hat's \
             maintainer reads first, and the issue cannot be edited once filed",
            not_applicable.join(", ")
        ));
    }
    let search_report = render_search(&target, &lookups, logged_in, &notes);
    let redhat_states: Vec<serde_json::Value> = facts
        .iter()
        .zip(&states)
        .map(|((cve, _), s)| serde_json::json!({"cve": cve, "state": s.as_ref().map(state_word)}))
        .collect();

    // A single CVE can name the issue; several need a person to.
    let cve_title = match facts.as_slice() {
        [(cve, f)] => cve_title(&target.package, cve, f.redhat.as_deref(), f.ghsa.as_deref()),
        _ => None,
    };
    let Some(summary) = summary_for(args, &target, &uncovered, cve_title.as_deref()) else {
        let why = if uncovered.len() > 1 {
            format!(
                "no draft: {} CVEs have no issue on {}; pass --summary to name one \
                 issue covering them all",
                uncovered.len(),
                target.branch
            )
        } else {
            "no draft: pass --summary (or --cve) to draft an issue".to_string()
        };
        if args.file {
            return Err(why.into());
        }
        if args.json {
            let out = serde_json::json!({
                "package": target.package,
                "branch": target.branch,
                "cves": target.cves,
                "logged_in": logged_in,
                "by_cve": lookups_json(&lookups),
                "redhat_states": redhat_states,
                "notes": notes,
                "draft": null,
                "draft_cves": uncovered,
                "filed": null,
                "resolves": resolves,
            });
            println!("{}", serde_json::to_string_pretty(&out)?);
        } else {
            print!("{search_report}");
            println!("{}{}.", why[..1].to_uppercase(), &why[1..]);
            print_resolves(&resolves, None);
        }
        return Ok(());
    };

    let rated: Vec<(&str, &[Rating])> = facts
        .iter()
        .filter(|(cve, _)| !not_applicable.contains(cve))
        .map(|(cve, f)| (*cve, f.ratings.as_slice()))
        .collect();
    let (priority, priority_note) =
        settle_priority(args.priority.as_deref(), strongest(&rated), ask)?;
    let priority_note = with_not_counted(priority_note, &not_applicable, major);

    // The CVEs that apply first, as the ones the case rests on.
    let mut blocks: Vec<CveBlock> = facts
        .iter()
        .map(|(cve, f)| CveBlock {
            cve,
            ratings: &f.ratings,
            description: f.description.as_deref(),
            not_applicable_in: not_applicable.contains(cve).then_some(major),
        })
        .collect();
    blocks.sort_by_key(|b| b.not_applicable_in.is_some());

    let mut issue = NewIssue::new(PROJECT, "Bug", &summary);
    issue.components = vec![target.package.clone()];
    issue.versions = vec![branch_version(&target.branch)?];
    issue.priority = priority;
    let text = with_upstream(text.as_deref(), &args.upstream);
    issue.description = compose_description(text.as_deref(), &blocks, target.mr_url.as_deref());

    let checks = runtime.block_on(check_against_metadata(&client, &issue, args.verbose));
    let draft_heading = if uncovered.is_empty() {
        "Draft:\n".to_string()
    } else {
        format!("Draft, for {}:\n", uncovered.join(", "))
    };

    if !args.json {
        print!("{search_report}");
    }
    let filed = if args.file {
        if !args.json {
            println!("{draft_heading}");
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
        let mut all = resolves.clone();
        all.extend(filed.clone());
        let out = serde_json::json!({
            "package": target.package,
            "branch": target.branch,
            "cves": target.cves,
            "logged_in": logged_in,
            "by_cve": lookups_json(&lookups),
            "redhat_states": redhat_states,
            "notes": notes,
            "draft": issue.to_json()["fields"],
            "draft_cves": uncovered,
            "priority_from": priority_note,
            "checks": checks,
            "filed": filed,
            "resolves": all,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    match &filed {
        Some(key) => println!("\nFiled {}", issue_url(key)),
        None if args.file => println!("\nNot filed."),
        None => {
            println!("{draft_heading}");
            print!("{}", render_draft(&issue, priority_note.as_deref()));
            for c in &checks {
                println!("  ! {c}");
            }
            println!("\nNot filed: pass --file to file it.");
        }
    }
    print_resolves(&resolves, filed.as_deref());
    Ok(())
}

/// The priority note, saying which CVEs were not counted because Red
/// Hat assesses them as not applicable to RHEL `major`.
pub fn with_not_counted(
    note: Option<String>,
    not_applicable: &[&str],
    major: u32,
) -> Option<String> {
    if not_applicable.is_empty() {
        return note;
    }
    let skipped = format!(
        "not counted: {}, not applicable to RHEL {major} per Red Hat",
        not_applicable.join(", ")
    );
    Some(match note {
        Some(n) => format!("{n}; {skipped}"),
        None => skipped,
    })
}

/// The summary: `--summary`; else, for one CVE or none, the MR's title
/// with the CVE in front when the title does not name it, or the CVE's
/// own title ([`cve_title`]); for several, the MR's title as it is,
/// since no one CVE's title names a change fixing them all. `RFE: ` goes
/// in front for an enhancement, as Red Hat's RFEs read. An explicit
/// summary is never rewritten; one for an enhancement without the
/// prefix gets a note. `None` when there is nothing to call the issue.
fn summary_for(
    args: &JiraArgs,
    target: &Target,
    uncovered: &[&str],
    cve_title: Option<&str>,
) -> Option<String> {
    let rfe = matches!(args.issue_type, Some(IssueType::Enhancement));
    let has_rfe = |s: &str| s.to_ascii_uppercase().starts_with("RFE:");
    if let Some(s) = &args.summary {
        if rfe && !has_rfe(s) {
            eprintln!("note: Red Hat's enhancement requests start \"RFE: \"; --summary does not");
        }
        return Some(s.clone());
    }
    match uncovered {
        [] | [_] => {
            let title = target.mr_title.as_deref().or(cve_title)?;
            Some(derived_summary(title, uncovered.first().copied(), rfe))
        }
        _ => Some(derived_summary(target.mr_title.as_deref()?, None, rfe)),
    }
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

/// Every severity rating of `cve`, Red Hat's first: what a tracking
/// issue's CVE line gives (`triage`).
pub(crate) async fn cve_ratings(cve: &str, verbose: bool) -> Vec<Rating> {
    cve_facts(cve, verbose).await.ratings
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
        "{} [{}] {} ({})\n",
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

/// The issue keys found for the branch's release, each once, in order:
/// the `Resolves:` lines the MR needs before anything is filed. Only a
/// search by CVE finds one: without a CVE there is nothing to match an
/// issue on, and an open issue for the package may be about anything
/// (see [`render_search`]).
fn found_keys(lookups: &[Lookup]) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for i in lookups
        .iter()
        .filter(|l| l.cve.is_some())
        .flat_map(|l| &l.here)
    {
        if !keys.contains(&i.key) {
            keys.push(i.key.clone());
        }
    }
    keys
}

/// Red Hat's word for a state, for `--json`.
fn state_word(state: &RhelState) -> String {
    match state {
        RhelState::Fixed(a) => format!("Fixed ({a})"),
        RhelState::Listed(w) => w.clone(),
    }
}

/// What each search found, for `--json`.
fn lookups_json(lookups: &[Lookup]) -> Vec<serde_json::Value> {
    lookups
        .iter()
        .map(|l| {
            serde_json::json!({
                "cve": l.cve,
                "found": l.here.iter().map(issue_json).collect::<Vec<_>>(),
                "other_releases": l.elsewhere.iter().map(issue_json).collect::<Vec<_>>(),
            })
        })
        .collect()
}

/// What the searches found: for each CVE (or the package, when the
/// update names none), the open issues on the branch's release or that
/// there are none, then those open for other releases only, then what
/// an empty search may be missing.
fn render_search(target: &Target, lookups: &[Lookup], logged_in: bool, notes: &[String]) -> String {
    let who = if logged_in {
        "searched as this account"
    } else {
        "searched anonymously"
    };
    let mut o = format!(
        "Open {PROJECT} issues for {} on {} ({who}):\n",
        target.package, target.branch
    );
    for l in lookups {
        let label = match l.cve.as_deref() {
            Some(c) => format!("{c}: "),
            None if l.here.is_empty() => String::new(),
            None => "possibly related: ".to_string(),
        };
        if l.here.is_empty() {
            o.push_str(&format!("  {label}none visible\n"));
        }
        for i in &l.here {
            o.push_str(&format!("  {label}{}", issue_line(i)));
        }
    }
    let mut shown: Vec<&str> = lookups
        .iter()
        .flat_map(|l| &l.here)
        .map(|i| i.key.as_str())
        .collect();
    let mut elsewhere = String::new();
    for i in lookups.iter().flat_map(|l| &l.elsewhere) {
        if !shown.contains(&i.key.as_str()) {
            shown.push(&i.key);
            elsewhere.push_str(&format!("  {}", issue_line(i)));
        }
    }
    if !elsewhere.is_empty() {
        o.push_str("Open for other releases only:\n");
        o.push_str(&elsewhere);
    }
    for n in notes {
        o.push_str(&format!("note: {n}\n"));
    }
    o.push('\n');
    o
}

/// The lines the merge request's description needs: one per issue
/// found, and the one filed.
fn print_resolves(found: &[String], filed: Option<&str>) {
    let keys: Vec<&str> = found.iter().map(String::as_str).chain(filed).collect();
    if keys.is_empty() {
        return;
    }
    println!("For the merge request's description:");
    for k in keys {
        println!("  Resolves: {k}");
    }
}

/// Report what the searches found when every CVE (or the update) has an
/// issue on the branch's release, and the lines its MR needs.
fn report_found(
    args: &JiraArgs,
    target: &Target,
    lookups: &[Lookup],
    logged_in: bool,
    resolves: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    if args.json {
        let out = serde_json::json!({
            "package": target.package,
            "branch": target.branch,
            "cves": target.cves,
            "logged_in": logged_in,
            "by_cve": lookups_json(lookups),
            "draft": null,
            "filed": null,
            "resolves": resolves,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }
    print!("{}", render_search(target, lookups, logged_in, &[]));
    println!("Nothing filed.");
    print_resolves(resolves, None);
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
        let notes = empty_search_notes("rsync", &[("CVE-1", Some(&affected))], 9, false);
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert!(notes[0].starts_with("Searched anonymously"), "{notes:?}");
        assert!(
            notes[1].starts_with("Red Hat lists RHEL 9 as Affected by CVE-1")
                && notes[1].contains("probably filed a tracker"),
            "{notes:?}"
        );
        // Logged in, without a CVE: nothing to add.
        assert!(empty_search_notes("rsync", &[], 9, true).is_empty());
        // Other answers are stated as they are, one per CVE.
        let wont = RhelState::Listed("Will not fix".into());
        let fixed = RhelState::Fixed("RHSA-2025:3407".into());
        let notes = empty_search_notes(
            "rsync",
            &[
                ("CVE-1", Some(&wont)),
                ("CVE-2", Some(&fixed)),
                ("CVE-3", None),
            ],
            9,
            true,
        );
        assert_eq!(notes[0], "Red Hat lists RHEL 9 as Will not fix for CVE-1");
        assert!(
            notes[1].contains("CVE-2 in RHEL 9 with RHSA-2025:3407"),
            "{notes:?}"
        );
        assert_eq!(
            notes[2],
            "Red Hat's security data says nothing of rsync in RHEL 9 for CVE-3"
        );
    }

    fn target(cves: &[&str]) -> Target {
        Target {
            package: "rsync".into(),
            branch: "c9s".into(),
            cves: cves.iter().map(|c| c.to_string()).collect(),
            mr_url: None,
            mr_title: None,
        }
    }

    fn lookup(cve: Option<&str>, here: Vec<Issue>, elsewhere: Vec<Issue>) -> Lookup {
        Lookup {
            cve: cve.map(str::to_string),
            here,
            elsewhere,
        }
    }

    #[test]
    fn each_cve_shows_its_issue_and_other_releases_are_listed_once() {
        let other = || issue("RHEL-10", &["rhel-10.1", "CentOS Stream 10"]);
        let lookups = [
            lookup(
                Some("CVE-1"),
                vec![issue("RHEL-9", &["rhel-9.6"])],
                vec![other()],
            ),
            lookup(Some("CVE-2"), vec![], vec![other()]),
        ];
        let shown = render_search(
            &target(&["CVE-1", "CVE-2"]),
            &lookups,
            false,
            &["something".to_string()],
        );
        assert!(
            shown.starts_with("Open RHEL issues for rsync on c9s (searched anonymously):\n"),
            "{shown}"
        );
        assert!(shown.contains("  CVE-1: "), "{shown}");
        assert!(shown.contains("/browse/RHEL-9 [New]"), "{shown}");
        assert!(shown.contains("  CVE-2: none visible\n"), "{shown}");
        assert_eq!(shown.matches("RHEL-10 [").count(), 1, "{shown}");
        assert!(
            shown.contains("Open for other releases only:\n")
                && shown.contains("(rhel-10.1, CentOS Stream 10)"),
            "{shown}"
        );
        assert!(shown.contains("note: something\n"), "{shown}");
        assert_eq!(found_keys(&lookups), ["RHEL-9"]);

        // Without a CVE, the package's issues are unlabelled.
        let plain = render_search(&target(&[]), &[lookup(None, vec![], vec![])], true, &[]);
        assert_eq!(
            plain,
            "Open RHEL issues for rsync on c9s (searched as this account):\n  none visible\n\n"
        );
    }

    #[test]
    fn without_a_cve_an_open_issue_is_only_possibly_related() {
        // rsync had RHEL-303720 open for its CVE when an unrelated fix
        // without one was to be filed: that issue is not the new one.
        let lookups = [lookup(
            None,
            vec![issue("RHEL-303720", &["CentOS Stream 9"])],
            vec![],
        )];
        assert!(found_keys(&lookups).is_empty());
        let shown = render_search(&target(&[]), &lookups, true, &[]);
        assert!(
            shown.contains("  possibly related: ") && shown.contains("RHEL-303720 ["),
            "{shown}"
        );
        // With a CVE, the issue the search found is the one.
        let by_cve = [lookup(
            Some("CVE-1"),
            vec![issue("RHEL-303720", &["CentOS Stream 9"])],
            vec![],
        )];
        assert_eq!(found_keys(&by_cve), ["RHEL-303720"]);
    }

    #[test]
    fn upstream_links_follow_the_submitter_s_text() {
        let up = ["https://github.com/RsyncProject/rsync/issues/1157".to_string()];
        assert_eq!(
            with_upstream(Some("sum2 is 16 bytes, sha1 is 20."), &up).as_deref(),
            Some(
                "sum2 is 16 bytes, sha1 is 20.\n\nUpstream:\n\
                 - https://github.com/RsyncProject/rsync/issues/1157"
            )
        );
        assert_eq!(
            with_upstream(None, &up).as_deref(),
            Some("Upstream:\n- https://github.com/RsyncProject/rsync/issues/1157")
        );
        assert_eq!(with_upstream(Some("  "), &[]), None);
    }

    #[test]
    fn cves_are_read_out_of_an_mr_each_once() {
        assert_eq!(
            extract_cves(&["OOB write fixes for CVE-2025-27363"]),
            ["CVE-2025-27363"]
        );
        assert_eq!(
            extract_cves(&[
                "Rebase for cve-2026-114583 and CVE-2026-119741",
                "Fixes CVE-2026-119741.",
            ]),
            ["CVE-2026-114583", "CVE-2026-119741"]
        );
        assert!(extract_cves(&["CVE-26-1 and none", "no id here"]).is_empty());
    }

    fn rating(level: Level, source: &str, detail: &str) -> Rating {
        Rating {
            level,
            source: source.to_string(),
            detail: detail.to_string(),
        }
    }

    #[test]
    fn the_description_carries_every_rating_and_the_links() {
        let ratings = [
            rating(Level::High, "Red Hat", "Important (CVSS 3: 8.1)"),
            rating(Level::High, "NVD", "CVSS 3.1 8.1 high (nvd@nist.gov)"),
        ];
        let d = compose_description(
            Some("Backport of the upstream fix, tested against the reproducer."),
            &[CveBlock {
                cve: "CVE-2025-27363",
                ratings: &ratings,
                description: Some(
                    "An out of bounds write exists in FreeType versions 2.13.0 and below.",
                ),
                not_applicable_in: None,
            }],
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
            compose_description(Some("RFE: ship pcilmr"), &[], None),
            "RFE: ship pcilmr"
        );
        // Several CVEs: a block each, headed by its id.
        let low = [rating(Level::Low, "Red Hat", "Low")];
        let d = compose_description(
            None,
            &[
                CveBlock {
                    cve: "CVE-1",
                    ratings: &ratings,
                    description: None,
                    not_applicable_in: None,
                },
                CveBlock {
                    cve: "CVE-2",
                    ratings: &low,
                    description: None,
                    not_applicable_in: None,
                },
            ],
            None,
        );
        assert_eq!(
            d,
            "CVE-1:\nSeverity: high (Red Hat: Important (CVSS 3: 8.1))\n\
             Also rated: NVD: CVSS 3.1 8.1 high (nvd@nist.gov)\n\n\
             https://nvd.nist.gov/vuln/detail/CVE-1\n\n\
             CVE-2:\nSeverity: low (Red Hat: Low)\n\n\
             https://nvd.nist.gov/vuln/detail/CVE-2"
        );
    }

    #[test]
    fn several_cves_take_the_most_severe_one_s_priority() {
        let low = [rating(Level::Low, "Red Hat", "Low")];
        let high = [rating(Level::High, "NVD", "CVSS 3.1 8.1 high")];
        let also_high = [rating(Level::High, "Red Hat", "Important")];
        let (cve, r) = strongest(&[
            ("CVE-1", &low[..]),
            ("CVE-2", &[][..]),
            ("CVE-3", &high[..]),
            ("CVE-4", &also_high[..]),
        ])
        .unwrap();
        // The first of the most severe.
        assert_eq!((cve, r.source.as_str()), ("CVE-3", "NVD"));
        assert!(strongest(&[("CVE-1", &[][..])]).is_none());
    }

    #[test]
    fn a_cve_red_hat_finds_not_applicable_is_described_but_not_counted() {
        let na = RhelState::Listed("Not affected".into());
        assert!(na.not_applicable());
        assert!(!RhelState::Listed("Will not fix".into()).not_applicable());
        assert!(!RhelState::Fixed("RHSA-1".into()).not_applicable());
        assert_eq!(
            empty_search_notes("rsync", &[("CVE-2", Some(&na))], 9, true),
            [
                "Red Hat assesses CVE-2 as not applicable to RHEL 9 (Not affected), \
              so it does not count toward the priority"
            ]
        );
        // The priority note says which were not counted, with or
        // without a priority derived from the others.
        assert_eq!(
            with_not_counted(Some("Red Hat rates CVE-1 Important".into()), &["CVE-2"], 9)
                .as_deref(),
            Some(
                "Red Hat rates CVE-1 Important; not counted: CVE-2, not applicable to \
                 RHEL 9 per Red Hat"
            )
        );
        assert_eq!(
            with_not_counted(None, &["CVE-2", "CVE-3"], 9).as_deref(),
            Some("not counted: CVE-2, CVE-3, not applicable to RHEL 9 per Red Hat")
        );
        assert_eq!(with_not_counted(None, &[], 9), None);
        // The description says so in the CVE's own section.
        let d = compose_description(
            None,
            &[CveBlock {
                cve: "CVE-2024-12084",
                ratings: &[],
                description: None,
                not_applicable_in: Some(9),
            }],
            None,
        );
        assert_eq!(
            d,
            "Red Hat's assessment: not applicable to RHEL 9 (Not affected), \
             https://access.redhat.com/security/cve/cve-2024-12084\n\n\
             https://nvd.nist.gov/vuln/detail/CVE-2024-12084"
        );
        // A priority Jira will default shows why there is none.
        let mut issue = NewIssue::new("RHEL", "Bug", "s");
        issue.description = String::new();
        assert!(
            render_draft(&issue, Some("not counted: CVE-2"))
                .contains("priority:    (Jira's default; not counted: CVE-2)")
        );
    }

    #[test]
    fn a_disagreeing_priority_is_kept_with_a_note_when_nobody_can_be_asked() {
        let important = rating(Level::High, "Red Hat", "Important");
        let rated = Some(("CVE-1", &important));
        // No flag: the severity's.
        let (p, why) = settle_priority(None, rated, false).unwrap();
        assert_eq!(p.as_deref(), Some("Major"));
        assert_eq!(why.as_deref(), Some("Red Hat rates CVE-1 Important"));
        // A flag that agrees, or with nothing to compare against.
        assert_eq!(
            settle_priority(Some("Major"), rated, false)
                .unwrap()
                .0
                .as_deref(),
            Some("Major")
        );
        assert_eq!(
            settle_priority(Some("Minor"), None, false)
                .unwrap()
                .0
                .as_deref(),
            Some("Minor")
        );
        // A flag that disagrees, unattended: kept, with why it differs.
        let (p, why) = settle_priority(Some("Critical"), rated, false).unwrap();
        assert_eq!(p.as_deref(), Some("Critical"));
        assert!(why.unwrap().contains("which maps to Major"));
        // Nothing to go on: Jira's default.
        assert_eq!(settle_priority(None, None, false).unwrap(), (None, None));
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
        let mut t = target(&["CVE-1"]);
        let title = Some("rsync: from the CVE (CVE-1)");
        let one = ["CVE-1"];
        assert_eq!(
            summary_for(&args(Some("mine")), &t, &one, title).as_deref(),
            Some("mine")
        );
        assert_eq!(summary_for(&args(None), &t, &one, title).as_deref(), title);
        assert_eq!(summary_for(&args(None), &t, &one, None), None);
        // Several CVEs: never a title made up from them.
        let two = ["CVE-1", "CVE-2"];
        assert_eq!(summary_for(&args(None), &t, &two, None), None);
        assert_eq!(
            summary_for(&args(Some("rsync: rebase to 3.5.2")), &t, &two, None).as_deref(),
            Some("rsync: rebase to 3.5.2")
        );
        t.mr_title = Some("MR title".into());
        assert_eq!(
            summary_for(&args(None), &t, &one, title).as_deref(),
            Some("CVE-1: MR title")
        );
        // The MR's title is a person's, and is taken as it is.
        assert_eq!(
            summary_for(&args(None), &t, &two, None).as_deref(),
            Some("MR title")
        );
    }

    #[test]
    fn cve_takes_a_list() {
        #[derive(clap::Parser)]
        struct P {
            #[command(flatten)]
            a: JiraArgs,
        }
        let p = <P as clap::Parser>::parse_from([
            "jira",
            "rsync",
            "--cve",
            "CVE-1,CVE-2",
            "--cve",
            "CVE-3",
        ]);
        assert_eq!(p.a.cve, ["CVE-1", "CVE-2", "CVE-3"]);
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
