// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `file-issue` subcommand.
//!
//! Given a CentOS Stream MR URL, file a tracking issue in the
//! corresponding `CentOS/proposed_updates/rpms/<pkg>` project on
//! gitlab.com. The issue body is a standardized markdown block
//! that `status` and `sync-issues` can parse back later.

use std::io::IsTerminal;
use std::process::ExitCode;

use crate::gitlab;
use crate::jira;
use crate::utils::{format_jira_lines, format_mr_line};

/// Label applied to tracking issues so `sync-issues` and
/// `status` can identify them.
const TRACKING_LABEL: &str = "cpu-sig-tracker";

/// Issue-type labels defined in the proposed_updates GitLab group.
/// Passed through `--type`. `Undefined` is for an update that fits
/// none, as Bodhi's "unspecified": one naming an advisory it does not
/// really fix, say, as when upstream only deprecated the unsafe code.
/// It is a label of its own, so it is only ever chosen, and `status
/// --refresh` does not take an issue without a type label for one
/// that was never given a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum IssueType {
    Enhancement,
    Bugfix,
    #[value(name = "arch-enablement")]
    ArchEnablement,
    Security,
    Undefined,
}

/// Every type, in the order they are offered.
const ISSUE_TYPES: [IssueType; 5] = [
    IssueType::Security,
    IssueType::Bugfix,
    IssueType::Enhancement,
    IssueType::ArchEnablement,
    IssueType::Undefined,
];

/// The type labels, for the readers of an issue's type: `status
/// --refresh` and `sync-updates`.
pub const TYPE_LABELS: [&str; 5] = [
    "arch-enablement",
    "bugfix",
    "enhancement",
    "security",
    "undefined",
];

impl IssueType {
    fn label(self) -> &'static str {
        match self {
            IssueType::Enhancement => "enhancement",
            IssueType::Bugfix => "bugfix",
            IssueType::ArchEnablement => "arch-enablement",
            IssueType::Security => "security",
            IssueType::Undefined => "undefined",
        }
    }
}

#[derive(clap::Args)]
pub struct FileIssueArgs {
    /// Full Merge Request URL to track, e.g.
    /// `gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42`
    /// (with the https:// scheme).
    pub mr_url: String,

    /// The stock build the update fixes (default: stock's current
    /// build; `status --refresh` fills it in later otherwise)
    #[arg(long, help_heading = "Issue")]
    pub affected: Option<String>,

    /// The stock build expected to carry the fix (default: the SIG's
    /// build without its ~proposed; `status --refresh` fills it in
    /// later otherwise)
    #[arg(long = "expected-fix", help_heading = "Issue")]
    pub expected_fix: Option<String>,

    /// The RHEL issue(s) the MR resolves, overriding those read from
    /// its Resolves: lines (repeatable, or comma-separated)
    #[arg(
        long,
        value_name = "KEY",
        value_delimiter = ',',
        help_heading = "Issue"
    )]
    pub jira: Vec<String>,

    /// File it confidential, for a fix still under embargo: hidden from
    /// the public, and so from the SIG's site, until made public by hand
    #[arg(long, help_heading = "Issue")]
    pub embargoed: bool,

    /// Free-form context note prepended to the issue body as
    /// a lead paragraph before the structured metadata.
    #[arg(long, help_heading = "Issue")]
    pub note: Option<String>,

    /// Override the release auto-derived from the MR target
    /// branch (e.g. `c10s`).
    #[arg(long, help_heading = "Issue")]
    pub release: Option<String>,

    /// The update's type label (default: security when it fixes a CVE;
    /// asked otherwise, and required without a terminal)
    #[arg(long = "type", value_enum, help_heading = "Issue")]
    pub issue_type: Option<IssueType>,

    /// Assign the issue to you (the GitLab token's user) without
    /// asking; at a terminal you are asked otherwise
    #[arg(long, help_heading = "Writing")]
    pub assign: bool,

    /// Print the issue that would be filed and exit without
    /// making any GitLab API calls.
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Print progress to stderr.
    #[arg(short, long, help_heading = "Output")]
    pub verbose: bool,
}

