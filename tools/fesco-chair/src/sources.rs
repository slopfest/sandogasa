// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared plumbing for the chair workflow — the FESCo ticket tracker
//! (Forgejo) and the meetbot archive — plus the pure helpers the
//! subcommands assemble their output from.
//!
//! The conventions encoded here come from
//! <https://fedoraproject.org/wiki/FESCo_meeting_process>: agenda
//! tickets carry the `meeting` label, tickets already decided by an
//! in-ticket vote carry `pending announcement` (the "Discussed and
//! Voted in the Ticket" section), and a `meeting` ticket that already
//! appeared at a previous meeting is a followup — inferred here by
//! scanning recent meetbot minutes for its `TOPIC: #NNNN` line.

use std::collections::BTreeSet;

use chrono::{Datelike, NaiveDate};
use sandogasa_meetbot::{Meetbot, Meeting};

/// The Forgejo instance hosting the FESCo tracker.
pub const FORGE_URL: &str = "https://forge.fedoraproject.org";
/// Tracker repository owner.
pub const TRACKER_OWNER: &str = "fesco";
/// Tracker repository name.
pub const TRACKER_REPO: &str = "tickets";
/// The FESCo docs repository (issues and PRs are offered onto the
/// agenda — the wiki's pre-meeting step 3).
pub const DOCS_REPO: &str = "docs";
/// Label marking a ticket for the meeting agenda.
pub const MEETING_LABEL: &str = "meeting";
/// Label on tickets approved/rejected by an in-ticket vote, announced
/// alongside the agenda ("Discussed and Voted in the Ticket").
pub const PENDING_LABEL: &str = "pending announcement";
/// The meetbot topic FESCo meetings are recorded under
/// (`!meetingname fesco`).
pub const MEETBOT_TOPIC: &str = "fesco";
/// The agenda report URL used in the announcement (6114 is the
/// `meeting` label's id on the tracker).
pub const AGENDA_URL: &str = "https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114";
/// Where the announcement is sent.
pub const ANNOUNCE_TO: &str = "devel@lists.fedoraproject.org";

/// A tracker ticket on the agenda.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Ticket {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The parsed in-ticket decision (voted tickets only), e.g.
    /// `APPROVED (+3, 0, 0)`; `None` falls back to the template
    /// placeholder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The repo slug for items outside the main tracker (e.g.
    /// `fesco/docs`); `None` for tracker tickets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

impl Ticket {
    /// The number as displayed in agenda entries and topics: `#3623`
    /// for tracker tickets, `fesco/docs#28` for docs items.
    pub fn label(&self) -> String {
        match &self.repo {
            Some(repo) => format!("{repo}#{}", self.number),
            None => format!("#{}", self.number),
        }
    }
}

impl From<sandogasa_forgejo::Issue> for Ticket {
    fn from(issue: sandogasa_forgejo::Issue) -> Self {
        // The per-repo endpoints populate html_url; compose it from
        // the tracker location if a response ever omits it.
        let url = if issue.html_url.is_empty() {
            format!(
                "{FORGE_URL}/{TRACKER_OWNER}/{TRACKER_REPO}/issues/{}",
                issue.number
            )
        } else {
            issue.html_url.clone()
        };
        Ticket {
            number: issue.number,
            title: issue.title,
            url,
            decision: None,
            repo: None,
        }
    }
}

/// The announcement's ticket sections.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Sections {
    /// "Discussed and Voted in the Ticket" — announced, not discussed.
    pub voted: Vec<Ticket>,
    /// Tickets already discussed at a previous meeting.
    pub followups: Vec<Ticket>,
    /// First-time agenda tickets.
    pub new_business: Vec<Ticket>,
}

impl Sections {
    /// The tickets discussed live, in meeting order (followups first).
    pub fn discussion(&self) -> Vec<&Ticket> {
        self.followups.iter().chain(&self.new_business).collect()
    }
}

/// Where a ticket is forced by an override flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    Voted,
    Followup,
    NewBusiness,
}

