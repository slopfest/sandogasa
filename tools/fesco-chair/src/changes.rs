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
    #[arg(long, conflicts_with = "script")]
    pub json: bool,

    /// The chair's zodbot lines for the meeting instead of the
    /// report: a `!topic FNN Change: <name>` and `!fesco <ticket>`
    /// pair per Change that needs a decision, in the ticket's order.
    #[arg(long)]
    pub script: bool,

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
    /// Where the block stands in the ticket: the newest listing it
    /// appears in (0 for the newest text of all) and its place there.
    /// The order the room reads the ticket in.
    #[serde(skip)]
    pub order: (usize, usize),
}

/// Where Bugzilla puts the Change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Still open against this release: NEW or ASSIGNED on its tracker.
    Decision,
    /// MODIFIED on this release's tracker: testable, which is what the
    /// policy asks of a Change by the testable deadline, but not 100%
    /// code complete, which it asks by Beta Freeze (ON_QA) — so still
    /// reviewed.
    Testable,
    /// ON_QA, VERIFIED or CLOSED on this release's tracker — 100% code
    /// complete or done.
    Complete,
    /// Still open on the tracker, but the ticket records the decision
    /// already (`AGREED: …`, or a "needs processing" section): nothing
    /// for the meeting, a bug for the Change Wrangler to update.
    Decided,
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
    /// The FESCo ticket the Change was approved in, when the tracker
    /// has one titled for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_url: Option<String>,
    /// The wiki link the ticket gives when it does not resolve; the
    /// entry's `wiki` is then the tracker bug's, or nothing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki_broken: Option<String>,
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
                order: (0, out.len()),
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
/// owners the earlier one lacked. The entries come back in the
/// ticket's order — the newest listing's order first, then what only
/// older texts list, in theirs — which is the order the room reads
/// the ticket in.
pub fn latest_entries<'a>(texts: impl Iterator<Item = (&'a str, &'a str, &'a str)>) -> Vec<Entry> {
    let texts: Vec<_> = texts.collect();
    let newest = texts.len().saturating_sub(1);
    let mut by_bug: BTreeMap<u64, Entry> = BTreeMap::new();
    for (i, (text, noted, noted_by)) in texts.into_iter().enumerate() {
        for mut entry in parse_blocks(text, noted, noted_by) {
            entry.order.0 = newest - i;
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
                    e.order = entry.order;
                })
                .or_insert(entry);
        }
    }
    let mut out: Vec<Entry> = by_bug.into_values().collect();
    out.sort_by_key(|e| e.order);
    out
}

/// Whether a FESCo ticket title names this Change: `Change: <name>`
/// or `Change: <wiki slug>` in any casing and spacing ("Change:
/// RelocateRpmRepoConfigsToUsr", "Change: Grub EFI For Confidential
/// Computing"), title and Change compared with everything but letters
/// and digits removed.
pub fn ticket_matches(title: &str, name: &str, slug: Option<&str>) -> bool {
    let squash = |s: &str| {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    let t = squash(title);
    let Some(rest) = t.strip_prefix("change") else {
        return false;
    };
    let squashed_name = squash(name);
    let slug = slug.map(squash).unwrap_or_default();
    // Every word of the Change's name in the title ("Encapsule devel
    // containers" against "Change: Encapsule isolated devel
    // containers"), the short ones aside.
    // Numbers count whatever their length: "LLVM 22" is not "LLVM 23".
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| w.len() >= 3 || w.chars().all(|c| c.is_ascii_digit()))
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let all_words = !words.is_empty() && words.iter().all(|w| rest.contains(w.as_str()));
    !rest.is_empty() && (rest == squashed_name || (!slug.is_empty() && rest == slug) || all_words)
}

