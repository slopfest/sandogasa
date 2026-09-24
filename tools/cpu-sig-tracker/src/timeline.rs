// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `timeline` — how long each Proposed Update took, and where the
//! time went. One row per tracking issue, open or closed, with the
//! dates that were kept in a spreadsheet by hand: when the RHEL
//! issue, the MR and the tracking issue were filed; when the SIG's
//! build reached `-release` (the change was covered from then); when
//! the fix reached stock CentOS Stream and became compose-bound; when
//! Red Hat's advisory shipped; and three spans — coverage (SIG build
//! in `-release` until the stock fix is compose-bound), Stream's lag
//! against RHEL, and the shadow gap: a stock build past the SIG's went
//! compose-bound while the SIG build was still tagged, and how long
//! until the SIG rebuilt or retired (or "still", when nobody has).
//!
//! Everything is read from the systems that keep history for good —
//! GitLab, Jira, CBS Koji, CentOS Stream's Koji and Red Hat's
//! security data — off the metadata a tracking issue carries: its
//! package and release, the MR, the RHEL key, the CVE in its title.
//! The "expected fix" a tracking issue names is a guess and is not
//! used; the stock fix is the build made from the commit that names
//! the change (the way `ping` and `retire` read evidence), or the
//! first compose-bound stock build after that commit.

use std::collections::BTreeMap;
use std::process::ExitCode;

use chrono::{NaiveDate, NaiveDateTime};
use sandogasa_koji::{TagEvent, build_source_commit, package_history, parse_nvr};
use sandogasa_rpmvercmp::compare_evr;

use crate::dump_inventory::proposed_updates_tag;
use crate::gitlab;
use crate::ping::{evidence_tokens, find_evidence_commit};
use crate::status::{parse_mr_line, scan_mr_url_in_body};
use crate::utils::gitlab_base;

const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";
const CBS_PROFILE: &str = "cbs";
/// centpkg ships `/etc/koji.conf.d/stream.conf` for CentOS Stream's
/// hub, which is readable without an account.
const STREAM_PROFILE: &str = "stream";
const TYPE_LABELS: [&str; 4] = ["security", "bugfix", "enhancement", "arch-enablement"];

#[derive(clap::Args)]
pub struct TimelineArgs {
    /// Restrict to a single release (e.g. `c10s`)
    #[arg(long)]
    pub release: Option<String>,

    /// Restrict to these package(s) (repeat/CSV)
    #[arg(long, value_delimiter = ',')]
    pub package: Vec<String>,

    /// Open tracking issues only (closed ones are the history and are
    /// included by default)
    #[arg(long)]
    pub open_only: bool,

    /// Emit a machine-readable JSON array instead of a table
    #[arg(long, conflicts_with = "csv")]
    pub json: bool,

    /// Emit CSV with the spreadsheet's columns instead of a table
    #[arg(long)]
    pub csv: bool,

    /// Print progress to stderr
    #[arg(short, long)]
    pub verbose: bool,
}

/// One change's dates and spans. Dates are `YYYY-MM-DD`; spans are
/// days, negative when the second event came first.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Row {
    pub release: String,
    pub package: String,
    /// The type label: `security`, `bugfix`, …
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The CVE the change addresses, when the titles name one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub issue_url: String,
    pub issue_filed: Option<String>,
    pub issue_closed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira_key: Option<String>,
    pub jira_filed: Option<String>,
    pub jira_resolved: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mr_url: Option<String>,
    pub mr_filed: Option<String>,
    pub mr_merged: Option<String>,
    /// The SIG's first `-release` build for the change, and when it
    /// was first tagged anywhere on CBS (built) and into `-release`
    /// (covered from).
    pub sig_build: Option<String>,
    pub sig_built: Option<String>,
    pub covered_from: Option<String>,
    /// The stock build carrying the fix, when it was first tagged on
    /// Stream's hub (built) and when it became compose-bound
    /// (`<release>-pending-signed`, available).
    pub stock_fix: Option<String>,
    pub stock_built: Option<String>,
    pub stock_available: Option<String>,
    /// How the stock fix was identified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stock_fix_by: Option<String>,
    pub rhel_advisory: Option<String>,
    pub rhel_available: Option<String>,
    /// A stock build past the SIG's went compose-bound while the SIG
    /// build was tagged, and was not the fix.
    pub shadowed_since: Option<String>,
    pub shadow_until: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shadow_ended_by: Option<String>,
    pub still_shadowed: bool,
    pub coverage_days: Option<i64>,
    pub stream_vs_rhel_days: Option<i64>,
    pub shadow_days: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
}