/// Distribute tickets into the announcement's sections. `voted` is
/// the pending-announcement pool, `meeting` the meeting-labeled pool;
/// a meeting ticket whose number appears in `past` (discussed at a
/// previous meeting) is a followup, the rest are new business. The
/// `force_*` lists override the inference per ticket (first match
/// wins in voted → followup → new order); every section ends up
/// sorted by ticket number.
pub fn split_sections(
    voted: Vec<Ticket>,
    meeting: Vec<Ticket>,
    past: &BTreeSet<u64>,
    force_voted: &[u64],
    force_followup: &[u64],
    force_new: &[u64],
) -> Sections {
    let forced = |n: u64| -> Option<Placement> {
        if force_voted.contains(&n) {
            Some(Placement::Voted)
        } else if force_followup.contains(&n) {
            Some(Placement::Followup)
        } else if force_new.contains(&n) {
            Some(Placement::NewBusiness)
        } else {
            None
        }
    };
    let mut sections = Sections::default();
    let mut seen = BTreeSet::new();
    let mut place = |ticket: Ticket, default: Placement| {
        if !seen.insert(ticket.number) {
            return;
        }
        match forced(ticket.number).unwrap_or(default) {
            Placement::Voted => sections.voted.push(ticket),
            Placement::Followup => sections.followups.push(ticket),
            Placement::NewBusiness => sections.new_business.push(ticket),
        }
    };
    for ticket in voted {
        place(ticket, Placement::Voted);
    }
    for ticket in meeting {
        let default = if past.contains(&ticket.number) {
            Placement::Followup
        } else {
            Placement::NewBusiness
        };
        place(ticket, default);
    }
    sections.voted.sort_by_key(|t| t.number);
    sections.followups.sort_by_key(|t| t.number);
    sections.new_business.sort_by_key(|t| t.number);
    sections
}

/// The date of "Tuesday's meeting": today when run on a Tuesday, else
/// the coming Tuesday.
pub fn next_tuesday(today: NaiveDate) -> NaiveDate {
    // Mon=0 … Sun=6; Tuesday is 1.
    let weekday = today.weekday().num_days_from_monday() as i64;
    let ahead = (1 - weekday).rem_euclid(7);
    today + chrono::Duration::days(ahead)
}

/// The plain-text artefact for a meetbot HTML URL: `….html` → `….txt`
/// (also turns `….log.html` into `….log.txt`).
pub fn txt_url(html_url: &str) -> String {
    match html_url.strip_suffix(".html") {
        Some(base) => format!("{base}.txt"),
        None => html_url.to_string(),
    }
}

/// The in-ticket decision for a pending-announcement ticket, parsed
/// from its comments (searched newest-first). The vote concludes with
/// a comment like "After a week: APPROVED (+3, 0, 0)" right before
/// the ticket is tagged; this returns the verdict-through-tally slice
/// of that line, e.g. `APPROVED (+3, 0, 0)`.
pub fn extract_decision<'a>(bodies_newest_first: impl Iterator<Item = &'a str>) -> Option<String> {
    for body in bodies_newest_first {
        for line in body.lines() {
            for verdict in ["APPROVED", "REJECTED"] {
                let Some(start) = line.find(verdict) else {
                    continue;
                };
                let rest = &line[start..];
                let Some(end) = rest.find(')') else { continue };
                if rest[..end].contains('(') {
                    return Some(rest[..=end].to_string());
                }
            }
        }
    }
    None
}

/// Ticket numbers discussed at a meeting, from its plain-text
/// minutes: meetbot renders each `!topic #NNNN Title` command as a
/// `* TOPIC: #NNNN Title (@chair, HH:MM:SS)` line.
pub fn extract_ticket_numbers(minutes: &str) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for line in minutes.lines() {
        let Some(rest) = line.trim_start().strip_prefix("* TOPIC:") else {
            continue;
        };
        let Some(num) = rest.trim_start().strip_prefix('#') else {
            continue;
        };
        let digits: String = num.chars().take_while(char::is_ascii_digit).collect();
        if let Ok(n) = digits.parse() {
            out.insert(n);
        }
    }
    out
}

