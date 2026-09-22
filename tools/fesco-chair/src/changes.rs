// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `changes` subcommand — where each Change on the release's
//! Incomplete Changes Report stands, from Bugzilla rather than from
//! the report.
//!
//! One week before the beta freeze the Change Wrangler files an
//! Incomplete Changes Report on the tracker, one block per Change
//! (name, owners, tracker bug, status, latest info), and FESCo works
//! through it at the meeting, deciding per Change whether to let it
//! continue, retarget it or invoke its contingency plan. The blocks
//! are hand-maintained, so by meeting day the report and the comments
//! restating it disagree with the tracker bugs. This reads every
//! block from the ticket and its comments, keeps the latest word on
//! each, and asks Bugzilla where the bug actually is: which release
//! tracker it blocks, its status, an unanswered NEEDINFO, its last
//! change — then groups the Changes into those still needing a
//! decision, those code-complete or done, and those already
//! retargeted.

use std::collections::BTreeMap;
use std::process::ExitCode;

use chrono::{DateTime, NaiveDate, Utc};

use crate::sources::{self, MEETING_LABEL, TRACKER_OWNER, TRACKER_REPO};

/// Red Hat's Bugzilla, where the Changes Tracking component lives.
pub const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";
/// The title the Change Wrangler gives the report ticket.
pub const REPORT_TITLE: &str = "Incomplete Changes Report";

#[derive(clap::Args)]
pub struct ChangesArgs {
    /// The report ticket (default: the open meeting ticket titled
    /// "... Incomplete Changes Report").
    #[arg(long, value_name = "N")]
    pub ticket: Option<u64>,

    /// Machine-readable JSON output.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

/// A Change as the report ticket describes it — the latest block
/// mentioning its tracker bug, across the body and the comments.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Entry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki: Option<String>,
    pub owners: Vec<String>,
    pub bug: u64,
    /// `Status:` as the block states it.
    pub status: String,
    /// `Latest Info:` as the block states it.
    pub info: String,
    /// The `## heading` the block sits under in its comment, if any
    /// ("Needs review", "Needs processing").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Date (`YYYY-MM-DD`) and author of the block this came from.
    pub noted: String,
    pub noted_by: String,
}

/// Where Bugzilla puts the Change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Still open against this release: NEW or ASSIGNED on its tracker.
    Decision,
    /// MODIFIED, ON_QA, VERIFIED or CLOSED on this release's tracker —
    /// code complete (ON_QA) or done.
    Complete,
    /// Blocks the next release's tracker instead.
    Retargeted,
    /// On neither tracker.
    Elsewhere,
}

/// One Change with the ticket's word and Bugzilla's.
#[derive(Debug, serde::Serialize)]
pub struct Report {
    #[serde(flatten)]
    pub entry: Entry,
    pub state: State,
    pub bz_status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub resolution: String,
    pub last_change: DateTime<Utc>,
    /// An unanswered NEEDINFO: whom it asks and since when.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needinfo: Option<(String, Option<DateTime<Utc>>)>,
    /// Whether the status the ticket's list gives disagrees with
    /// Bugzilla's.
    pub stale: bool,
}

#[derive(serde::Serialize)]
struct ChangesJson<'a> {
    ticket: u64,
    title: &'a str,
    url: &'a str,
    release: u32,
    changes: &'a [Report],
}

/// The `[text](url)` of a markdown link, or the text alone.
fn link(s: &str) -> (String, Option<String>) {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('[')
        && let Some((text, tail)) = rest.split_once("](")
    {
        let url = tail.split([')', ']']).next().unwrap_or("").trim();
        return (text.trim().to_string(), Some(url.to_string()));
    }
    (s.to_string(), None)
}