pub fn run(args: &FileIssueArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// How to create the SIG's repo for `package` when it has none yet:
/// `cbspkg sig-repo` for the merge request at `mr_url`, or by hand, a
/// pull mirror of the CentOS Stream repo with the proposed change's
/// branch pushed to it as `<release>-proposed-<RHEL key>`, from
/// `source_branch` (the merge request's) when known.
pub fn missing_sig_repo_help(
    mr_url: &str,
    package: &str,
    release: &str,
    source_branch: Option<&str>,
    jira_key: Option<&str>,
) -> String {
    let group = sandogasa_cbs::Sig::ProposedUpdates.gitlab_group();
    let stream = sandogasa_cbs::stream_project(package);
    let topic = sandogasa_cbs::proposed_branch(release, jira_key.unwrap_or("<RHEL key>"));
    format!(
        "the SIG has no repo for {package} yet ({group}/{package}). Create it with:\n\
         \x20 cbspkg sig-repo {mr_url}\n\
         or by hand, as a pull mirror of the CentOS Stream repo:\n\
         \x20 1. On GitLab: New project → Import project → Repository by URL, in {group}\n\
         \x20    URL: https://gitlab.com/{stream}.git\n\
         \x20    name {package}, public, with \"Mirror repository\" ticked; then in Settings →\n\
         \x20    Repository → Mirroring repositories, leave \"Overwrite diverged branches\" off\n\
         \x20 2. Push the change's branch to it, from the dist-git checkout:\n\
         \x20    git push git+ssh://git@gitlab.com/{group}/{package}.git \
         {}:refs/heads/{topic}",
        source_branch.unwrap_or("<branch>")
    )
}

/// Whether the SIG has a repo for `package`, where its tracking issues
/// are filed.
pub fn sig_repo_exists(package: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let path = format!(
        "{}/{package}",
        sandogasa_cbs::Sig::ProposedUpdates.gitlab_group()
    );
    Ok(gitlab::reader(&crate::utils::gitlab_base(), &path)?
        .project()?
        .is_some())
}

pub(crate) fn run_inner(args: &FileIssueArgs) -> Result<(), Box<dyn std::error::Error>> {
    let (_parsed_base, mr_project, iid) = gitlab::parse_mr_url(&args.mr_url)?;
    if args.verbose {
        eprintln!("[cpu-sig-tracker] fetching MR {iid} from {mr_project}");
    }

    let package = package_from_project(&mr_project)
        .ok_or_else(|| format!("could not extract package name from '{mr_project}'"))?;

    let mr_client = gitlab::client(&crate::utils::gitlab_base(), &mr_project)?;
    let mr = mr_client.merge_request(iid)?;

    let release = args
        .release
        .clone()
        .unwrap_or_else(|| mr.target_branch.clone());

    let jira_keys = if args.jira.is_empty() {
        extract_jira_keys(&mr.title, mr.description.as_deref())
    } else {
        args.jira.clone()
    };
    // Before Jira is asked anything: whether there is a repo to file
    // in. A dry run says so and still shows the issue.
    if !sig_repo_exists(package)? {
        let help = missing_sig_repo_help(
            &mr.web_url,
            package,
            &release,
            Some(&mr.source_branch),
            jira_keys.first().map(String::as_str),
        );
        if !args.dry_run {
            return Err(format!("{help}\nthen run this again.").into());
        }
        eprintln!("warning: {help}\n");
    }
    let group = sandogasa_cbs::Sig::ProposedUpdates.gitlab_group();
    let seen: Vec<(String, Option<JiraSeen>)> = jira_keys
        .iter()
        .map(|key| (key.clone(), fetch_jira_info(key, args.verbose)))
        .collect();
    let jira: Vec<(String, Option<String>, Option<String>)> = seen
        .iter()
        .map(|(key, s)| match s {
            Some(s) => (key.clone(), Some(s.status.clone()), s.resolution.clone()),
            None => (key.clone(), None, None),
        })
        .collect();
    // How urgent it is (#98): the CVEs the change fixes, rated, and the
    // priority Red Hat gave its RHEL issues.
    let cves = crate::triage::update_cves(
        [mr.title.as_str(), mr.description.as_deref().unwrap_or("")]
            .into_iter()
            .chain(
                seen.iter()
                    .filter_map(|(_, s)| s.as_ref().map(|s| s.summary.as_str())),
            ),
        seen.iter()
            .filter_map(|(_, s)| s.as_ref()?.description.as_deref()),
    );
    let cve_lines = crate::triage::rated_cve_lines(&cves, args.verbose);
    let priority = crate::triage::priority_label(
        seen.iter()
            .filter_map(|(_, s)| s.as_ref()?.priority.as_deref()),
    );
    let interactive = !args.dry_run && std::io::stdin().is_terminal();
    // The builds, from the evidence when not named (#101): stock's build
    // expected to carry the fix, named by the SIG's in -release or
    // -testing, and stock's current build, the affected one while the
    // fix is not in stock.
    let sig_build = crate::builds::sig_build(
        crate::status::fetch_proposed_updates_nvrs(&release, args.verbose)
            .get(package)
            .map(String::as_str),
        crate::status::fetch_proposed_updates_testing_nvrs(&release, args.verbose)
            .get(package)
            .map(String::as_str),
    );
    let expected_fix = settle_build(
        "--expected-fix",
        "the SIG's build",
        args.expected_fix.as_deref(),
        sig_build.as_deref().and_then(crate::builds::expected_fix),
        interactive,
    );
    let stock = crate::status::fetch_stream_nvrs(&release, &[package.to_string()], args.verbose);
    let affected = settle_build(
        "--affected",
        "stock's build",
        args.affected.as_deref(),
        crate::builds::affected(
            stock.get(package).map(String::as_str),
            sig_build.as_deref(),
            false,
        ),
        interactive,
    );
    let issue_type = resolve_type(args.issue_type, &cves, interactive, &ask_type).or_else(|e| {
        // A dry run shows the rest; the type is settled when filing.
        if args.dry_run {
            eprintln!("note: {e}");
            Ok(None)
        } else {
            Err(e)
        }
    })?;

    let body = format_body(&BodyFields {
        release: &release,
        mr_url: &mr.web_url,
        mr_title: &mr.title,
        mr_state: Some(&mr.state),
        jira: &jira,
        cve_lines: &cve_lines,
        affected: affected.as_deref(),
        expected_fix: expected_fix.as_deref(),
        note: args.note.as_deref(),
    });

    let title = format_title(package, &mr.title);

    let mut labels = build_labels(&release, issue_type);
    if let Some(p) = &priority {
        labels.push(',');
        labels.push_str(p);
    }

    if args.dry_run {
        println!("Would file in {group}/{package}:");
        println!("---");
        println!("title: {title}");
        println!("labels: {labels}");
        if args.embargoed {
            println!("confidential: yes (--embargoed)");
        }
        println!(
            "assignee: {}",
            if args.assign {
                "you (--assign)"
            } else {
                "you, if you say so when filing"
            }
        );
        println!("---");
        println!("{body}");
        return Ok(());
    }

    let tracking_project = format!("{group}/{package}");
    if args.verbose {
        eprintln!("[cpu-sig-tracker] creating issue in {tracking_project}");
    }
    let tracking_client = gitlab::client(&crate::utils::gitlab_base(), &tracking_project)?;
    // Asked before filing, so the answer is not lost to a failure after.
    let assignee = if wants_assignment(args.assign, interactive) {
        let me =
            sandogasa_gitlab::current_user(&crate::utils::gitlab_base(), &gitlab::load_token()?)?;
        Some(me)
    } else {
        None
    };
    // Confidential from the start, for embargoed work: set afterwards, it
    // would be public for a while (#80).
    let issue = tracking_client.create_issue_confidential(
        &title,
        Some(&body),
        Some(&labels),
        args.embargoed,
    )?;
    eprintln!("Filed #{} {}", issue.iid, issue.web_url);
    if let Some(me) = assignee {
        let update = gitlab::IssueUpdate {
            assignee_ids: Some(vec![me.id]),
            ..Default::default()
        };
        match tracking_client.edit_issue(issue.iid, &update) {
            Ok(_) => eprintln!("Assigned to {}", me.username),
            Err(e) => eprintln!("warning: could not assign it to {}: {e}", me.username),
        }
    }

    // The issue represents tracking work against an MR that already
    // exists, so the "To do" default isn't accurate — transition to
    // "In progress" immediately.
    if let Err(e) = tracking_client.set_work_item_status(issue.iid, "In progress") {
        eprintln!("warning: could not set status to In progress: {e}");
    }

    // Set the issue start_date to when the Koji build entered
    // the SIG's tag line (approximated by the build's creation
    // time — close enough for the "how long did we need this"
    // retrospective the SIG cares about). Goes via the GraphQL
    // workItemUpdate mutation; REST PUT /issues ignores
    // start_date on work items.
    if let Some(date) = sig_build
        .as_deref()
        .and_then(|nvr| build_start_date(nvr, args.verbose))
    {
        let formatted = date.format("%Y-%m-%d").to_string();
        if let Err(e) = tracking_client.set_work_item_dates(issue.iid, Some(&formatted), None) {
            eprintln!("warning: could not set start_date: {e}");
        }
    } else if args.verbose {
        eprintln!(
            "[cpu-sig-tracker] no Koji build found for {package} in c{release} \
             -release or -testing; leaving start_date blank",
        );
    }

    Ok(())
}

/// The day the SIG's build of `package` for `release` was made: its
/// newest build in `-release`, else in `-testing`, read as `status`
/// reads them. For a caller that has not read the tags already.
pub(crate) fn find_build_start_date(
    package: &str,
    release: &str,
    verbose: bool,
) -> Option<chrono::NaiveDate> {
    let nvr = crate::builds::sig_build(
        crate::status::fetch_proposed_updates_nvrs(release, verbose)
            .get(package)
            .map(String::as_str),
        crate::status::fetch_proposed_updates_testing_nvrs(release, verbose)
            .get(package)
            .map(String::as_str),
    )?;
    build_start_date(&nvr, verbose)
}

/// The day `nvr` was made, from Koji, as a tracking issue's start date.
pub(crate) fn build_start_date(nvr: &str, verbose: bool) -> Option<chrono::NaiveDate> {
    match sandogasa_koji::build_creation_date(nvr, Some(KOJI_PROFILE)) {
        Ok(Some(date)) => Some(date),
        Ok(None) => {
            if verbose {
                eprintln!("[cpu-sig-tracker] koji getBuild {nvr} gave no creation time");
            }
            None
        }
        Err(e) => {
            if verbose {
                eprintln!("[cpu-sig-tracker] koji getBuild {nvr} failed: {e}");
            }
            None
        }
    }
}

const KOJI_PROFILE: &str = "cbs";

/// Fields we substitute into the standardized issue body.
struct BodyFields<'a> {
    release: &'a str,
    mr_url: &'a str,
    mr_title: &'a str,
    mr_state: Option<&'a str>,
    /// `(key, status, resolution)` of each RHEL issue the MR resolves.
    jira: &'a [(String, Option<String>, Option<String>)],
    /// One line per CVE the change fixes (`triage::cve_line`).
    cve_lines: &'a [String],
    affected: Option<&'a str>,
    expected_fix: Option<&'a str>,
    note: Option<&'a str>,
}