pub fn run(args: &TimelineArgs) -> ExitCode {
    match build_rows(args) {
        Ok(rows) => {
            if args.json {
                match serde_json::to_string_pretty(&rows) {
                    Ok(j) => println!("{j}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else if args.csv {
                print!("{}", render_csv(&rows));
            } else {
                print!("{}", render(&rows));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The dates a tracking issue's systems record, gathered once per
/// issue; [`assemble`] turns them into a [`Row`].
#[derive(Debug, Default)]
pub struct Sources {
    pub issue_created: Option<String>,
    pub issue_closed: Option<String>,
    pub mr_created: Option<String>,
    pub mr_merged: Option<String>,
    /// The SIG's `-release` tag events for the package on CBS (added
    /// only), oldest first, and every CBS event for those builds.
    pub cbs: Vec<TagEvent>,
    /// Every Stream tag event for the package, oldest first.
    pub stream: Vec<TagEvent>,
    /// The stock commit naming the change, if one does: sha, token
    /// named, committed date.
    pub evidence: Option<(String, String, Option<String>)>,
    /// Source commit of each stock build looked at, sha by NVR.
    pub stock_sources: BTreeMap<String, String>,
    pub jira_created: Option<String>,
    pub jira_resolved: Option<String>,
    pub rhel: Option<(String, String)>,
    /// The CVE the change addresses, from the issue's or the MR's
    /// title.
    pub cve: Option<String>,
    /// The stock build a person recorded on the issue as carrying the
    /// fix (`- **Stock fix**:`), and the note beside it.
    pub recorded_fix: Option<(String, String)>,
}

pub(crate) fn build_rows(args: &TimelineArgs) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    sandogasa_cli::require_tools(&[("koji", "sudo dnf install koji centpkg", Some("version"))])?;
    let base = gitlab_base();
    let group = gitlab::group_client(&base, PROPOSED_UPDATES_GROUP)?;
    let state = if args.open_only { Some("opened") } else { None };
    let releases: Vec<String> = match &args.release {
        Some(r) => vec![r.clone()],
        None => {
            if args.verbose {
                eprintln!("[timeline] fetching issues to find the releases");
            }
            let all = match state {
                Some(s) => group.list_issues_where(&[("state", s)])?,
                None => group.list_issues_where(&[("scope", "all")])?,
            };
            releases_from_labels(&all)
        }
    };
    let mut rows = Vec::new();
    for release in &releases {
        if args.verbose {
            eprintln!("[timeline] fetching {release} issues");
        }
        for issue in group.list_issues(release, state)? {
            let Some(package) = gitlab::package_from_issue_url(&issue.web_url) else {
                continue;
            };
            if !args.package.is_empty() && !args.package.iter().any(|p| p == package) {
                continue;
            }
            if args.verbose {
                eprintln!("[timeline] {package} {release}: {}", issue.web_url);
            }
            let sources = gather(&issue, package, release, args.verbose);
            rows.push(assemble(&issue, package, release, &sources));
        }
    }
    rows.sort_by(|a, b| (&a.release, &a.package).cmp(&(&b.release, &b.package)));
    Ok(rows)
}

/// The releases the issues are labelled with: `c9s`, `c10s`.
fn releases_from_labels(issues: &[gitlab::Issue]) -> Vec<String> {
    issues
        .iter()
        .flat_map(|i| i.labels.iter())
        .filter(|l| is_release_label(l))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_release_label(l: &str) -> bool {
    l.strip_prefix('c')
        .and_then(|r| r.strip_suffix('s'))
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Read every source for one issue. Each read is best-effort: what
/// cannot be read leaves its dates blank, and `notes` says so.
fn gather(issue: &gitlab::Issue, package: &str, release: &str, verbose: bool) -> Sources {
    let base = gitlab_base();
    let body = issue.description.clone().unwrap_or_default();
    let mut s = Sources {
        issue_created: issue.created_at.clone(),
        issue_closed: issue.closed_at.clone(),
        recorded_fix: crate::utils::parse_stock_fix_line(&body),
        ..Default::default()
    };
    // The MR.
    let mr_url = parse_mr_line(&body)
        .map(|(u, _)| u)
        .or_else(|| scan_mr_url_in_body(&body));
    let mut mr_title = String::new();
    let mut mr_branch = String::new();
    if let Some(url) = &mr_url
        && let Ok((_, project, iid)) = gitlab::parse_mr_url(url)
    {
        match gitlab::client(&base, &project).and_then(|c| c.merge_request(iid)) {
            Ok(mr) => {
                s.mr_created = mr.created_at.clone();
                s.mr_merged = mr.merged_at.clone();
                mr_title = mr.title.clone();
                mr_branch = mr.source_branch.clone();
            }
            Err(e) if verbose => eprintln!("[timeline] cannot read {url}: {e}"),
            Err(_) => {}
        }
    }
    // Jira.
    if let Some(key) = crate::utils::parse_jira_key_from_body(&body)
        .or_else(|| crate::file_issue::scan_rhel_key(&body))
        && let Ok(Some(j)) = crate::jira::fetch(&key, verbose)
    {
        s.jira_created = j.fields.created.clone();
        s.jira_resolved = j.fields.resolutiondate.clone();
    }
    // CBS: the SIG's builds.
    match package_history(package, Some(CBS_PROFILE)) {
        Ok(events) => s.cbs = events,
        Err(e) if verbose => eprintln!("[timeline] cbs history for {package}: {e}"),
        Err(_) => {}
    }
    // Stream: stock's builds.
    match package_history(package, Some(STREAM_PROFILE)) {
        Ok(events) => s.stream = events,
        Err(e) if verbose => eprintln!("[timeline] stream history for {package}: {e}"),
        Err(_) => {}
    }
    // The commit naming the change, and the stock builds' sources.
    let tokens = evidence_tokens(&body, &issue.title, &mr_title, &mr_branch);
    if let Ok(commits) = gitlab::client(&base, &format!("redhat/centos-stream/rpms/{package}"))
        .and_then(|c| c.branch_commits(release))
        && let Some((c, t)) = find_evidence_commit(&commits, &tokens)
    {
        s.evidence = Some((c.id.clone(), t.to_string(), c.committed_date.clone()));
        // Which stock build was made from it: ask the hub for the
        // source of each compose-bound build after the commit, newest
        // last, and stop at the first match.
        let after = c.committed_date.as_deref().and_then(rfc3339_to_naive);
        for nvr in compose_bound(&s.stream, release)
            .into_iter()
            .filter(|(_, at)| after.is_none_or(|a| *at >= a))
            .map(|(nvr, _)| nvr)
            .take(6)
        {
            if let Ok(Some(sha)) = build_source_commit(&nvr, Some(STREAM_PROFILE)) {
                let hit = sha.starts_with(&c.id) || c.id.starts_with(&sha);
                s.stock_sources.insert(nvr, sha);
                if hit {
                    break;
                }
            }
        }
    }
    // Red Hat's advisory for the CVE, for this RHEL major.
    s.cve = tokens.iter().find(|t| t.starts_with("CVE-")).cloned();
    if let Some(cve) = &s.cve {
        s.rhel = rhel_advisory(cve, stream_major(release), verbose);
    }
    s
}

/// The compose-bound stock builds of a release, `(nvr, when)` oldest
/// first: tagged into `<release>-pending-signed`.
fn compose_bound(stream: &[TagEvent], release: &str) -> Vec<(String, NaiveDateTime)> {
    let tag = format!("{release}-pending-signed");
    let mut out: Vec<(String, NaiveDateTime)> = stream
        .iter()
        .filter(|e| e.added && e.tag == tag)
        .filter_map(|e| Some((e.nvr.clone(), e.at()?)))
        .collect();
    out.sort_by_key(|(_, at)| *at);
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// When `nvr` was first tagged anywhere in `events`: built, near enough.
fn first_seen(events: &[TagEvent], nvr: &str) -> Option<NaiveDateTime> {
    events
        .iter()
        .filter(|e| e.added && e.nvr == nvr)
        .filter_map(TagEvent::at)
        .min()
}

/// The version-release of an NVR, for RPM comparison.
fn vr(nvr: &str) -> Option<String> {
    parse_nvr(nvr).map(|(_, v, r)| format!("{v}-{r}"))
}

/// `c10s` → `10`.
fn stream_major(release: &str) -> &str {
    release.trim_start_matches('c').trim_end_matches('s')
}

/// Put the sources together: the dates, the stock fix, the shadow.
pub fn assemble(issue: &gitlab::Issue, package: &str, release: &str, s: &Sources) -> Row {
    let mut row = Row {
        release: release.to_string(),
        package: package.to_string(),
        kind: issue
            .labels
            .iter()
            .find(|l| TYPE_LABELS.contains(&l.as_str()))
            .cloned(),
        reason: s.cve.clone(),
        issue_url: issue.web_url.clone(),
        issue_filed: s.issue_created.as_deref().and_then(rfc3339_day),
        issue_closed: s.issue_closed.as_deref().and_then(rfc3339_day),
        jira_key: crate::utils::parse_jira_key_from_body(
            issue.description.as_deref().unwrap_or(""),
        )
        .or_else(|| crate::file_issue::scan_rhel_key(issue.description.as_deref().unwrap_or(""))),
        jira_filed: s.jira_created.as_deref().and_then(rfc3339_day),
        jira_resolved: s.jira_resolved.as_deref().and_then(rfc3339_day),
        mr_url: issue.description.as_deref().and_then(|b| {
            parse_mr_line(b)
                .map(|(u, _)| u)
                .or_else(|| scan_mr_url_in_body(b))
        }),
        mr_filed: s.mr_created.as_deref().and_then(rfc3339_day),
        mr_merged: s.mr_merged.as_deref().and_then(rfc3339_day),
        ..Default::default()
    };
    if let Some((adv, date)) = &s.rhel {
        row.rhel_advisory = Some(adv.clone());
        row.rhel_available = Some(date.clone());
    }

    // The SIG's builds in -release, oldest first.
    let release_tag = proposed_updates_tag(release).unwrap_or_default();
    let mut sig_releases: Vec<(String, NaiveDateTime)> = s
        .cbs
        .iter()
        .filter(|e| e.added && e.tag == release_tag && e.nvr.contains("~proposed"))
        .filter(|e| parse_nvr(&e.nvr).map(|(n, _, _)| n) == Some(package))
        .filter_map(|e| Some((e.nvr.clone(), e.at()?)))
        .collect();
    sig_releases.sort_by_key(|(_, at)| *at);
    let Some((sig_build, covered_from)) = sig_releases.first().cloned() else {
        row.notes
            .push("no SIG build ever tagged into -release".to_string());
        return row;
    };
    row.sig_build = Some(sig_build.clone());
    row.sig_built = first_seen(&s.cbs, &sig_build).map(day);
    row.covered_from = Some(day(covered_from));

    // The stock fix: the build made from the evidence commit, else the
    // first compose-bound build after that commit (or after the merge).
    let bound = compose_bound(&s.stream, release);
    let fix_after: Option<NaiveDateTime> = match &s.evidence {
        Some((_, _, date)) => date.as_deref().and_then(rfc3339_to_naive),
        None => s.mr_merged.as_deref().and_then(rfc3339_to_naive),
    };
    let mut stock_fix: Option<(String, NaiveDateTime)> = None;
    if let Some((nvr, note)) = &s.recorded_fix {
        // A person's determination, recorded on the issue, outranks
        // any inference; its dates come from Stream's history when the
        // build is there.
        stock_fix = bound.iter().find(|(n, _)| n == nvr).cloned();
        row.stock_fix_by = Some(format!("recorded on the issue: {note}"));
        if stock_fix.is_none() {
            row.stock_fix = Some(nvr.clone());
            row.stock_built = first_seen(&s.stream, nvr).map(day);
            row.notes.push(format!(
                "recorded stock fix {nvr} is not compose-bound on Stream's hub"
            ));
        }
    } else if let Some((sha, token, _)) = &s.evidence {
        if let Some((nvr, _)) = s
            .stock_sources
            .iter()
            .find(|(_, src)| src.starts_with(sha) || sha.starts_with(src.as_str()))
        {
            stock_fix = bound.iter().find(|(n, _)| n == nvr).cloned();
            row.stock_fix_by = Some(format!(
                "built from stock commit {} naming {token}",
                &sha[..sha.len().min(8)]
            ));
        }
        if stock_fix.is_none()
            && let Some(after) = fix_after
            && let Some(first) = bound.iter().find(|(_, at)| *at >= after)
        {
            stock_fix = Some(first.clone());
            row.stock_fix_by = Some(format!(
                "first compose-bound stock build after commit {} naming {token}",
                &sha[..sha.len().min(8)]
            ));
        }
    } else if let Some(after) = fix_after
        && let Some(first) = bound.iter().find(|(_, at)| *at >= after)
    {
        stock_fix = Some(first.clone());
        row.stock_fix_by = Some("first compose-bound stock build after the MR merged".to_string());
    }
    if let Some((nvr, at)) = &stock_fix {
        row.stock_fix = Some(nvr.clone());
        row.stock_built = first_seen(&s.stream, nvr).map(day);
        row.stock_available = Some(day(*at));
        row.coverage_days = Some((at.date() - covered_from.date()).num_days());
        if let Some((_, rhel)) = &s.rhel
            && let Ok(r) = NaiveDate::parse_from_str(rhel, "%Y-%m-%d")
        {
            row.stream_vs_rhel_days = Some((at.date() - r).num_days());
        }
    } else if s.evidence.is_none() && s.mr_merged.is_none() && s.recorded_fix.is_none() {
        row.notes.push(
            "no stock fix found: MR not merged, no stock commit names the change; \
             `retire --reason landed --stock-fix <nvr>` records what you establish"
                .to_string(),
        );
    }

    // The shadow: a compose-bound stock build past the SIG's, tagged
    // after coverage began, that is not the fix.
    let sig_vr = vr(&sig_build).unwrap_or_default();
    let shadow = bound.iter().find(|(nvr, at)| {
        *at > covered_from
            && stock_fix.as_ref().is_none_or(|(f, _)| f != nvr)
            && vr(nvr).is_some_and(|v| compare_evr(&v, &sig_vr) == std::cmp::Ordering::Greater)
            && stock_fix.as_ref().is_none_or(|(_, fat)| *at < *fat)
    });
    if let Some((nvr, since)) = shadow {
        row.shadowed_since = Some(day(*since));
        row.notes.push(format!("shadowed by stock {nvr}"));
        // Ended by whichever came first: the SIG's next -release build,
        // the stock fix going compose-bound (the SIG build is then
        // superseded, not shadowed), or the issue closing.
        let mut ends: Vec<(NaiveDateTime, String)> = Vec::new();
        if let Some((n, at)) = sig_releases
            .iter()
            .find(|(n, at)| n != &sig_build && *at > *since)
        {
            ends.push((*at, format!("rebuild {n}")));
        }
        if let Some((n, at)) = &stock_fix {
            ends.push((*at, format!("stock fix {n} compose-bound")));
        }
        if let Some(c) = s.issue_closed.as_deref().and_then(rfc3339_to_naive) {
            ends.push((c, "retired".to_string()));
        }
        let end = ends.into_iter().min_by_key(|(at, _)| *at);
        match end {
            Some((at, by)) => {
                row.shadow_until = Some(day(at));
                row.shadow_ended_by = Some(by);
                row.shadow_days = Some((at.date() - since.date()).num_days());
            }
            None => {
                row.still_shadowed = true;
                row.shadow_days = Some((chrono::Utc::now().date_naive() - since.date()).num_days());
            }
        }
    }
    row
}

/// Red Hat's advisory for `cve` on RHEL `major`: `(advisory, day)`
/// from the security data API, which needs no account.
fn rhel_advisory(cve: &str, major: &str, verbose: bool) -> Option<(String, String)> {
    let url = format!("https://access.redhat.com/hydra/rest/securitydata/cve/{cve}.json");
    let rt = tokio::runtime::Runtime::new().ok()?;
    let text = rt.block_on(async {
        let client = sandogasa_cli::http::builder(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .ok()?;
        client.get(&url).send().await.ok()?.text().await.ok()
    })?;
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            if verbose {
                eprintln!("[timeline] {url}: {e}");
            }
            return None;
        }
    };
    rhel_release_for(&value, major)
}

/// The `(advisory, day)` for `Red Hat Enterprise Linux <major>` in a
/// security-data CVE record — the base product, not its EUS/AUS
/// streams.
pub fn rhel_release_for(record: &serde_json::Value, major: &str) -> Option<(String, String)> {
    let want = format!("Red Hat Enterprise Linux {major}");
    record
        .get("affected_release")?
        .as_array()?
        .iter()
        .filter(|r| r.get("product_name").and_then(|p| p.as_str()) == Some(want.as_str()))
        .filter_map(|r| {
            Some((
                r.get("advisory")?.as_str()?.to_string(),
                r.get("release_date")?.as_str()?.get(..10)?.to_string(),
            ))
        })
        .min_by(|a, b| a.1.cmp(&b.1))
}

fn day(t: NaiveDateTime) -> String {
    t.date().to_string()
}

fn rfc3339_to_naive(s: &str) -> Option<NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.naive_utc())
        .or_else(|| NaiveDateTime::parse_from_str(&s[..s.len().min(19)], "%Y-%m-%dT%H:%M:%S").ok())
}

fn rfc3339_day(s: &str) -> Option<String> {
    s.get(..10).map(str::to_string)
}

/// One block per change, then the spans that matter.
pub fn render(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "no tracking issues\n".to_string();
    }
    let dash = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".to_string());
    let mut out = Vec::new();
    for r in rows {
        out.push(format!(
            "{} {}{}{}: {}",
            r.package,
            r.release,
            r.kind
                .as_deref()
                .map(|k| format!(" [{k}]"))
                .unwrap_or_default(),
            r.reason
                .as_deref()
                .map(|c| format!(" {c}"))
                .unwrap_or_default(),
            r.issue_url
        ));
        out.push(format!(
            "    filed: RHEL {} ({}), MR {} (merged {}), tracking {}{}",
            dash(&r.jira_key),
            dash(&r.jira_filed),
            dash(&r.mr_filed),
            dash(&r.mr_merged),
            dash(&r.issue_filed),
            r.issue_closed
                .as_deref()
                .map(|d| format!(", closed {d}"))
                .unwrap_or_default()
        ));
        out.push(format!(
            "    SIG: {} built {}, in -release {}",
            dash(&r.sig_build),
            dash(&r.sig_built),
            dash(&r.covered_from)
        ));
        out.push(format!(
            "    stock fix: {} built {}, compose-bound {}{}",
            dash(&r.stock_fix),
            dash(&r.stock_built),
            dash(&r.stock_available),
            r.stock_fix_by
                .as_deref()
                .map(|b| format!(" ({b})"))
                .unwrap_or_default()
        ));
        out.push(format!(
            "    RHEL: {} {}",
            dash(&r.rhel_advisory),
            dash(&r.rhel_available)
        ));
        let mut spans = Vec::new();
        if let Some(d) = r.coverage_days {
            spans.push(format!("covered {d} days"));
        }
        if let Some(d) = r.stream_vs_rhel_days {
            spans.push(format!(
                "Stream {} RHEL by {} days",
                if d > 0 { "behind" } else { "ahead of" },
                d.abs()
            ));
        }
        if let Some(d) = r.shadow_days {
            spans.push(match (&r.shadow_ended_by, r.still_shadowed) {
                (_, true) => format!(
                    "SHADOWED {d} days and counting (since {})",
                    dash(&r.shadowed_since)
                ),
                (Some(by), _) => format!("shadowed {d} days until {by}"),
                (None, _) => format!("shadowed {d} days"),
            });
        }
        if !spans.is_empty() {
            out.push(format!("    → {}", spans.join("; ")));
        }
        for n in &r.notes {
            out.push(format!("    note: {n}"));
        }
        out.push(String::new());
    }
    out.join("\n")
}

/// The spreadsheet's columns, plus the spans.
pub fn render_csv(rows: &[Row]) -> String {
    let header = [
        "Reason",
        "Release",
        "Type",
        "JIRA",
        "JIRA filed",
        "MR",
        "MR filed",
        "CPU issue",
        "CPU issue filed",
        "SIG build",
        "Built",
        "Covered from",
        "MR merged",
        "Stock fix",
        "Stock built",
        "CS fix available",
        "RHEL fix",
        "RHEL fix available",
        "Delta",
        "Coverage days",
        "Shadowed since",
        "Shadow until",
        "Shadow ended by",
        "Shadow days",
        "Notes",
    ];
    let cell = |o: &Option<String>| o.clone().unwrap_or_default();
    let num = |o: Option<i64>| o.map(|n| n.to_string()).unwrap_or_default();
    let mut lines = vec![header.join(",")];
    for r in rows {
        let fields = [
            cell(&r.reason),
            r.release.clone(),
            cell(&r.kind),
            cell(&r.jira_key),
            cell(&r.jira_filed),
            cell(&r.mr_url),
            cell(&r.mr_filed),
            r.issue_url.clone(),
            cell(&r.issue_filed),
            cell(&r.sig_build),
            cell(&r.sig_built),
            cell(&r.covered_from),
            cell(&r.mr_merged),
            cell(&r.stock_fix),
            cell(&r.stock_built),
            cell(&r.stock_available),
            cell(&r.rhel_advisory),
            cell(&r.rhel_available),
            num(r.stream_vs_rhel_days),
            num(r.coverage_days),
            cell(&r.shadowed_since),
            if r.still_shadowed {
                "still".to_string()
            } else {
                cell(&r.shadow_until)
            },
            cell(&r.shadow_ended_by),
            num(r.shadow_days),
            r.notes.join("; "),
        ];
        lines.push(
            fields
                .iter()
                .map(|f| csv_cell(f))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    lines.join("\n") + "\n"
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(when: &str, nvr: &str, tag: &str) -> TagEvent {
        sandogasa_koji::parse_package_history(&format!("{when} {nvr} tagged into {tag} by x\n"))
            .remove(0)
    }

    fn issue(
        title: &str,
        body: &str,
        labels: &[&str],
        created: &str,
        closed: Option<&str>,
    ) -> gitlab::Issue {
        serde_json::from_value(serde_json::json!({
            "iid": 3, "title": title, "description": body, "state": if closed.is_some() { "closed" } else { "opened" },
            "web_url": "https://gitlab.com/CentOS/proposed_updates/rpms/PackageKit/-/issues/3",
            "labels": labels, "assignees": [], "created_at": created, "closed_at": closed
        }))
        .unwrap()
    }

    /// PackageKit c10s as it happened: the SIG built -9~proposed on
    /// 22 April and released it; hughsie's own -9 was built on 27
    /// April and went compose-bound on 7 May; RHSA-2026:19141 on 19 May.
    fn packagekit() -> (gitlab::Issue, Sources) {
        let issue = issue(
            "PackageKit: 1.2.8-8.el10 → 1.2.8-9.el10",
            "- **MR**: [Fix CVE-2026-41651](https://gitlab.com/redhat/centos-stream/rpms/PackageKit/-/merge_requests/13) — opened\n\
             - **JIRA**: [RHEL-170526](https://issues.redhat.com/browse/RHEL-170526) — New\n\
             - **Release**: c10s\n",
            &["c10s", "cpu-sig-tracker", "security"],
            "2026-04-22T22:00:00Z",
            None,
        );
        let sources = Sources {
            issue_created: issue.created_at.clone(),
            mr_created: Some("2026-04-22T20:00:00Z".into()),
            cbs: vec![
                ev(
                    "Wed Apr 22 20:14:46 2026",
                    "PackageKit-1.2.8-9~proposed.el10",
                    "proposed_updates10s-packages-main-testing",
                ),
                ev(
                    "Wed Apr 22 21:00:00 2026",
                    "PackageKit-1.2.8-9~proposed.el10",
                    "proposed_updates10s-packages-main-release",
                ),
            ],
            stream: vec![
                ev(
                    "Mon Apr 27 15:20:04 2026",
                    "PackageKit-1.2.8-9.el10",
                    "c10s-draft",
                ),
                ev(
                    "Thu May  7 11:33:13 2026",
                    "PackageKit-1.2.8-9.el10",
                    "c10s-pending-signed",
                ),
            ],
            evidence: Some((
                "99e0f170abcd".into(),
                "CVE-2026-41651".into(),
                Some("2026-04-27T14:00:00Z".into()),
            )),
            stock_sources: BTreeMap::from([(
                "PackageKit-1.2.8-9.el10".to_string(),
                "99e0f170abcd".to_string(),
            )]),
            jira_created: Some("2026-04-22T19:16:15.021+0000".into()),
            rhel: Some(("RHSA-2026:19141".into(), "2026-05-19".into())),
            cve: Some("CVE-2026-41651".into()),
            ..Default::default()
        };
        (issue, sources)
    }

    #[test]
    fn packagekit_c10s_is_covered_fifteen_days_and_stream_leads_rhel() {
        let (issue, s) = packagekit();
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.reason.as_deref(), Some("CVE-2026-41651"));
        assert_eq!(r.kind.as_deref(), Some("security"));
        assert_eq!(r.jira_filed.as_deref(), Some("2026-04-22"));
        assert_eq!(
            r.sig_build.as_deref(),
            Some("PackageKit-1.2.8-9~proposed.el10")
        );
        assert_eq!(r.covered_from.as_deref(), Some("2026-04-22"));
        assert_eq!(r.stock_fix.as_deref(), Some("PackageKit-1.2.8-9.el10"));
        assert_eq!(r.stock_built.as_deref(), Some("2026-04-27"));
        assert_eq!(r.stock_available.as_deref(), Some("2026-05-07"));
        assert!(
            r.stock_fix_by
                .as_deref()
                .unwrap()
                .starts_with("built from stock commit 99e0f170")
        );
        assert_eq!(r.coverage_days, Some(15));
        // Stream's fix was compose-bound 12 days before RHEL's advisory.
        assert_eq!(r.stream_vs_rhel_days, Some(-12));
        // The fix itself is not a shadow.
        assert_eq!(r.shadowed_since, None);
        assert!(!r.still_shadowed);
    }

    #[test]
    fn a_stock_build_past_the_sigs_without_the_fix_is_a_shadow_until_rebuild_or_retire() {
        let (issue, mut s) = packagekit();
        // A mass rebuild -9 without the fix goes compose-bound first;
        // the fix arrives as -10 later. Shadowed from the rebuild until
        // the SIG's own rebuild.
        s.stream = vec![
            ev(
                "Fri Apr 24 10:00:00 2026",
                "PackageKit-1.2.8-9.el10",
                "c10s-pending-signed",
            ),
            ev(
                "Thu May  7 11:33:13 2026",
                "PackageKit-1.2.8-10.el10",
                "c10s-pending-signed",
            ),
        ];
        s.stock_sources = BTreeMap::from([(
            "PackageKit-1.2.8-10.el10".to_string(),
            "99e0f170abcd".to_string(),
        )]);
        s.cbs.push(ev(
            "Tue Apr 28 09:00:00 2026",
            "PackageKit-1.2.8-10~proposed.el10",
            "proposed_updates10s-packages-main-release",
        ));
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.stock_fix.as_deref(), Some("PackageKit-1.2.8-10.el10"));
        assert_eq!(r.shadowed_since.as_deref(), Some("2026-04-24"));
        assert_eq!(
            r.shadow_ended_by.as_deref(),
            Some("rebuild PackageKit-1.2.8-10~proposed.el10")
        );
        assert_eq!(r.shadow_days, Some(4));
        assert!(!r.still_shadowed);
        // No rebuild: the stock fix going compose-bound ends the shadow.
        s.cbs.pop();
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(
            r.shadow_ended_by.as_deref(),
            Some("stock fix PackageKit-1.2.8-10.el10 compose-bound")
        );
        assert_eq!(r.shadow_days, Some(13));
        // No fix either, no closing: still shadowed, counting.
        s.stream.pop();
        s.stock_sources.clear();
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert!(r.still_shadowed);
        assert!(r.shadow_days.unwrap() > 100);
        // Closed instead: shadowed until retired.
        let closed = issue_with_close(&issue);
        let r = assemble(
            &closed,
            "PackageKit",
            "c10s",
            &Sources {
                issue_closed: Some("2026-05-01T00:00:00Z".into()),
                ..s
            },
        );
        assert_eq!(r.shadow_ended_by.as_deref(), Some("retired"));
        assert_eq!(r.shadow_days, Some(7));
    }

    fn issue_with_close(i: &gitlab::Issue) -> gitlab::Issue {
        issue(
            &i.title,
            i.description.as_deref().unwrap_or(""),
            &["c10s"],
            "2026-04-22T22:00:00Z",
            Some("2026-05-01T00:00:00Z"),
        )
    }

    #[test]
    fn without_evidence_the_fix_is_the_first_compose_bound_build_after_the_merge() {
        let (issue, mut s) = packagekit();
        s.evidence = None;
        s.stock_sources.clear();
        s.mr_merged = Some("2026-05-01T00:00:00Z".into());
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.stock_fix.as_deref(), Some("PackageKit-1.2.8-9.el10"));
        assert!(
            r.stock_fix_by
                .as_deref()
                .unwrap()
                .contains("after the MR merged")
        );
        s.mr_merged = None;
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.stock_fix, None);
        assert!(r.notes.iter().any(|n| n.contains("no stock fix found")));
    }

    #[test]
    fn a_stock_fix_recorded_on_the_issue_outranks_inference() {
        let (issue, mut s) = packagekit();
        s.evidence = None;
        s.stock_sources.clear();
        s.recorded_fix = Some((
            "PackageKit-1.2.8-9.el10".into(),
            "verified by salimma on 2026-09-24".into(),
        ));
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.stock_fix.as_deref(), Some("PackageKit-1.2.8-9.el10"));
        assert_eq!(r.stock_available.as_deref(), Some("2026-05-07"));
        assert_eq!(
            r.stock_fix_by.as_deref(),
            Some("recorded on the issue: verified by salimma on 2026-09-24")
        );
        assert!(r.notes.is_empty());
        // Recorded but not on Stream's hub: the name is kept, the dates
        // stay blank, and the row says so.
        s.recorded_fix = Some((
            "PackageKit-1.2.8-11.el10".into(),
            "verified by salimma".into(),
        ));
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        assert_eq!(r.stock_fix.as_deref(), Some("PackageKit-1.2.8-11.el10"));
        assert_eq!(r.stock_available, None);
        assert!(r.notes.iter().any(|n| n.contains("not compose-bound")));
    }

    #[test]
    fn the_rhel_advisory_is_the_base_products_earliest() {
        let record = serde_json::json!({"affected_release": [
            {"product_name": "Red Hat Enterprise Linux 10", "advisory": "RHSA-2026:19141", "release_date": "2026-05-19T00:00:00Z"},
            {"product_name": "Red Hat Enterprise Linux 10.0 Extended Update Support", "advisory": "RHSA-2026:19601", "release_date": "2026-05-20T00:00:00Z"},
            {"product_name": "Red Hat Enterprise Linux 8", "advisory": "RHSA-2026:11635", "release_date": "2026-04-29T00:00:00Z"}
        ]});
        assert_eq!(
            rhel_release_for(&record, "10"),
            Some(("RHSA-2026:19141".to_string(), "2026-05-19".to_string()))
        );
        assert_eq!(rhel_release_for(&record, "9"), None);
    }

    #[test]
    fn csv_has_the_spreadsheets_columns_and_quotes_what_needs_it() {
        let (issue, s) = packagekit();
        let r = assemble(&issue, "PackageKit", "c10s", &s);
        let csv = render_csv(&[r]);
        let mut lines = csv.lines();
        assert!(
            lines
                .next()
                .unwrap()
                .starts_with("Reason,Release,Type,JIRA,JIRA filed,MR,MR filed,CPU issue,")
        );
        let row = lines.next().unwrap();
        assert!(row.starts_with("CVE-2026-41651,c10s,security,RHEL-170526,2026-04-22,"));
        assert!(row.contains(",RHSA-2026:19141,2026-05-19,-12,15,"));
        assert_eq!(csv_cell("a, b"), "\"a, b\"");
    }

    #[test]
    fn the_report_reads_the_dates_and_spans_back() {
        assert_eq!(render(&[]), "no tracking issues\n");
        let (issue, s) = packagekit();
        let covered = assemble(&issue, "PackageKit", "c10s", &s);
        // A still-shadowed change with a reply owed nowhere: no fix, no
        // RHEL advisory, a note.
        let (issue2, mut s2) = packagekit();
        s2.evidence = None;
        s2.stock_sources.clear();
        s2.rhel = None;
        s2.cve = None;
        s2.stream = vec![ev(
            "Fri Apr 24 10:00:00 2026",
            "PackageKit-1.2.8-9.el10",
            "c10s-pending-signed",
        )];
        let shadowed = assemble(&issue2, "PackageKit", "c10s", &s2);
        assert!(shadowed.still_shadowed);
        // And one that never reached -release.
        let (issue3, mut s3) = packagekit();
        s3.cbs.clear();
        let unreleased = assemble(&issue3, "PackageKit", "c10s", &s3);
        let text = render(&[covered, shadowed, unreleased]);
        assert!(
            text.contains("PackageKit c10s [security] CVE-2026-41651: https://gitlab.com/"),
            "{text}"
        );
        assert!(text.contains("filed: RHEL RHEL-170526 (2026-04-22), MR 2026-04-22 (merged -), tracking 2026-04-22"), "{text}");
        assert!(
            text.contains(
                "SIG: PackageKit-1.2.8-9~proposed.el10 built 2026-04-22, in -release 2026-04-22"
            ),
            "{text}"
        );
        assert!(text.contains("stock fix: PackageKit-1.2.8-9.el10 built 2026-04-27, compose-bound 2026-05-07 (built from stock commit 99e0f170"), "{text}");
        assert!(text.contains("RHEL: RHSA-2026:19141 2026-05-19"), "{text}");
        assert!(
            text.contains("→ covered 15 days; Stream ahead of RHEL by 12 days"),
            "{text}"
        );
        assert!(
            text.contains("SHADOWED") && text.contains("and counting (since 2026-04-24)"),
            "{text}"
        );
        assert!(
            text.contains("note: shadowed by stock PackageKit-1.2.8-9.el10"),
            "{text}"
        );
        assert!(
            text.contains("note: no SIG build ever tagged into -release"),
            "{text}"
        );
        // CSV of the same rows: one line per row plus the header.
        let csv = render_csv(&[
            assemble(&issue, "PackageKit", "c10s", &s),
            assemble(&issue2, "PackageKit", "c10s", &s2),
        ]);
        assert_eq!(csv.lines().count(), 3);
        assert!(csv.lines().nth(2).unwrap().contains(",still,"), "{csv}");
    }

    #[test]
    fn timestamps_of_either_spelling_become_days() {
        assert_eq!(
            rfc3339_day("2026-04-22T19:16:15.021+0000").as_deref(),
            Some("2026-04-22")
        );
        assert_eq!(rfc3339_day("short"), None);
        assert_eq!(
            rfc3339_to_naive("2026-04-22T19:16:15Z").map(day).as_deref(),
            Some("2026-04-22")
        );
        assert_eq!(
            rfc3339_to_naive("2026-04-22T19:16:15.021+0000")
                .map(day)
                .as_deref(),
            Some("2026-04-22")
        );
        assert_eq!(rfc3339_to_naive("not a time"), None);
        assert_eq!(stream_major("c10s"), "10");
        assert_eq!(
            vr("PackageKit-1.2.8-9.el10").as_deref(),
            Some("1.2.8-9.el10")
        );
    }
}