/// The bug number in a `show_bug.cgi?id=NNNN` link; the visible
/// `#NNNN` is typed by hand and has been wrong.
fn bug_in(s: &str) -> Option<u64> {
    let rest = &s[s.find("show_bug.cgi?id=")? + "show_bug.cgi?id=".len()..];
    rest.chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// Parse the Change blocks in one piece of text (the ticket body or a
/// comment), skipping quoted lines. Each block starts at `Change:` and
/// carries the keys that follow it; a block without a tracker link is
/// dropped. `## headings` name the section for the blocks below them.
pub fn parse_blocks(text: &str, noted: &str, noted_by: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut section: Option<String> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('>') {
            continue;
        }
        if let Some(h) = line.strip_prefix("## ") {
            section = Some(h.trim().replace("**", ""));
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().replace("**", "");
        match key.trim() {
            "Change" => out.push(Entry {
                name: link(&value).0,
                wiki: link(&value).1,
                section: section.clone(),
                noted: noted.to_string(),
                noted_by: noted_by.to_string(),
                ..Entry::default()
            }),
            _ if out.is_empty() => {}
            "Tracker ID" => out.last_mut().unwrap().bug = bug_in(&value).unwrap_or(0),
            "Status" => out.last_mut().unwrap().status = value,
            "Latest Info" => out.last_mut().unwrap().info = value,
            k if k.starts_with("Owner") => {
                out.last_mut().unwrap().owners = value
                    .split_whitespace()
                    .filter_map(|w| w.strip_prefix('@'))
                    .map(str::to_string)
                    .collect();
            }
            _ => {}
        }
    }
    out.retain(|e| e.bug != 0);
    out
}

/// Fold the blocks from the body and then each comment, oldest first,
/// into one entry per tracker bug: a later block replaces the earlier
/// word on status, info and section, and fills in a name, wiki or
/// owners the earlier one lacked.
pub fn latest_entries<'a>(texts: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> Vec<Entry> {
    let mut by_bug: BTreeMap<u64, Entry> = BTreeMap::new();
    for (text, noted, noted_by) in texts {
        for entry in parse_blocks(text, noted, noted_by) {
            by_bug
                .entry(entry.bug)
                .and_modify(|e| {
                    if !entry.name.is_empty() {
                        e.name = entry.name.clone();
                    }
                    if entry.wiki.is_some() {
                        e.wiki = entry.wiki.clone();
                    }
                    if !entry.owners.is_empty() {
                        e.owners = entry.owners.clone();
                    }
                    e.status = entry.status.clone();
                    e.info = entry.info.clone();
                    e.section = entry.section.clone();
                    e.noted = entry.noted.clone();
                    e.noted_by = entry.noted_by.clone();
                })
                .or_insert(entry);
        }
    }
    by_bug.into_values().collect()
}