/// Extract the final path segment of a GitLab project path —
/// by convention the package name for `rpms/<pkg>`-style
/// projects.
pub(crate) fn package_from_project(project: &str) -> Option<&str> {
    project.rsplit('/').next().filter(|s| !s.is_empty())
}

/// Build the comma-separated label list: tracking label +
/// release label (only if the release is a valid `c<N>s`
/// identifier) + type label (if given).
fn build_labels(release: &str, issue_type: Option<IssueType>) -> String {
    let mut labels: Vec<&str> = vec![TRACKING_LABEL];
    if let Some(r) = release_label(release) {
        labels.push(r);
    }
    if let Some(t) = issue_type {
        labels.push(t.label());
    }
    labels.join(",")
}

/// Return `Some(release)` if it looks like a CentOS Stream
/// release identifier (`c9s`, `c10s`, …). Returns `None` for
/// anything else (e.g. `main`, feature branches) so we don't
/// invent bogus project labels.
fn release_label(release: &str) -> Option<&str> {
    let digits = release.strip_prefix('c')?.strip_suffix('s')?;
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        Some(release)
    } else {
        None
    }
}

/// The RHEL issues a merge request resolves: the keys on its
/// `Resolves:` lines, each once, in order, as CentOS Stream's template
/// lists them, one per fix it carries (#86); without any, the first
/// `RHEL-\d+` in its title or description, as a hand-written MR may
/// name its issue anywhere.
pub(crate) fn extract_jira_keys(title: &str, description: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for line in description.unwrap_or("").lines() {
        if let Some(rest) = line.trim().strip_prefix("Resolves:")
            && let Some(key) = scan_rhel_key(rest)
            && !keys.contains(&key)
        {
            keys.push(key);
        }
    }
    if keys.is_empty() {
        keys.extend(
            [Some(title), description]
                .into_iter()
                .flatten()
                .find_map(scan_rhel_key),
        );
    }
    keys
}