/// The FESCo ticket a Change was approved in, found by searching the
/// tracker for its wiki slug and its name and keeping the title that
/// names it ([`ticket_matches`]); the newest when several do.
fn find_change_ticket(
    client: &sandogasa_forgejo::Client,
    entry: &Entry,
    verbose: bool,
) -> Option<sandogasa_forgejo::Issue> {
    let slug = entry
        .wiki
        .as_deref()
        .and_then(|w| w.trim_end_matches('/').rsplit('/').next())
        .map(str::to_string);
    let mut queries: Vec<String> = Vec::new();
    if let Some(s) = &slug {
        queries.push(s.clone());
    }
    queries.push(entry.name.clone());
    let mut best: Option<sandogasa_forgejo::Issue> = None;
    for q in queries {
        let found = match client.search_repo_issues(TRACKER_OWNER, TRACKER_REPO, &q, "all") {
            Ok(f) => f,
            Err(e) => {
                if verbose {
                    eprintln!("[changes] ticket search for {q:?}: {e}");
                }
                continue;
            }
        };
        for issue in found {
            if ticket_matches(&issue.title, &entry.name, slug.as_deref())
                && best.as_ref().is_none_or(|b| issue.number > b.number)
            {
                best = Some(issue);
            }
        }
        if best.is_some() {
            break;
        }
    }
    best
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
            "MODIFIED" => State::Testable,
            _ => State::Complete,
        }
    } else if next.is_some_and(|n| blocks.contains(&n)) {
        State::Retargeted
    } else {
        State::Elsewhere
    }
}

/// Whether the ticket has already decided this Change, whatever the
/// tracker bug says: its latest line is a meeting agreement
/// (`AGREED: This Change is completed. (+5, 0, -0)`), or the block sits
/// under a section for decided items ("Needs processing (NOT on
/// agenda for next week)").
pub fn already_decided(entry: &Entry) -> bool {
    let info = entry.info.trim_start().to_ascii_lowercase();
    let section = entry.section.as_deref().unwrap_or("").to_ascii_lowercase();
    info.starts_with("agreed")
        || section.contains("needs processing")
        || section.contains("not on agenda")
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
                // A list edited in place is as new as its edit.
                c.updated_at
                    .as_deref()
                    .or(c.created_at.as_deref())
                    .unwrap_or("?"),
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
        if args.verbose {
            eprintln!("[changes] looking for {}'s FESCo ticket", entry.name);
        }
        let ticket = find_change_ticket(&client, &entry, args.verbose);
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
        let mut state = classify(&bug.status, &bug.blocks, this, next);
        if matches!(state, State::Decision | State::Testable) && already_decided(&entry) {
            state = State::Decided;
        }
        reports.push(Report {
            state,
            bz_status: bug.status.clone(),
            resolution: bug.resolution.clone(),
            last_change: bug.last_change_time,
            needinfo,
            stale,
            ticket: ticket.as_ref().map(|t| t.number),
            ticket_url: ticket.map(|t| t.html_url),
            wiki_broken: None,
            entry,
        });
    }
    // The ticket's wiki links are typed by hand and have been wrong
    // ("llvm23" for LLVM-23); the tracker bug's description names the
    // page the wrangler's tooling filed it for. A link that does not
    // resolve is replaced from there and reported for the wrangler.
    let http = sources::http_client();
    for r in &mut reports {
        let Some(link) = r.entry.wiki.clone() else {
            continue;
        };
        if resolves(&http, &link) {
            continue;
        }
        if args.verbose {
            eprintln!(
                "[changes] {} links {link}, which does not resolve",
                r.entry.name
            );
        }
        let from_bug = rt
            .block_on(bz.comments(r.entry.bug))
            .ok()
            .and_then(|cs| cs.first().and_then(|c| wiki_in(&c.text)))
            .filter(|u| resolves(&http, u));
        r.wiki_broken = Some(link);
        r.entry.wiki = from_bug;
    }
    // Grouped by where Bugzilla puts them, and within a group in the
    // ticket's order: the room follows the list, not the alphabet.
    reports.sort_by(|a, b| {
        a.state
            .cmp(&b.state)
            .then(a.entry.order.cmp(&b.entry.order))
    });
    Ok((issue, release, reports))
}

/// Whether `url` answers 2xx to a HEAD. A network failure counts as
/// resolving: a flaky line must not report the wrangler's links wrong.
fn resolves(http: &reqwest::blocking::Client, url: &str) -> bool {
    match http.head(url).send() {
        Ok(resp) => resp.status().is_success(),
        Err(_) => true,
    }
}

/// The Change page a text names — `https://fedoraproject.org/wiki/Changes/…`,
/// as a tracker bug's description does ("For more details, see: …").
pub fn wiki_in(text: &str) -> Option<String> {
    const PREFIX: &str = "https://fedoraproject.org/wiki/Changes/";
    let start = text.find(PREFIX)?;
    let url: String = text[start..]
        .chars()
        .take_while(|c| !c.is_whitespace() && !matches!(c, ')' | ']' | '>' | '"'))
        .collect();
    (url.len() > PREFIX.len()).then_some(url)
}