/// Split the open docs items into those forced onto the agenda by
/// `--docs` and the rest (offered interactively).
pub fn partition_forced(items: Vec<Ticket>, forced: &[u64]) -> (Vec<Ticket>, Vec<Ticket>) {
    items.into_iter().partition(|t| forced.contains(&t.number))
}

/// Ask a yes/no question on stderr, defaulting to **no**.
pub fn confirm_default_no(question: &str) -> Result<bool, String> {
    use std::io::{BufRead, Write};
    eprint!("{question} [y/N]: ");
    std::io::stderr().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
}

/// Open fesco/docs issues and pull requests, as agenda candidates
/// (`repo` set so they render as `fesco/docs#NN`), sorted by number
/// (issues and PRs share one number space on Forgejo).
pub fn fetch_docs_items(
    client: &sandogasa_forgejo::Client,
) -> Result<Vec<Ticket>, Box<dyn std::error::Error>> {
    let mut items: Vec<Ticket> = Vec::new();
    for batch in [
        client.repo_issues(TRACKER_OWNER, DOCS_REPO, "open", &[])?,
        client.repo_pulls(TRACKER_OWNER, DOCS_REPO, "open")?,
    ] {
        for issue in batch {
            let mut ticket = Ticket::from(issue);
            ticket.repo = Some(format!("{TRACKER_OWNER}/{DOCS_REPO}"));
            items.push(ticket);
        }
    }
    items.sort_by_key(|t| t.number);
    items.dedup_by_key(|t| t.number);
    Ok(items)
}

/// Forgejo API token lookup: the env vars first (matching
/// sandogasa-report's convention — instance-specific, then generic),
/// then the token stored by `fesco-chair config`. A token is required —
/// the chair workflow uses an authenticated client (future versions
/// will also update tickets).
pub fn forge_token() -> Result<String, String> {
    const HOST_VAR: &str = "FORGEJO_TOKEN_FORGE_FEDORAPROJECT_ORG";
    for var in [HOST_VAR, "FORGEJO_TOKEN"] {
        if let Ok(token) = std::env::var(var)
            && !token.is_empty()
        {
            return Ok(token);
        }
    }
    if let Some(token) = crate::config::stored_token() {
        return Ok(token);
    }
    Err(format!(
        "no Forgejo token: run `fesco-chair config` to store one, or set \
         {HOST_VAR} / FORGEJO_TOKEN (create the token at \
         {FORGE_URL}/user/settings/applications)"
    ))
}

/// An authenticated client for the FESCo tracker's Forgejo instance.
pub fn forge_client() -> Result<sandogasa_forgejo::Client, Box<dyn std::error::Error>> {
    sandogasa_forgejo::Client::new(FORGE_URL, &forge_token()?)
}