/// Scan `text` for the first RHEL-\d+ occurrence that isn't
/// embedded inside a longer word. Shared with `status` so
/// legacy manually-filed issues (whose bodies don't match the
/// tool's standardized structure) still resolve a JIRA key.
pub(crate) fn scan_rhel_key(text: &str) -> Option<String> {
    const PREFIX: &str = "RHEL-";
    let mut rest = text;
    while let Some(idx) = rest.find(PREFIX) {
        let after = &rest[idx + PREFIX.len()..];
        // Make sure the char before PREFIX (if any) isn't
        // alphanumeric — avoid matching inside longer words.
        let before_ok = idx == 0
            || !rest[..idx]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
        let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        if before_ok && !digits.is_empty() {
            return Some(format!("{PREFIX}{digits}"));
        }
        rest = &rest[idx + PREFIX.len()..];
    }
    None
}

/// Fetch the JIRA issue's status and resolution for inclusion
/// in the rendered body. Returns `(status, resolution)` where
/// resolution is None when the issue is still open.
/// What file-issue reads of a RHEL issue.
struct JiraSeen {
    status: String,
    resolution: Option<String>,
    priority: Option<String>,
    summary: String,
    description: Option<String>,
}

fn fetch_jira_info(key: &str, verbose: bool) -> Option<JiraSeen> {
    let issue = jira::fetch_or_warn(key, verbose)?;
    Some(JiraSeen {
        status: issue.status().to_string(),
        resolution: issue.resolution().map(|s| s.to_string()),
        priority: issue.fields.priority.as_ref().map(|p| p.name.clone()),
        summary: issue.fields.summary.clone(),
        description: issue.fields.description.clone(),
    })
}

/// The type to file as. A merge request or RHEL issue naming a CVE is
/// evidence of `security`: with no `--type` it is used, and an explicit
/// type that disagrees is confirmed at a terminal (`interactive`),
/// defaulting to the evidence; without one the explicit type is kept
/// and the disagreement reported (#98). With no CVE and no `--type`,
/// the type is asked at a terminal (`ask`), and is an error without
/// one: every update has a type, `undefined` included, chosen rather
/// than left out. A dry run goes on without one.
fn resolve_type(
    explicit: Option<IssueType>,
    cves: &[String],
    interactive: bool,
    ask: &dyn Fn() -> Option<IssueType>,
) -> Result<Option<IssueType>, String> {
    if cves.is_empty() {
        return match explicit {
            Some(t) => Ok(Some(t)),
            None if interactive => ask()
                .map(Some)
                .ok_or_else(|| "no type chosen; pass --type".to_string()),
            None => Err(format!(
                "the change fixes no CVE that its merge request or RHEL issues name, so \
                 its type is not known: pass --type ({})",
                ISSUE_TYPES.map(IssueType::label).join(", ")
            )),
        };
    }
    Ok(match explicit {
        None => {
            eprintln!("type: security, as the change fixes {}", cves.join(", "));
            Some(IssueType::Security)
        }
        Some(IssueType::Security) => explicit,
        Some(other) => {
            let question = format!(
                "The change fixes {}, but --type is {}. File it as security instead?",
                cves.join(", "),
                other.label()
            );
            if interactive && sandogasa_cli::confirm(&question, true).unwrap_or(false) {
                Some(IssueType::Security)
            } else {
                if !interactive {
                    eprintln!(
                        "note: filing as {} as --type says, though the change fixes {}",
                        other.label(),
                        cves.join(", ")
                    );
                }
                explicit
            }
        }
    })
}

/// Ask for the type at the terminal, until one is picked; `None` at the
/// end of input.
fn ask_type() -> Option<IssueType> {
    use std::io::Write;
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "The change fixes no CVE it names. What type of update is it?"
    );
    for (i, t) in ISSUE_TYPES.iter().enumerate() {
        let _ = writeln!(err, "  {}) {}", i + 1, t.label());
    }
    loop {
        let _ = write!(err, "Type [1-{}]: ", ISSUE_TYPES.len());
        let _ = err.flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).ok()? == 0 {
            return None;
        }
        if let Some(t) = pick_type(&line) {
            return Some(t);
        }
    }
}

/// The type an answer picks: its number in [`ISSUE_TYPES`] or its
/// label. An empty answer picks none, so `undefined` is never a
/// default.
fn pick_type(answer: &str) -> Option<IssueType> {
    let answer = answer.trim();
    if let Ok(n) = answer.parse::<usize>() {
        return n.checked_sub(1).and_then(|i| ISSUE_TYPES.get(i).copied());
    }
    ISSUE_TYPES
        .into_iter()
        .find(|t| t.label().eq_ignore_ascii_case(answer))
}