/// The chair's zodbot lines: one `!topic` / `!fesco` pair per Change
/// still reviewed — needing a decision, or testable but not code
/// complete — in the ticket's order, then the rest as comments so
/// nothing is missed. Copied line by line as the meeting goes.
pub fn render_script(reports: &[Report], release: u32) -> String {
    let mut out = Vec::new();
    for r in reports
        .iter()
        .filter(|r| matches!(r.state, State::Decision | State::Testable))
    {
        out.push(format!("!topic F{release} Change: {}", r.entry.name));
        match r.ticket {
            Some(n) => out.push(format!("!fesco {n}")),
            None => out.push(format!(
                "# no FESCo ticket found for {}; !link {}",
                r.entry.name,
                r.entry.wiki.as_deref().unwrap_or("(no wiki page)")
            )),
        }
    }
    for r in reports
        .iter()
        .filter(|r| !matches!(r.state, State::Decision | State::Testable))
    {
        out.push(format!(
            "# {} — {}{}",
            r.entry.name,
            match r.state {
                State::Complete => "code complete or done",
                State::Decided => "decided in the ticket; bz for the wrangler",
                State::Retargeted => "retargeted",
                State::Elsewhere => "on neither tracker",
                State::Decision | State::Testable => unreachable!(),
            },
            r.ticket
                .map(|n| format!(" (!fesco {n})"))
                .unwrap_or_default()
        ));
    }
    out.join("\n")
}

/// The report as Markdown, ready to paste into the ticket: a heading
/// per group with a list item per Change, and a closing section
/// listing the blocks whose stated status Bugzilla has moved on from,
/// for the Change Wrangler.
pub fn render(reports: &[Report], release: u32, today: NaiveDate) -> String {
    let mut out = Vec::new();
    for (state, heading) in [
        (
            State::Decision,
            format!("Needs a decision — still open against F{release}"),
        ),
        (
            State::Testable,
            "Testable but not code complete (MODIFIED) — still reviewed".to_string(),
        ),
        (
            State::Complete,
            "Code complete or done — nothing to decide".to_string(),
        ),
        (
            State::Decided,
            "Decided in the ticket, Bugzilla not yet updated — for the Change Wrangler".to_string(),
        ),
        (State::Retargeted, format!("Retargeted to F{}", release + 1)),
        (State::Elsewhere, "On neither release tracker".to_string()),
    ] {
        let group: Vec<&Report> = reports.iter().filter(|r| r.state == state).collect();
        if group.is_empty() {
            continue;
        }
        out.push(format!(
            "## {heading} ({}, in the ticket's order)\n",
            group.len()
        ));
        out.extend(group.iter().map(|r| render_one(r, today)));
        out.push(String::new());
    }
    let stale: Vec<&Report> = reports
        .iter()
        .filter(|r| r.stale || r.wiki_broken.is_some() || r.state == State::Decided)
        .collect();
    if !stale.is_empty() {
        out.push(format!(
            "## Ticket list behind Bugzilla ({}) — for the Change Wrangler\n",
            stale.len()
        ));
        for r in &stale {
            if r.state == State::Decided {
                out.push(format!(
                    "- {}: decided in the ticket ({}), bz {} still {}",
                    r.entry.name,
                    if r.entry.info.is_empty() {
                        "-"
                    } else {
                        &r.entry.info
                    },
                    bug_link(r.entry.bug),
                    r.bz_status
                ));
            }
            if r.stale {
                out.push(format!(
                    "- {}: ticket says {}, bz {} is {}",
                    r.entry.name,
                    r.entry.status,
                    bug_link(r.entry.bug),
                    r.bz_status
                ));
            }
            if let Some(bad) = &r.wiki_broken {
                out.push(match &r.entry.wiki {
                    Some(good) => format!(
                        "- {}: ticket links {bad}, which does not resolve; bz {} says {good}",
                        r.entry.name,
                        bug_link(r.entry.bug)
                    ),
                    None => format!(
                        "- {}: ticket links {bad}, which does not resolve, and bz {} names no page",
                        r.entry.name,
                        bug_link(r.entry.bug)
                    ),
                });
            }
        }
    }
    out.join("\n").trim_end().to_string()
}