/// A plain HTTP client for fetching meetbot artefacts.
pub fn http_client() -> reqwest::blocking::Client {
    sandogasa_cli::install_crypto_provider();
    reqwest::blocking::Client::builder()
        .user_agent(concat!("fesco-chair/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .expect("build reqwest client")
}

/// Fetch a text artefact, surfacing HTTP errors with the URL.
pub fn fetch_text(http: &reqwest::blocking::Client, url: &str) -> Result<String, String> {
    let resp = http
        .get(url)
        .send()
        .map_err(|e| format!("GET {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url}: HTTP {}", resp.status()));
    }
    resp.text().map_err(|e| format!("GET {url}: {e}"))
}

/// Ticket numbers discussed at the most recent `history` FESCo
/// meetings before `date` — the followup signal. A single meeting's
/// minutes failing to fetch degrades to a warning (partial inference
/// beats aborting the agenda).
pub fn past_ticket_numbers(
    meetbot: &Meetbot,
    http: &reqwest::blocking::Client,
    before: NaiveDate,
    history: usize,
    verbose: bool,
) -> Result<BTreeSet<u64>, Box<dyn std::error::Error>> {
    let meetings = meetbot.search(MEETBOT_TOPIC)?;
    // search() sorts ascending by datetime; keep the exact topic only
    // (the search itself matches substrings).
    let recent: Vec<Meeting> = meetings
        .into_iter()
        .filter(|m| m.topic == MEETBOT_TOPIC && m.datetime.date() < before)
        .collect();
    let start = recent.len().saturating_sub(history);
    let mut out = BTreeSet::new();
    for meeting in &recent[start..] {
        let url = txt_url(&meeting.summary_url);
        if verbose {
            eprintln!("[followups] scanning {} ({url})", meeting.datetime.date());
        }
        match fetch_text(http, &url) {
            Ok(text) => out.extend(extract_ticket_numbers(&text)),
            Err(e) => eprintln!("warning: skipping {}: {e}", meeting.datetime.date()),
        }
    }
    Ok(out)
}

/// The FESCo meeting recorded on `date` (the newest when several).
pub fn find_meeting(meetbot: &Meetbot, date: NaiveDate) -> Result<Meeting, String> {
    let meetings = meetbot
        .search(MEETBOT_TOPIC)
        .map_err(|e| format!("meetbot search failed: {e}"))?;
    meetings
        .into_iter()
        .rfind(|m| m.topic == MEETBOT_TOPIC && m.datetime.date() == date)
        .ok_or_else(|| {
            format!(
                "no '{MEETBOT_TOPIC}' meeting on meetbot for {date} — meetings \
                 appear right after !endmeeting; check \
                 https://meetbot.fedoraproject.org/"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(number: u64) -> Ticket {
        Ticket {
            number,
            title: format!("Ticket {number}"),
            url: format!("{FORGE_URL}/fesco/tickets/issues/{number}"),
            decision: None,
            repo: None,
        }
    }

    #[test]
    fn next_tuesday_from_each_weekday() {
        // 2026-07-06 is a Monday, 2026-07-07 a Tuesday.
        let tue = NaiveDate::from_ymd_opt(2026, 7, 7).unwrap();
        assert_eq!(
            next_tuesday(NaiveDate::from_ymd_opt(2026, 7, 6).unwrap()),
            tue
        );
        // On the meeting day itself, "Tuesday's meeting" is today.
        assert_eq!(next_tuesday(tue), tue);
        // Wednesday rolls over to next week.
        assert_eq!(
            next_tuesday(NaiveDate::from_ymd_opt(2026, 7, 8).unwrap()),
            NaiveDate::from_ymd_opt(2026, 7, 14).unwrap()
        );
        // Sunday is two days out.
        assert_eq!(
            next_tuesday(NaiveDate::from_ymd_opt(2026, 7, 5).unwrap()),
            tue
        );
    }

    #[test]
    fn txt_url_swaps_extension() {
        assert_eq!(
            txt_url("https://meetbot.fedoraproject.org/m/2026-06-30/fesco.2026-06-30-17.00.html"),
            "https://meetbot.fedoraproject.org/m/2026-06-30/fesco.2026-06-30-17.00.txt"
        );
        assert_eq!(
            txt_url("https://m/x/fesco.2026-06-30-17.00.log.html"),
            "https://m/x/fesco.2026-06-30-17.00.log.txt"
        );
        // Unrecognized shape passes through unchanged.
        assert_eq!(txt_url("https://m/x/foo.txt"), "https://m/x/foo.txt");
    }

    #[test]
    fn extract_ticket_numbers_reads_topic_lines() {
        // The real meetbot .txt shape (2026-06-30 FESCo meeting).
        let minutes = "\
Meeting summary
---------------
* TOPIC: Init Process (@gotmax23:fedora.im, 17:01:28)
    * INFO: PSA: subscribe to notifications (@gotmax23:fedora.im, 17:05:57)
* TOPIC: #3620 Selection of the Fedora Council Engineering Rep  (@gotmax23:fedora.im, 17:06:03)
    * LINK: https://forge.fedoraproject.org/fesco/tickets/issues/3620 (@x, 17:08:29)
* TOPIC: #3623 Planning for the Forgejo distgit migration (@gotmax23:fedora.im, 17:15:32)
* TOPIC: Next week's chair (@gotmax23:fedora.im, 17:27:53)
* TOPIC: Open Floor (@gotmax23:fedora.im, 17:28:57)
";
        let numbers = extract_ticket_numbers(minutes);
        assert_eq!(numbers, BTreeSet::from([3620, 3623]));
    }

    fn docs_item(number: u64) -> Ticket {
        Ticket {
            number,
            title: format!("Docs item {number}"),
            url: format!("{FORGE_URL}/fesco/docs/pulls/{number}"),
            decision: None,
            repo: Some("fesco/docs".to_string()),
        }
    }

    #[test]
    fn label_distinguishes_tracker_and_docs() {
        assert_eq!(ticket(3623).label(), "#3623");
        assert_eq!(docs_item(28).label(), "fesco/docs#28");
    }

    #[test]
    fn partition_forced_splits_by_number() {
        let (selected, rest) = partition_forced(vec![docs_item(28), docs_item(31)], &[31]);
        assert_eq!(selected, vec![docs_item(31)]);
        assert_eq!(rest, vec![docs_item(28)]);
        let (selected, rest) = partition_forced(vec![docs_item(28)], &[]);
        assert!(selected.is_empty());
        assert_eq!(rest.len(), 1);
    }

    #[test]
    fn extract_decision_finds_verdict_and_tally() {
        // The real ticket-3616 shape: votes, then the concluding
        // tally, then metadata comments — searched newest-first.
        let comments = [
            "**Metadata Update from @zbyszek**:\n- Issue tagged with: pending announcement",
            "After a week: APPROVED (+3, 0, 0)\n",
            "+1",
        ];
        assert_eq!(
            extract_decision(comments.iter().copied()),
            Some("APPROVED (+3, 0, 0)".to_string())
        );
        assert_eq!(
            extract_decision(["REJECTED (+1, 0, -5)"].into_iter()),
            Some("REJECTED (+1, 0, -5)".to_string())
        );
        // A verdict word without a tally doesn't count...
        assert_eq!(
            extract_decision(["this should be APPROVED soon"].into_iter()),
            None
        );
        // ...and no comments means no decision.
        assert_eq!(extract_decision(std::iter::empty()), None);
    }

    #[test]
    fn split_sections_infers_and_dedups() {
        let past = BTreeSet::from([3620]);
        let sections = split_sections(
            vec![ticket(3610)],
            // 3610 also carries the meeting label — announced, not
            // discussed, so the voted pool wins the dedup.
            vec![ticket(3620), ticket(3623), ticket(3610)],
            &past,
            &[],
            &[],
            &[],
        );
        assert_eq!(sections.voted, vec![ticket(3610)]);
        assert_eq!(sections.followups, vec![ticket(3620)]);
        assert_eq!(sections.new_business, vec![ticket(3623)]);
        let discussion: Vec<u64> = sections.discussion().iter().map(|t| t.number).collect();
        assert_eq!(discussion, vec![3620, 3623]);
    }

    #[test]
    fn split_sections_overrides_win() {
        let past = BTreeSet::from([3620]);
        let sections = split_sections(
            vec![],
            vec![ticket(3620), ticket(3623)],
            &past,
            // Force the inferred followup into New business and the
            // inferred new-business ticket into Followups.
            &[],
            &[3623],
            &[3620],
        );
        assert_eq!(sections.followups, vec![ticket(3623)]);
        assert_eq!(sections.new_business, vec![ticket(3620)]);
    }

    #[test]
    fn ticket_from_issue_composes_url_when_missing() {
        let issue: sandogasa_forgejo::Issue =
            serde_json::from_str(r#"{"number": 3620, "title": "T", "state": "open"}"#).unwrap();
        assert_eq!(
            Ticket::from(issue).url,
            "https://forge.fedoraproject.org/fesco/tickets/issues/3620"
        );
    }
}