/// A build line's value: the flag's when given, else `derived` (what
/// the evidence says, `source` naming it). A flag that disagrees with
/// the evidence is confirmed at a terminal (`interactive`), defaulting to
/// the evidence; without one it is kept and the disagreement reported.
fn settle_build(
    flag: &str,
    source: &str,
    explicit: Option<&str>,
    derived: Option<String>,
    interactive: bool,
) -> Option<String> {
    match (explicit, derived) {
        (None, derived) => {
            if let Some(d) = &derived {
                eprintln!("{flag} {d}, from {source}");
            }
            derived
        }
        (Some(e), Some(d)) if e != d => {
            let question = format!("{flag} is {e}, but {source} is {d}. Use {d}?");
            if interactive && sandogasa_cli::confirm(&question, true).unwrap_or(false) {
                Some(d)
            } else {
                if !interactive {
                    eprintln!("note: keeping {flag} {e}, though {source} is {d}");
                }
                Some(e.to_string())
            }
        }
        (Some(e), _) => Some(e.to_string()),
    }
}

/// Whether to assign the new issue to the person filing it: yes with
/// `--assign`, else asked at a terminal (defaulting to yes), else no.
fn wants_assignment(assign: bool, interactive: bool) -> bool {
    assign
        || (interactive
            && sandogasa_cli::confirm("Assign the tracking issue to you?", true).unwrap_or(false))
}

/// The issue's title: the package and what the update does, from the
/// merge request's title. The builds are in the body; a title of
/// versions (`rsync-3.5.0-1.el10 → rsync-3.5.0-2.el10`) would not say why
/// there is an update at all.
fn format_title(package: &str, mr_title: &str) -> String {
    format!("{package}: {mr_title}")
}