/// The release number in a report title such as "F45 Incomplete
/// Changes Report".
pub fn release_of(title: &str) -> Option<u32> {
    title.split_whitespace().find_map(|w| {
        w.strip_prefix('F')
            .filter(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
            .and_then(|d| d.parse().ok())
    })
}

/// Where Bugzilla puts a Change, from its tracker bug's status and the
/// release trackers it blocks.
pub fn classify(status: &str, blocks: &[u64], this: u64, next: Option<u64>) -> State {
    if blocks.contains(&this) {
        match status {
            "NEW" | "ASSIGNED" => State::Decision,
            _ => State::Complete,
        }
    } else if next.is_some_and(|n| blocks.contains(&n)) {
        State::Retargeted
    } else {
        State::Elsewhere
    }
}

/// The report ticket: `--ticket`, else the one open meeting ticket
/// whose title carries [`REPORT_TITLE`].
fn find_ticket(
    client: &sandogasa_forgejo::Client,
    args: &ChangesArgs,
) -> Result<sandogasa_forgejo::Issue, Box<dyn std::error::Error>> {
    if let Some(n) = args.ticket {
        return client.issue(TRACKER_OWNER, TRACKER_REPO, n);
    }
    let mut found: Vec<sandogasa_forgejo::Issue> = client
        .repo_issues(TRACKER_OWNER, TRACKER_REPO, "open", &[MEETING_LABEL])?
        .into_iter()
        .filter(|i| i.title.contains(REPORT_TITLE))
        .collect();
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(format!(
            "no open `{MEETING_LABEL}` ticket titled \"{REPORT_TITLE}\"; name it with --ticket N"
        )
        .into()),
        _ => Err(format!(
            "several open `{MEETING_LABEL}` tickets titled \"{REPORT_TITLE}\" ({}); pick one with --ticket N",
            found
                .iter()
                .map(|i| format!("#{}", i.number))
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into()),
    }
}

fn date_of(timestamp: Option<&str>) -> String {
    timestamp.unwrap_or("?").chars().take(10).collect()
}

/// Read the ticket, fold its blocks, and check every tracker bug.
fn assemble(
    args: &ChangesArgs,
) -> Result<(sandogasa_forgejo::Issue, u32, Vec<Report>), Box<dyn std::error::Error>> {
    let client = sources::forge_client()?;
    let issue = find_ticket(&client, args)?;
    let release = release_of(&issue.title)
        .ok_or_else(|| format!("no release number (F45) in the title of #{}", issue.number))?;
    if args.verbose {
        eprintln!("[changes] reading #{} and its comments", issue.number);
    }
    let comments = client.issue_comments(TRACKER_OWNER, TRACKER_REPO, issue.number)?;
    let body_date = date_of(issue.created_at.as_deref());
    let body = issue.body.clone().unwrap_or_default();
    let texts = std::iter::once((body.as_str(), body_date.as_str(), "report"))
        .chain(comments.iter().map(|c| {
            (
                c.body.as_str(),
                c.created_at.as_deref().unwrap_or("?"),
                c.user.as_ref().map_or("?", |u| u.login.as_str()),
            )
        }))
        .collect::<Vec<_>>();
    let mut entries = latest_entries(texts.iter().map(|(t, d, by)| (*t, *d, *by)));
    for e in &mut entries {
        e.noted = e.noted.chars().take(10).collect();
    }
    if entries.is_empty() {
        return Err(format!(
            "no Change blocks with a tracker link found in #{}",
            issue.number
        )
        .into());
    }

    let bz = sandogasa_bugzilla::BzClient::new(BUGZILLA_URL);
    let ids: Vec<u64> = entries.iter().map(|e| e.bug).collect();
    if args.verbose {
        eprintln!(
            "[changes] checking {} tracker bugs and the F{release}/F{} trackers on Bugzilla",
            ids.len(),
            release + 1
        );
    }
    let rt = tokio::runtime::Runtime::new()?;
    let (bugs, this, next) = rt.block_on(async {
        let bugs = bz.bugs(&ids).await?;
        let this = bz.bug_by_alias(&format!("F{release}Changes")).await?.id;
        let next = bz
            .bug_by_alias(&format!("F{}Changes", release + 1))
            .await
            .ok()
            .map(|b| b.id);
        Ok::<_, reqwest::Error>((bugs, this, next))
    })?;
    let bugs: BTreeMap<u64, sandogasa_bugzilla::models::Bug> =
        bugs.into_iter().map(|b| (b.id, b)).collect();
    let mut reports = Vec::new();
    for entry in entries {
        let Some(bug) = bugs.get(&entry.bug) else {
            eprintln!(
                "warning: bug #{} ({}) not returned by Bugzilla; skipped",
                entry.bug, entry.name
            );
            continue;
        };
        let needinfo = bug
            .flags
            .iter()
            .find(|f| f.name == "needinfo" && f.status == "?")
            .map(|f| (f.requestee.clone().unwrap_or_default(), f.creation_date));
        let stale = !entry.status.eq_ignore_ascii_case(&bug.status);
        reports.push(Report {
            state: classify(&bug.status, &bug.blocks, this, next),
            bz_status: bug.status.clone(),
            resolution: bug.resolution.clone(),
            last_change: bug.last_change_time,
            needinfo,
            stale,
            entry,
        });
    }
    reports.sort_by(|a, b| a.state.cmp(&b.state).then(a.entry.name.cmp(&b.entry.name)));
    Ok((issue, release, reports))
}

/// The text report: three groups, one block per Change.
pub fn render(reports: &[Report], release: u32, today: NaiveDate) -> String {
    let mut out = Vec::new();
    for (state, heading) in [
        (
            State::Decision,
            format!("Needs a decision — still open against F{release}"),
        ),
        (
            State::Complete,
            "Code complete or done — nothing to decide".to_string(),
        ),
        (State::Retargeted, format!("Retargeted to F{}", release + 1)),
        (State::Elsewhere, "On neither release tracker".to_string()),
    ] {
        let group: Vec<&Report> = reports.iter().filter(|r| r.state == state).collect();
        if group.is_empty() {
            continue;
        }
        out.push(format!("= {heading} ({}) =", group.len()));
        for r in group {
            out.push(render_one(r, today));
        }
        out.push(String::new());
    }
    out.join("\n").trim_end().to_string()
}

fn render_one(r: &Report, today: NaiveDate) -> String {
    let e = &r.entry;
    let owners = if e.owners.is_empty() {
        "-".to_string()
    } else {
        e.owners
            .iter()
            .map(|o| format!("@{o}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let mut lines = vec![format!("{} — {owners}", e.name)];
    if let Some(w) = &e.wiki {
        lines.push(format!("  {w}"));
    }
    let mut bz = format!(
        "  bz #{} {}{}, last changed {}",
        e.bug,
        r.bz_status,
        if r.resolution.is_empty() {
            String::new()
        } else {
            format!(" {}", r.resolution)
        },
        r.last_change.format("%Y-%m-%d")
    );
    if let Some((who, since)) = &r.needinfo {
        let age = since
            .map(|s| {
                format!(
                    " since {} ({} days)",
                    s.format("%Y-%m-%d"),
                    (today - s.date_naive()).num_days()
                )
            })
            .unwrap_or_default();
        bz.push_str(&format!("; NEEDINFO {who} unanswered{age}"));
    }
    lines.push(bz);
    let section = e
        .section
        .as_deref()
        .map(|s| format!(", \"{s}\""))
        .unwrap_or_default();
    let stale = if r.stale {
        format!(" [ticket says {}]", e.status)
    } else {
        String::new()
    };
    lines.push(format!(
        "  ticket ({} {}{section}){stale}: {}",
        e.noted,
        e.noted_by,
        if e.info.is_empty() { "-" } else { &e.info }
    ));
    lines.join("\n")
}

pub fn run(args: &ChangesArgs) -> ExitCode {
    let (issue, release, reports) = match assemble(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.json {
        let out = ChangesJson {
            ticket: issue.number,
            title: &issue.title,
            url: &issue.html_url,
            release,
            changes: &reports,
        };
        println!("{}", serde_json::to_string_pretty(&out).expect("serialize"));
    } else {
        println!(
            "#{} {}\n{}\n\n{}",
            issue.number,
            issue.title,
            issue.html_url,
            render(&reports, release, Utc::now().date_naive())
        );
        let stale: Vec<String> = reports
            .iter()
            .filter(|r| r.stale)
            .map(|r| {
                format!(
                    "{} (ticket says {}, bz #{} is {})",
                    r.entry.name, r.entry.status, r.entry.bug, r.bz_status
                )
            })
            .collect();
        if !stale.is_empty() {
            eprintln!(
                "\nnote: the ticket's list is behind Bugzilla on {} Change(s) — the groups \
                 above follow Bugzilla; the list is the Change Wrangler's to edit, so point \
                 them at these rather than editing the ticket:\n  {}",
                stale.len(),
                stale.join("\n  ")
            );
        }
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "Below is the current list of incomplete changes.\n\n\
Change: [mkosi-initrd](https://fedoraproject.org/wiki/Changes/mkosi-initrd)\n\
Owner:s): @zbyszek \n\
Tracker ID: [#2203221](https://bugzilla.redhat.com/show_bug.cgi?id=2203221)\n\
Status: ASSIGNED\n\
Latest Info: Deferred since F39\n\n\
Change: [Enable Shadow Stack by Default on x86_64](https://fedoraproject.org/wiki/Changes/ShadowStack)\n\
Owner:s): @submachine\n\
Tracker ID: [#25084691](https://bugzilla.redhat.com/show_bug.cgi?id=2508469)\n\
Status: ASSIGNED\n\
Latest Info: Unknown - owner NEEDINFO'd on bz for update\n\n\
Change: [No tracker](https://fedoraproject.org/wiki/Changes/None)\n\
Owner:s): @nobody\n\
Status: ASSIGNED\n";

    const COMMENT: &str = "This is for next week's meeting.\n\n\
## Needs review (on agenda for next week)\n\n\
> Change: [mkosi-initrd](https://fedoraproject.org/wiki/Changes/mkosi-initrd) Tracker ID: [#2203221](https://bugzilla.redhat.com/show_bug.cgi?id=2203221) Status: ASSIGNED\n\n\
Change: [Restrict ptrace by default](https://fedoraproject.org/wiki/Changes/Restrict_ptrace_by_default)  \n\
Owner:s): @fche @py0xc3  \n\
Tracker ID: [#2448388](https://bugzilla.redhat.com/show_bug.cgi?id=2448388)  \n\
Status: CLOSED  \n\
Latest Info: Tracker bug CLOSED. The elfutils update was pushed to f45.\n\n\
## Needs processing (**NOT** on agenda for next week)\n\n\
Change: [Enable Shadow Stack by Default on x86_64](https://fedoraproject.org/wiki/Changes/ShadowStack)  \n\
Owner:s): @submachine  \n\
Tracker ID: [#25084691](https://bugzilla.redhat.com/show_bug.cgi?id=2508469)  \n\
Status: ASSIGNED  \n\
Latest Info: **Deferred to F46**\n";

    #[test]
    fn parse_blocks_reads_the_wranglers_format_and_the_link_not_the_visible_number() {
        let entries = parse_blocks(BODY, "2026-08-31", "report");
        assert_eq!(entries.len(), 2, "block without a tracker link is dropped");
        assert_eq!(entries[0].name, "mkosi-initrd");
        assert_eq!(
            entries[0].wiki.as_deref(),
            Some("https://fedoraproject.org/wiki/Changes/mkosi-initrd")
        );
        assert_eq!(entries[0].owners, vec!["zbyszek"]);
        assert_eq!(entries[0].bug, 2203221);
        assert_eq!(entries[0].info, "Deferred since F39");
        assert_eq!(entries[1].bug, 2508469, "typed #25084691, linked 2508469");
        assert_eq!(entries[1].section, None);
    }

    #[test]
    fn parse_blocks_skips_quotes_and_tracks_sections() {
        let entries = parse_blocks(COMMENT, "2026-09-09", "gotmax23");
        assert_eq!(entries.len(), 2, "the quoted block does not count");
        assert_eq!(entries[0].owners, vec!["fche", "py0xc3"]);
        assert_eq!(
            entries[0].section.as_deref(),
            Some("Needs review (on agenda for next week)")
        );
        assert_eq!(entries[0].status, "CLOSED");
        assert_eq!(
            entries[1].section.as_deref(),
            Some("Needs processing (NOT on agenda for next week)")
        );
        assert_eq!(entries[1].info, "Deferred to F46");
    }

    #[test]
    fn latest_entries_keeps_the_last_word_per_bug() {
        let entries = latest_entries(
            [
                (BODY, "2026-08-31", "report"),
                (COMMENT, "2026-09-09", "gotmax23"),
            ]
            .into_iter(),
        );
        let bugs: Vec<u64> = entries.iter().map(|e| e.bug).collect();
        assert_eq!(bugs, vec![2203221, 2448388, 2508469]);
        let shadow = &entries[2];
        assert_eq!(shadow.info, "Deferred to F46");
        assert_eq!(
            (shadow.noted.as_str(), shadow.noted_by.as_str()),
            ("2026-09-09", "gotmax23")
        );
        assert_eq!(entries[0].noted_by, "report", "untouched by the comment");
    }

    #[test]
    fn release_and_classification() {
        assert_eq!(release_of("F45 Incomplete Changes Report"), Some(45));
        assert_eq!(release_of("Incomplete Changes"), None);
        assert_eq!(
            classify("ASSIGNED", &[2402320], 2402320, Some(2520385)),
            State::Decision
        );
        assert_eq!(
            classify("ON_QA", &[2402320], 2402320, Some(2520385)),
            State::Complete
        );
        assert_eq!(
            classify("CLOSED", &[2402320], 2402320, Some(2520385)),
            State::Complete
        );
        assert_eq!(
            classify("ASSIGNED", &[2520385], 2402320, Some(2520385)),
            State::Retargeted
        );
        assert_eq!(classify("ASSIGNED", &[], 2402320, None), State::Elsewhere);
    }

    #[test]
    fn render_groups_and_flags_stale_blocks() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let entry = |name: &str, bug, status: &str, section: Option<&str>| Entry {
            name: name.into(),
            wiki: Some(format!("https://fedoraproject.org/wiki/Changes/{name}")),
            owners: vec!["ngompa".into()],
            bug,
            status: status.into(),
            info: "Owner NEEDINFO'd".into(),
            section: section.map(str::to_string),
            noted: "2026-09-09".into(),
            noted_by: "gotmax23".into(),
        };
        let reports = vec![
            Report {
                entry: entry("Relocate", 2481848, "ASSIGNED", Some("Needs review")),
                state: State::Decision,
                bz_status: "ASSIGNED".into(),
                resolution: String::new(),
                last_change: at("2026-09-09T00:00:00Z"),
                needinfo: Some(("ngompa13".into(), Some(at("2026-08-31T10:35:42Z")))),
                stale: false,
            },
            Report {
                entry: entry("Toolchain", 2503684, "ASSIGNED", None),
                state: State::Complete,
                bz_status: "ON_QA".into(),
                resolution: String::new(),
                last_change: at("2026-09-10T00:00:00Z"),
                needinfo: None,
                stale: true,
            },
        ];
        let text = render(&reports, 45, NaiveDate::from_ymd_opt(2026, 9, 22).unwrap());
        assert!(
            text.starts_with(
                "= Needs a decision — still open against F45 (1) =\nRelocate — @ngompa\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("  bz #2481848 ASSIGNED, last changed 2026-09-09; NEEDINFO ngompa13 unanswered since 2026-08-31 (22 days)"),
            "{text}"
        );
        assert!(
            text.contains("  ticket (2026-09-09 gotmax23, \"Needs review\"): Owner NEEDINFO'd"),
            "{text}"
        );
        assert!(
            text.contains("= Code complete or done — nothing to decide (1) =\nToolchain — @ngompa"),
            "{text}"
        );
        assert!(text.contains("  bz #2503684 ON_QA, last changed 2026-09-10\n  ticket (2026-09-09 gotmax23) [ticket says ASSIGNED]: Owner NEEDINFO'd"), "{text}");
        assert!(!text.contains("Retargeted"), "empty groups are left out");
    }
}