fn bug_link(bug: u64) -> String {
    format!("[#{bug}]({BUGZILLA_URL}/show_bug.cgi?id={bug})")
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
    let name = match &e.wiki {
        Some(w) => format!("[{}]({w})", e.name),
        None => e.name.clone(),
    };
    let mut bz = format!(
        "  - bz {} {}{}, last changed {}",
        bug_link(e.bug),
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
    let fesco = match (r.ticket, &r.ticket_url) {
        (Some(n), Some(url)) => format!("\n  - FESCo ticket [#{n}]({url})"),
        _ => String::new(),
    };
    format!(
        "- **{name}** — {owners}\n{bz}\n  - ticket ({} {}{section}){stale}: {}{fesco}",
        e.noted,
        e.noted_by,
        if e.info.is_empty() { "-" } else { &e.info }
    )
}

pub fn run(args: &ChangesArgs) -> ExitCode {
    let (issue, release, reports) = match assemble(args) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.script {
        println!("{}", render_script(&reports, release));
        return ExitCode::SUCCESS;
    }
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
            "Incomplete Changes as of {} — Bugzilla against #{} {}\n{}\n\n{}",
            Utc::now().date_naive(),
            issue.number,
            issue.title,
            issue.html_url,
            render(&reports, release, Utc::now().date_naive())
        );
        if reports.iter().any(|r| r.stale) {
            eprintln!(
                "\nnote: the last section is for the Change Wrangler, whose list the \
                 ticket is — point them at it rather than editing the ticket"
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
        // The comment's order first (it is the newest list), then what
        // only the body lists.
        let bugs: Vec<u64> = entries.iter().map(|e| e.bug).collect();
        assert_eq!(bugs, vec![2448388, 2508469, 2203221]);
        let shadow = &entries[1];
        assert_eq!(shadow.info, "Deferred to F46");
        assert_eq!(
            (shadow.noted.as_str(), shadow.noted_by.as_str()),
            ("2026-09-09", "gotmax23")
        );
        assert_eq!(entries[2].noted_by, "report", "untouched by the comment");
    }

    #[test]
    fn release_and_classification() {
        assert_eq!(release_of("F45 Incomplete Changes Report"), Some(45));
        assert_eq!(release_of("Incomplete Changes"), None);
        assert_eq!(
            classify("ASSIGNED", &[2402320], 2402320, Some(2520385)),
            State::Decision
        );
        // Testable is not complete: the policy wants ON_QA by Beta Freeze.
        assert_eq!(
            classify("MODIFIED", &[2402320], 2402320, Some(2520385)),
            State::Testable
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
            order: (0, 0),
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
                ticket: None,
                ticket_url: None,
                wiki_broken: None,
            },
            Report {
                entry: entry("Toolchain", 2503684, "ASSIGNED", None),
                state: State::Complete,
                bz_status: "ON_QA".into(),
                resolution: String::new(),
                last_change: at("2026-09-10T00:00:00Z"),
                needinfo: None,
                stale: true,
                ticket: None,
                ticket_url: None,
                wiki_broken: None,
            },
        ];
        let text = render(&reports, 45, NaiveDate::from_ymd_opt(2026, 9, 22).unwrap());
        assert!(
            text.starts_with(
                "## Needs a decision — still open against F45 (1, in the ticket's order)\n\n\
                 - **[Relocate](https://fedoraproject.org/wiki/Changes/Relocate)** — @ngompa\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  - bz [#2481848](https://bugzilla.redhat.com/show_bug.cgi?id=2481848) ASSIGNED, \
                 last changed 2026-09-09; NEEDINFO ngompa13 unanswered since 2026-08-31 (22 days)"
            ),
            "{text}"
        );
        assert!(
            text.contains("  - ticket (2026-09-09 gotmax23, \"Needs review\"): Owner NEEDINFO'd"),
            "{text}"
        );
        assert!(
            text.contains("## Code complete or done — nothing to decide (1, in the ticket's order)\n\n- **[Toolchain]"),
            "{text}"
        );
        assert!(
            text.contains(
                "ON_QA, last changed 2026-09-10\n  - ticket (2026-09-09 gotmax23) \
                 [ticket says ASSIGNED]: Owner NEEDINFO'd"
            ),
            "{text}"
        );
        assert!(
            text.ends_with(
                "## Ticket list behind Bugzilla (1) — for the Change Wrangler\n\n\
                 - Toolchain: ticket says ASSIGNED, bz \
                 [#2503684](https://bugzilla.redhat.com/show_bug.cgi?id=2503684) is ON_QA"
            ),
            "{text}"
        );
        assert!(!text.contains("Retargeted"), "empty groups are left out");
    }

    #[test]
    fn entries_come_in_the_tickets_order_newest_listing_first() {
        // The body lists A, B, C; a later comment lists C then A. The
        // room reads the newest list: C, A, then B from the body.
        let block = |name: &str, bug: u64| {
            format!(
                "Change: [{name}](https://fedoraproject.org/wiki/Changes/{name})\nOwner:s): @x\n\
                 Tracker ID: [#{bug}](https://bugzilla.redhat.com/show_bug.cgi?id={bug})\n\
                 Status: ASSIGNED\nLatest Info: -\n\n"
            )
        };
        let body = format!("{}{}{}", block("A", 1), block("B", 2), block("C", 3));
        let comment = format!("{}{}", block("C", 3), block("A", 1));
        let entries = latest_entries(
            [
                (body.as_str(), "2026-08-31", "report"),
                (comment.as_str(), "2026-09-09", "gotmax23"),
            ]
            .into_iter(),
        );
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["C", "A", "B"]);
        assert_eq!(entries[0].noted_by, "gotmax23");
        assert_eq!(entries[2].noted_by, "report");
    }

    #[test]
    fn a_change_ticket_is_recognised_by_slug_name_or_every_word() {
        assert!(ticket_matches("Change: LLVM 23", "LLVM 23", Some("llvm23")));
        assert!(ticket_matches(
            "Change: RelocateRpmRepoConfigsToUsr",
            "Relocate RPM repository configs to /usr",
            Some("RelocateRpmRepoConfigsToUsr")
        ));
        assert!(ticket_matches(
            "Change: Grub EFI For Confidential Computing",
            "GRUB EFI for Confidential Computing",
            Some("Grub2LightForConfidentialComputing")
        ));
        // Every word of the name, in a title that says more.
        assert!(ticket_matches(
            "Change: Encapsule isolated devel containers",
            "Encapsule devel containers",
            Some("Encapsule_devel_containers")
        ));
        // Not a Change ticket, or another Change.
        assert!(!ticket_matches(
            "F45 Incomplete Changes Report",
            "LLVM 23",
            None
        ));
        assert!(!ticket_matches(
            "Change: LLVM 22",
            "LLVM 23",
            Some("llvm23")
        ));
        assert!(!ticket_matches(
            "Change: UseKmsconVTConsole",
            "LLVM 23",
            Some("llvm23")
        ));
    }

    #[test]
    fn the_script_pairs_a_topic_with_the_fesco_ticket_and_lists_the_rest() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let report = |name: &str, state: State, ticket: Option<u64>| Report {
            entry: Entry {
                name: name.into(),
                wiki: Some(format!("https://fedoraproject.org/wiki/Changes/{name}")),
                bug: 1,
                ..Entry::default()
            },
            state,
            bz_status: "ASSIGNED".into(),
            resolution: String::new(),
            last_change: at("2026-09-09T00:00:00Z"),
            needinfo: None,
            stale: false,
            ticket,
            ticket_url: ticket
                .map(|n| format!("https://forge.fedoraproject.org/fesco/tickets/issues/{n}")),
            wiki_broken: None,
        };
        let out = render_script(
            &[
                report("LLVM 23", State::Decision, Some(3629)),
                report("Encapsule devel containers", State::Decision, None),
                report("mkosi-initrd", State::Retargeted, Some(2990)),
            ],
            45,
        );
        assert_eq!(
            out,
            "!topic F45 Change: LLVM 23\n!fesco 3629\n\
             !topic F45 Change: Encapsule devel containers\n\
             # no FESCo ticket found for Encapsule devel containers; !link https://fedoraproject.org/wiki/Changes/Encapsule devel containers\n\
             # mkosi-initrd — retargeted (!fesco 2990)"
        );
    }

    #[test]
    fn a_broken_wiki_link_is_replaced_from_the_bug_and_reported() {
        assert_eq!(
            wiki_in(
                "This is a tracking bug for Change: LLVM 23\nFor more details, see: \
                 https://fedoraproject.org/wiki/Changes/LLVM-23\n\nUpdate all llvm"
            )
            .as_deref(),
            Some("https://fedoraproject.org/wiki/Changes/LLVM-23")
        );
        assert_eq!(wiki_in("no page here"), None);
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let r = Report {
            entry: Entry {
                name: "LLVM 23".into(),
                wiki: Some("https://fedoraproject.org/wiki/Changes/LLVM-23".into()),
                bug: 2499980,
                status: "ASSIGNED".into(),
                ..Entry::default()
            },
            state: State::Complete,
            bz_status: "ON_QA".into(),
            resolution: String::new(),
            last_change: at("2026-09-10T00:00:00Z"),
            needinfo: None,
            stale: true,
            ticket: Some(3629),
            ticket_url: Some("https://forge.fedoraproject.org/fesco/tickets/issues/3629".into()),
            wiki_broken: Some("https://fedoraproject.org/wiki/Changes/llvm23".into()),
        };
        let text = render(&[r], 45, NaiveDate::from_ymd_opt(2026, 9, 24).unwrap());
        assert!(text.contains("- **[LLVM 23](https://fedoraproject.org/wiki/Changes/LLVM-23)**"));
        assert!(
            text.contains(
                "- LLVM 23: ticket links https://fedoraproject.org/wiki/Changes/llvm23, which does \
             not resolve; bz [#2499980](https://bugzilla.redhat.com/show_bug.cgi?id=2499980) \
             says https://fedoraproject.org/wiki/Changes/LLVM-23"
            ),
            "{text}"
        );
    }

    #[test]
    fn a_decision_recorded_in_the_ticket_outranks_the_bugs_state() {
        // The Flatpak filter on 2026-09-22: ASSIGNED on Bugzilla, but the
        // ticket's list has it under "Needs processing (NOT on agenda for
        // next week)" with an AGREED line.
        let decided = Entry {
            name: "Filter Fedora Flatpaks".into(),
            info: "AGREED: This Change is completed. (+5, 0, -0); bug needs to be updated".into(),
            section: Some("Needs processing (NOT on agenda for next week)".into()),
            ..Entry::default()
        };
        assert!(already_decided(&decided));
        let by_section = Entry {
            info: "Owner NEEDINFO'd".into(),
            section: Some("Needs processing".into()),
            ..Entry::default()
        };
        assert!(already_decided(&by_section));
        let open = Entry {
            info: "INFO: This is only blocked by package review.".into(),
            section: Some("Needs review (on agenda for next week)".into()),
            ..Entry::default()
        };
        assert!(!already_decided(&open));
    }

    #[test]
    fn testable_and_decided_changes_have_their_own_groups() {
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        let report = |name: &str, state: State, info: &str, section: Option<&str>| Report {
            entry: Entry {
                name: name.into(),
                wiki: Some(format!("https://fedoraproject.org/wiki/Changes/{name}")),
                bug: 7,
                status: "ASSIGNED".into(),
                info: info.into(),
                section: section.map(str::to_string),
                noted: "2026-09-15".into(),
                noted_by: "gotmax23".into(),
                ..Entry::default()
            },
            state,
            bz_status: "MODIFIED".into(),
            resolution: String::new(),
            last_change: at("2026-09-22T00:00:00Z"),
            needinfo: None,
            stale: false,
            ticket: Some(3606),
            ticket_url: Some("https://forge.fedoraproject.org/fesco/tickets/issues/3606".into()),
            wiki_broken: None,
        };
        let reports = [
            report("Relocate", State::Testable, "INFO: all implemented", None),
            report(
                "Flatpaks",
                State::Decided,
                "AGREED: This Change is completed. (+5, 0, -0); bug needs to be updated",
                Some("Needs processing (NOT on agenda for next week)"),
            ),
            report("Elsewhere", State::Elsewhere, "-", None),
        ];
        let text = render(&reports, 45, NaiveDate::from_ymd_opt(2026, 9, 24).unwrap());
        assert!(text.contains("## Testable but not code complete (MODIFIED) — still reviewed (1, in the ticket's order)"), "{text}");
        assert!(text.contains("## Decided in the ticket, Bugzilla not yet updated — for the Change Wrangler (1, in the ticket's order)"), "{text}");
        assert!(
            text.contains("## On neither release tracker (1, in the ticket's order)"),
            "{text}"
        );
        assert!(text.contains("- Flatpaks: decided in the ticket (AGREED: This Change is completed. (+5, 0, -0); bug needs to be updated), bz [#7]"), "{text}");
        assert!(text.contains("  - FESCo ticket [#3606](https://forge.fedoraproject.org/fesco/tickets/issues/3606)"), "{text}");
        let script = render_script(&reports, 45);
        assert!(
            script.starts_with("!topic F45 Change: Relocate\n!fesco 3606\n"),
            "{script}"
        );
        assert!(
            script
                .contains("# Flatpaks — decided in the ticket; bz for the wrangler (!fesco 3606)"),
            "{script}"
        );
        assert!(
            script.contains("# Elsewhere — on neither tracker (!fesco 3606)"),
            "{script}"
        );
    }
}