fn format_body(f: &BodyFields<'_>) -> String {
    let affected = f.affected.unwrap_or("_(unknown)_");
    let expected_fix = f.expected_fix.unwrap_or("_(unknown)_");
    let mr_line = format_mr_line(f.mr_url, f.mr_title, f.mr_state);
    let issues: Vec<(&str, Option<&str>, Option<&str>)> = f
        .jira
        .iter()
        .map(|(k, s, r)| (k.as_str(), s.as_deref(), r.as_deref()))
        .collect();
    let jira_line = format_jira_lines(&issues);

    let cves: String = f.cve_lines.iter().map(|l| format!("{l}\n")).collect();
    let metadata = format!(
        "{mr_line}\n\
         {jira_line}\n\
         {cves}\
         - **Release**: {release}\n\
         - **Affected build**: {affected}\n\
         - **Expected fix**: {expected_fix}\n",
        release = f.release,
    );

    match f.note {
        Some(note) if !note.trim().is_empty() => format!("{}\n\n{metadata}", note.trim()),
        _ => metadata,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_from_project_basic() {
        assert_eq!(
            package_from_project("redhat/centos-stream/rpms/xz"),
            Some("xz")
        );
    }

    #[test]
    fn package_from_project_trailing_slash_stripped() {
        // parse_mr_url already strips trailing slashes; no need to
        // handle here. Empty final segment returns None.
        assert_eq!(package_from_project("redhat/centos-stream/rpms/"), None);
    }

    #[test]
    fn package_from_project_flat() {
        assert_eq!(package_from_project("xz"), Some("xz"));
    }

    #[test]
    fn scan_rhel_key_in_title() {
        assert_eq!(
            extract_jira_keys("Fix RHEL-12345 backport", None),
            ["RHEL-12345"]
        );
    }

    #[test]
    fn scan_rhel_key_in_description_when_title_empty() {
        assert_eq!(
            extract_jira_keys("no key here", Some("see RHEL-42 for details")),
            ["RHEL-42"]
        );
    }

    #[test]
    fn scan_rhel_key_title_wins_over_description() {
        assert_eq!(
            extract_jira_keys("RHEL-1 summary", Some("actually RHEL-999")),
            ["RHEL-1"]
        );
    }

    #[test]
    fn scan_rhel_key_none_when_absent() {
        assert!(extract_jira_keys("no JIRA here", Some("nor here")).is_empty());
    }

    #[test]
    fn scan_rhel_key_ignores_embedded_in_word() {
        // "NOTRHEL-9" should not match — previous char is alnum.
        assert!(extract_jira_keys("NOTRHEL-9 thing", None).is_empty());
    }

    #[test]
    fn scan_rhel_key_requires_digits() {
        assert!(extract_jira_keys("bare RHEL- prefix", None).is_empty());
    }

    #[test]
    fn every_resolves_line_names_an_issue() {
        // CentOS Stream's template lists one per fix the MR carries.
        let description = "## Approved Development Ticket(s)\n\
                           List tickets like \"Resolves: RHEL-76229\".\n\
                           </details>\n\n\
                           Resolves: RHEL-303720\n\
                           Resolves: RHEL-303900\n\
                           Resolves: RHEL-303720\n";
        assert_eq!(
            extract_jira_keys("rsync: two fixes", Some(description)),
            ["RHEL-303720", "RHEL-303900"]
        );
    }

    #[test]
    fn scan_rhel_key_handles_url_context() {
        assert_eq!(
            extract_jira_keys("", Some("https://issues.redhat.com/browse/RHEL-7 please"),),
            ["RHEL-7"]
        );
    }

    #[test]
    fn the_title_says_what_the_update_does_whatever_the_builds() {
        assert_eq!(
            format_title(
                "rsync",
                "Fix denial of service via crafted checksums (CVE-2026-119741)"
            ),
            "rsync: Fix denial of service via crafted checksums (CVE-2026-119741)"
        );
    }

    #[test]
    #[serial_test::serial]
    fn format_body_full() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42",
            mr_title: "Fix CVE-2026-0001",
            mr_state: Some("opened"),
            jira: &[(
                "RHEL-12345".to_string(),
                Some("In Progress".to_string()),
                None,
            )],
            cve_lines: &[],
            affected: Some("xz-5.4-1.el10"),
            expected_fix: Some("xz-5.6-1.el10"),
            note: None,
        });
        assert!(
            !body.contains("##"),
            "body should not contain heading: {body}"
        );
        assert!(body.contains(
            "- **MR**: [Fix CVE-2026-0001](https://gitlab.com/redhat/centos-stream/rpms/xz/-/merge_requests/42) — opened"
        ));
        let expected = format!(
            "- **JIRA**: [RHEL-12345]({}/browse/RHEL-12345) — In Progress",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
        assert!(body.contains("- **Release**: c10s"));
        assert!(body.contains("- **Affected build**: xz-5.4-1.el10"));
        assert!(body.contains("- **Expected fix**: xz-5.6-1.el10"));
        // Separate Status line is no longer emitted — state
        // lives on the MR line, JIRA status on the JIRA line.
        assert!(
            !body.contains("- **Status**:"),
            "body should not contain a standalone Status line: {body}",
        );
    }

    #[test]
    fn format_body_without_jira() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            mr_state: None,
            jira: &[],
            cve_lines: &[],
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **Affected build**: _(unknown)_"));
        assert!(body.contains("- **Expected fix**: _(unknown)_"));
        assert!(body.contains("- **JIRA**: _(not found in MR; set with `--jira`)_"));
        // MR state suffix is omitted when unknown.
        assert!(body.contains("- **MR**: [t](https://example/mr)\n"));
    }

    #[test]
    #[serial_test::serial]
    fn format_body_with_resolved_jira_shows_resolution() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            mr_state: Some("merged"),
            jira: &[(
                "RHEL-1".to_string(),
                Some("Closed".to_string()),
                Some("Done".to_string()),
            )],
            cve_lines: &[],
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(body.contains("- **MR**: [t](https://example/mr) — merged"));
        let expected = format!(
            "- **JIRA**: [RHEL-1]({}/browse/RHEL-1) — Closed (Done)",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
    }

    #[test]
    fn release_label_accepts_c10s() {
        assert_eq!(release_label("c10s"), Some("c10s"));
    }

    #[test]
    fn release_label_accepts_c9s() {
        assert_eq!(release_label("c9s"), Some("c9s"));
    }

    #[test]
    fn release_label_rejects_main() {
        assert_eq!(release_label("main"), None);
    }

    #[test]
    fn release_label_rejects_prefix_only() {
        assert_eq!(release_label("cs"), None);
    }

    #[test]
    fn release_label_rejects_non_digit_body() {
        assert_eq!(release_label("cfoos"), None);
    }

    #[test]
    fn build_labels_all_three() {
        assert_eq!(
            build_labels("c10s", Some(IssueType::Security)),
            "cpu-sig-tracker,c10s,security"
        );
    }

    #[test]
    fn build_labels_tracking_only_when_release_invalid_and_no_type() {
        assert_eq!(build_labels("main", None), "cpu-sig-tracker");
    }

    #[test]
    fn build_labels_skips_invalid_release_but_keeps_type() {
        assert_eq!(
            build_labels("main", Some(IssueType::Bugfix)),
            "cpu-sig-tracker,bugfix"
        );
    }

    #[test]
    fn build_labels_release_only() {
        assert_eq!(build_labels("c9s", None), "cpu-sig-tracker,c9s");
    }

    #[test]
    fn issue_type_labels_match_gitlab_project_labels() {
        assert_eq!(IssueType::Enhancement.label(), "enhancement");
        assert_eq!(IssueType::Bugfix.label(), "bugfix");
        assert_eq!(IssueType::ArchEnablement.label(), "arch-enablement");
        assert_eq!(IssueType::Security.label(), "security");
    }

    #[test]
    #[serial_test::serial]
    fn format_body_with_jira_key_but_no_summary() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira: &[("RHEL-1".to_string(), None, None)],
            cve_lines: &[],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: None,
        });
        let expected = format!(
            "- **JIRA**: [RHEL-1]({}/browse/RHEL-1)\n",
            crate::utils::jira_base(),
        );
        assert!(body.contains(&expected), "{body}");
    }

    #[test]
    fn format_body_prepends_note() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira: &[],
            cve_lines: &[],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: Some("Tracking for SIG decision 2026-04-22."),
        });
        assert!(body.starts_with("Tracking for SIG decision 2026-04-22.\n\n- **MR**:"));
    }

    #[test]
    fn format_body_ignores_empty_note() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira: &[],
            cve_lines: &[],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: Some("   \n\t"),
        });
        assert!(body.starts_with("- **MR**:"));
    }

    #[test]
    fn format_body_trims_note_whitespace() {
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            jira: &[],
            cve_lines: &[],
            mr_state: None,
            affected: None,
            expected_fix: None,
            note: Some("  leading and trailing  \n\n"),
        });
        assert!(body.starts_with("leading and trailing\n\n- **MR**:"));
    }

    // ---- end-to-end wiremock + fake-koji test ----

    use crate::test_support::{EnvGuard, install_fake_bin};
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wiremock_path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    #[serial_test::serial]
    fn file_issue_end_to_end() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            // The SIG's repo the issue is filed in exists.
            Mock::given(method("GET"))
                .and(wiremock_path("/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "id": 7,
                    "path_with_namespace": "CentOS/proposed_updates/rpms/xz",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz",
                    "ssh_url_to_repo": "git@gitlab.example:CentOS/proposed_updates/rpms/xz.git",
                    "http_url_to_repo": "https://gitlab.example/CentOS/proposed_updates/rpms/xz.git",
                })))
                .mount(&server)
                .await;
            // MR fetch from the source project.
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Fxz/merge_requests/42",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 42,
                    "title": "Fix RHEL-1234 in xz",
                    "description": "Linked to RHEL-1234.",
                    "state": "opened",
                    "web_url": "https://gitlab.example/redhat/centos-stream/rpms/xz/-/merge_requests/42",
                    "source_branch": "fix",
                    "target_branch": "c10s",
                })))
                .mount(&server)
                .await;

            // JIRA enrichment.
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-1234"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "key": "RHEL-1234",
                    "fields": {
                        "summary": "s",
                        "status": { "name": "In Progress" },
                        "priority": { "name": "Major" }
                    }
                })))
                .mount(&server)
                .await;

            // create_issue POST on the tracking project, with the
            // priority Jira gave the RHEL issue as a label (#98).
            Mock::given(method("POST"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues",
                ))
                .and(wiremock::matchers::body_partial_json(json!({
                    "labels": "cpu-sig-tracker,c10s,security,priority::major"
                })))
                .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                    "iid": 7,
                    "title": "xz: Fix RHEL-1234 in xz",
                    "description": null,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/7",
                    "assignees": [],
                })))
                .expect(1)
                .mount(&server)
                .await;

            // --assign: the token's user, then the issue assigned to them.
            Mock::given(method("GET"))
                .and(wiremock_path("/api/v4/user"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"id": 5, "username": "me"})),
                )
                .mount(&server)
                .await;
            Mock::given(method("PUT"))
                .and(wiremock_path(
                    "/api/v4/projects/CentOS%2Fproposed_updates%2Frpms%2Fxz/issues/7",
                ))
                .and(wiremock::matchers::body_partial_json(json!({"assignee_ids": [5]})))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 7,
                    "title": "xz: Fix RHEL-1234 in xz",
                    "description": null,
                    "state": "opened",
                    "web_url": "https://gitlab.example/CentOS/proposed_updates/rpms/xz/-/issues/7",
                    "assignees": [{"username": "me"}],
                })))
                .expect(1)
                .mount(&server)
                .await;

            // GraphQL — set_work_item_status (→ In progress) and
            // set_work_item_dates (start_date) both land here.
            Mock::given(method("POST"))
                .and(wiremock_path("/api/graphql"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "data": {
                        "project": {
                            "workItems": {
                                "nodes": [{
                                    "id": "gid://gitlab/WorkItem/7",
                                    "widgets": [{
                                        "type": "STATUS",
                                        "status": { "name": "To do" }
                                    }],
                                    "namespace": {
                                        "workItemTypes": {
                                            "nodes": [{
                                                "name": "Issue",
                                                "widgetDefinitions": [{
                                                    "type": "STATUS",
                                                    "allowedStatuses": [{
                                                        "id": "gid://gitlab/status/1",
                                                        "name": "In progress"
                                                    }]
                                                }]
                                            }]
                                        }
                                    }
                                }]
                            }
                        },
                        "workItemUpdate": { "errors": [] }
                    }
                })))
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        install_fake_bin(
            dir.path(),
            "koji",
            &[
                // The SIG's builds: xz in -release.
                (
                    "list-tagged --latest -- proposed_updates10s-packages-main-release",
                    "Build  Tag  Built by\n-----  ---  --------\n\
                     xz-5.6.4-1~proposed.el10  proposed_updates10s-packages-main-release  me\n",
                ),
                // getBuild for the start date, both fields as CBS sends
                // them: 2026-04-22 10:11:12.5 UTC.
                (
                    "call --json-output getBuild -- xz-5.6.4-1~proposed.el10",
                    r#"{"nvr": "xz-5.6.4-1~proposed.el10",
                        "creation_time": "2026-04-22 10:11:12.500000+00:00",
                        "creation_ts": 1776852672.5}"#,
                ),
            ],
        );
        // Stock's build, for the affected line: no network in a test.
        install_fake_bin(dir.path(), "fedrq", &[("pkgs", "")]);
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

        let args = FileIssueArgs {
            mr_url: "https://gitlab.example/redhat/centos-stream/rpms/xz/-/merge_requests/42"
                .to_string(),
            affected: Some("xz-5.6.2-3.el10".to_string()),
            expected_fix: Some("xz-5.6.4-1~proposed.el10".to_string()),
            jira: vec![],
            release: None,
            issue_type: Some(IssueType::Security),
            note: None,
            embargoed: false,
            assign: true,
            dry_run: false,
            verbose: false,
        };
        run_inner(&args).expect("file-issue succeeds");
        // The start date is the build's creation day, from getBuild.
        let requests = runtime.block_on(server.received_requests()).unwrap();
        let start_dates: Vec<String> = requests
            .iter()
            .filter_map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).ok())
            .filter_map(|b| b.get("query")?.as_str().map(str::to_string))
            .filter(|q| q.contains("startDate:"))
            .collect();
        assert_eq!(start_dates.len(), 1, "{start_dates:?}");
        assert!(
            start_dates[0].contains(r#"startDate: "2026-04-22""#),
            "{start_dates:?}"
        );
    }

    /// rsync's first run: no SIG repo yet, so nothing is asked of Jira
    /// or posted, and the error says how to make one.
    #[test]
    #[serial_test::serial]
    fn a_missing_sig_repo_stops_file_issue_before_jira_with_how_to_make_it() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(wiremock_path(
                    "/api/v4/projects/redhat%2Fcentos-stream%2Frpms%2Frsync/merge_requests/90",
                ))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "iid": 90,
                    "title": "Fix denial of service via crafted checksums (CVE-2026-119741)",
                    "description": "Resolves: RHEL-303721",
                    "state": "opened",
                    "web_url": "https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/90",
                    "source_branch": "c10s-CVE-2026-119741",
                    "target_branch": "c10s",
                })))
                .mount(&server)
                .await;
            // Anything else (the SIG's project, Jira, the issue POST)
            // must not be reached, bar the project lookup's 404.
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(wiremock_path("/rest/api/2/issue/RHEL-303721"))
                .respond_with(ResponseTemplate::new(500))
                .expect(0)
                .mount(&server)
                .await;
        });
        let dir = tempdir().unwrap();
        let _guard = EnvGuard::new(&[
            ("GITLAB_TOKEN", "test-token"),
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("CPU_SIG_TRACKER_GITLAB_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_JIRA_BASE", &server.uri()),
        ]);
        let args = FileIssueArgs {
            mr_url: "https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/90"
                .to_string(),
            affected: None,
            expected_fix: None,
            jira: vec![],
            release: None,
            issue_type: None,
            note: None,
            embargoed: false,
            assign: true,
            dry_run: false,
            verbose: false,
        };
        let err = run_inner(&args).unwrap_err().to_string();
        assert!(
            err.contains("the SIG has no repo for rsync yet")
                && err.contains("  cbspkg sig-repo https://gitlab.example/redhat/centos-stream/rpms/rsync/-/merge_requests/90\n")
                && err.contains(
                    "rpms/rsync.git c10s-CVE-2026-119741:refs/heads/c10s-proposed-RHEL-303721"
                )
                && err.ends_with("then run this again."),
            "{err}"
        );
        runtime.block_on(server.verify());
    }

    #[test]
    fn without_a_cve_the_type_is_given_or_asked_and_never_assumed() {
        let never = || -> Option<IssueType> { panic!("not asked") };
        assert_eq!(
            resolve_type(Some(IssueType::Undefined), &[], true, &never),
            Ok(Some(IssueType::Undefined))
        );
        let err = resolve_type(None, &[], false, &never).unwrap_err();
        assert!(
            err.contains("pass --type (security, bugfix, enhancement, arch-enablement, undefined)"),
            "{err}"
        );
        assert_eq!(
            resolve_type(None, &[], true, &|| Some(IssueType::Bugfix)),
            Ok(Some(IssueType::Bugfix))
        );
        assert!(resolve_type(None, &[], true, &|| None).is_err());
        // Answers: a number or a label; nothing else, an empty one
        // included, picks a type.
        assert_eq!(pick_type("1\n"), Some(IssueType::Security));
        assert_eq!(pick_type("5"), Some(IssueType::Undefined));
        assert_eq!(pick_type(" Undefined \n"), Some(IssueType::Undefined));
        assert_eq!(
            pick_type("arch-enablement"),
            Some(IssueType::ArchEnablement)
        );
        for none in ["", "\n", "0", "6", "unspecified"] {
            assert_eq!(pick_type(none), None, "{none:?}");
        }
        assert_eq!(IssueType::Undefined.label(), "undefined");
        assert!(ISSUE_TYPES.iter().all(|t| TYPE_LABELS.contains(&t.label())));
    }

    #[test]
    fn a_cve_makes_it_a_security_update_unless_a_type_was_given() {
        let cves = vec!["CVE-2026-119741".to_string()];
        let never = || -> Option<IssueType> { panic!("not asked") };
        assert_eq!(
            resolve_type(None, &cves, false, &never),
            Ok(Some(IssueType::Security))
        );
        // Without a terminal, an explicit type that disagrees is kept.
        assert_eq!(
            resolve_type(Some(IssueType::Bugfix), &cves, false, &never),
            Ok(Some(IssueType::Bugfix))
        );
        assert_eq!(
            resolve_type(Some(IssueType::Security), &cves, false, &never),
            Ok(Some(IssueType::Security))
        );
        assert!(!wants_assignment(false, false));
        assert!(wants_assignment(true, false));
    }

    #[test]
    fn cve_lines_follow_the_jira_lines() {
        let line = crate::triage::cve_line("CVE-2026-119741", &[]);
        let body = format_body(&BodyFields {
            release: "c10s",
            mr_url: "https://example/mr",
            mr_title: "t",
            mr_state: Some("opened"),
            jira: &[("RHEL-1".to_string(), Some("New".to_string()), None)],
            cve_lines: std::slice::from_ref(&line),
            affected: None,
            expected_fix: None,
            note: None,
        });
        assert!(
            body.contains(&format!("— New\n{line}\n- **Release**: c10s\n")),
            "{body}"
        );
    }

    #[test]
    fn a_build_comes_from_the_evidence_unless_a_flag_names_it() {
        let d = || Some("rsync-3.5.0-2.el10".to_string());
        assert_eq!(
            settle_build("--expected-fix", "the SIG's build", None, d(), false).as_deref(),
            Some("rsync-3.5.0-2.el10")
        );
        assert_eq!(settle_build("--affected", "stock", None, None, false), None);
        // Without a terminal, a flag that disagrees is kept.
        assert_eq!(
            settle_build(
                "--expected-fix",
                "s",
                Some("rsync-3.5.0-3~proposed.el10"),
                d(),
                false
            )
            .as_deref(),
            Some("rsync-3.5.0-3~proposed.el10")
        );
        assert_eq!(
            settle_build("--affected", "s", Some("x"), None, false).as_deref(),
            Some("x")
        );
    }
}
