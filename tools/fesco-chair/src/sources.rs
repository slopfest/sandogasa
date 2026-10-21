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

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
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
/// Label on tickets with an in-ticket vote running.
pub const VOTE_LABEL: &str = "vote-in-progress";
/// Status labels a ticket is closed with, by what was decided.
pub const ACCEPTED_LABEL: &str = "Status/Accepted";
pub const REJECTED_LABEL: &str = "Status/Rejected";
/// Label on tickets under the policy's Fast Track (`+7` approves at
/// once, a reminder is due after 48 hours).
pub const FAST_TRACK_LABEL: &str = "fast track";
/// The meetbot topic FESCo meetings are recorded under
/// (`!meetingname fesco`).
pub const MEETBOT_TOPIC: &str = "fesco";
/// The agenda report URL used in the announcement (6114 is the
/// `meeting` label's id on the tracker).
pub const AGENDA_URL: &str = "https://forge.fedoraproject.org/fesco/tickets/issues?labels=6114";
/// Where the announcement is sent.
pub const ANNOUNCE_TO: &str = "devel@lists.fedoraproject.org";
/// Column both emails are wrapped to. Something has to wrap them —
/// meetbot's minutes run past 200 columns — and if we don't, the
/// sending mail client does, at its own width and with continuations
/// flush left, which loses the structure.
///
/// The width must not exceed the client's, or every line is broken a
/// second time and the result fills with one- and two-word orphans;
/// that is what 80 produced in the 2026-07-28 summary. 71 rather than
/// the conventional 72 (<https://useplaintext.email/#etiquette>):
/// that summary's archived copy contains surviving 71-column lines,
/// so 71 is known to pass through untouched, while nothing in it
/// reaches 72 despite lines clustering at 66-71. Both are within the
/// convention; this is the one with evidence.
pub const WRAP: usize = 71;

/// A tracker ticket on the agenda.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Ticket {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// The parsed in-ticket decision (voted tickets only), e.g.
    /// `APPROVED (+3, 0, 0)`; `None` falls back to the template
    /// placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    /// The repo slug for items outside the main tracker (e.g.
    /// `fesco/docs`); `None` for tracker tickets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Whether this is a pull request (drives the `!forge pr` vs
    /// `!forge issue` lookup in the meeting script).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pull: bool,
    /// Dates (`YYYY-MM-DD`) the item was opened and last changed —
    /// shown when offering docs items, so a stale one is easy to skip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
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
            pull: false,
            created: issue.created_at.as_deref().map(date_of),
            updated: issue.updated_at.as_deref().map(date_of),
        }
    }
}

/// The `YYYY-MM-DD` part of an RFC 3339 timestamp.
fn date_of(timestamp: &str) -> String {
    timestamp.chars().take(10).collect()
}

/// The announcement's ticket sections.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    sandogasa_cli::date::next_weekday(today, chrono::Weekday::Tue)
}

/// The plain-text artefact for a meetbot HTML URL: `….html` → `….txt`
/// (also turns `….log.html` into `….log.txt`).
pub use sandogasa_meetbot::txt_url;

/// Word-wrap one line to [`WRAP`], keeping its leading indent and
/// `*` bullet on the first line and aligning continuations under the
/// text — the shape meetbot's minutes rely on. Lines already within
/// the width pass through untouched, and a word longer than the width
/// (a bare URL) overflows rather than being broken, since a split URL
/// stops being a link.
pub fn wrap_line(line: &str) -> String {
    if line.chars().count() <= WRAP {
        return line.to_string();
    }
    let indent = line.len() - line.trim_start().len();
    let bullet = if line[indent..].starts_with("* ") {
        2
    } else {
        0
    };
    let (prefix, content) = line.split_at(indent + bullet);
    // Continuations align under the text; same width as the prefix, so
    // swapping it back in below keeps every line's length correct.
    let hang = " ".repeat(prefix.len());
    let wrapped = sandogasa_cli::wrap_prefixed(content, &hang, WRAP);
    format!("{prefix}{}", &wrapped[hang.len()..])
}

/// An agenda or summary entry for `ticket`: the wrapped
/// `#NNNN Title` line followed by its URL, which is left long rather
/// than broken.
pub fn entry(ticket: &Ticket) -> String {
    format!(
        "{}\n{}",
        wrap_line(&format!("{} {}", ticket.label(), ticket.title)),
        ticket.url
    )
}

/// The in-ticket decision for a pending-announcement ticket, parsed
/// from its comments (searched newest-first). The vote concludes with
/// a comment like "After a week: APPROVED (+3, 0, 0)" right before
/// the ticket is tagged; this returns the verdict-through-tally slice
/// of that line, e.g. `APPROVED (+3, 0, 0)`. The verdict is matched
/// case-insensitively ("… is approved (+6, 0, 0)" is just as common)
/// and normalized to uppercase for the announcement — the verdict
/// word alone: whatever the chair wrote between it and the tally
/// ("approved under normal ticket vote rule (+5, 1, 0)") keeps its
/// casing, since the decision is the word, not the sentence.
pub fn extract_decision<'a>(bodies_newest_first: impl Iterator<Item = &'a str>) -> Option<String> {
    for body in bodies_newest_first {
        for line in body.lines() {
            let upper = line.to_ascii_uppercase();
            for verdict in ["APPROVED", "REJECTED"] {
                let Some(start) = upper.find(verdict) else {
                    continue;
                };
                let rest = &line[start..];
                let Some(end) = rest.find(')') else { continue };
                if rest[..end].contains('(') {
                    return Some(format!("{verdict}{}", &rest[verdict.len()..=end]));
                }
            }
        }
    }
    None
}

/// Ticket numbers discussed at a meeting, from its plain-text
/// minutes: meetbot renders each `!topic` command as a
/// `* TOPIC: … (@chair, HH:MM:SS)` line. Chairs write the ticket
/// number first (`#NNNN Title`) or last (`Title #NNNN`), so every
/// `#NNNN` on a TOPIC line counts.
pub fn extract_ticket_numbers(minutes: &str) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for line in minutes.lines() {
        let Some(rest) = line.trim_start().strip_prefix("* TOPIC:") else {
            continue;
        };
        for (i, _) in rest.match_indices('#') {
            let digits: String = rest[i + 1..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(n) = digits.parse() {
                out.insert(n);
            }
        }
    }
    out
}

/// What a meeting agreed about one ticket.
///
/// Both halves are kept: the derived verdict is what gets commented,
/// and the line it came from is what a person checks it against. A
/// tool that showed only its own conclusion would be asking to be
/// trusted rather than verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingDecision {
    /// The verdict and tally, as the announcement writes it.
    pub decision: String,
    /// The `AGREED` text it was read from, verbatim.
    pub agreed: String,
}

/// Decisions a meeting reached, by ticket.
///
/// meetbot nests an `AGREED` under the `TOPIC` it was said during,
/// so a ticket's decision is the AGREED lines following its TOPIC
/// line and preceding the next one. A meeting decides a ticket by
/// agreeing something about it; an INFO or an ACTION is not a
/// decision and leaves the ticket open.
///
/// Only an agreement carrying a verdict counts, which
/// [`extract_decision`] is what decides — "AGREED: approved
/// (+6, 0, -0)" closes a ticket and "AGREED: we will revisit next
/// week" does not, though both are agreements.
pub fn extract_decisions(minutes: &str) -> BTreeMap<u64, MeetingDecision> {
    let mut out = BTreeMap::new();
    let mut current: BTreeSet<u64> = BTreeSet::new();
    for line in minutes.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("* TOPIC:") {
            current = extract_ticket_numbers(line);
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("* AGREED:") else {
            continue;
        };
        let Some(decision) = extract_decision(std::iter::once(rest)) else {
            continue;
        };
        for number in &current {
            // First decision wins: a topic revisited later in the
            // same meeting usually restates rather than overturns,
            // and a chair who did overturn one would say so in a
            // comment the tool must not silently contradict.
            out.entry(*number).or_insert_with(|| MeetingDecision {
                decision: decision.clone(),
                agreed: rest.trim().to_string(),
            });
        }
    }
    out
}

/// The agenda's section headings, as `render_body` writes them.
pub const VOTED_HEADING: &str = "Discussed and Voted in the Ticket";
pub const FOLLOWUPS_HEADING: &str = "Followups";
pub const NEW_BUSINESS_HEADING: &str = "New business";

/// Read an agenda or a summary, from a URL or a path on disk.
///
/// Anything that is not an `http://` or `https://` URL is a file.
/// A mail is often to hand as a file before it is to hand as a
/// link -- a draft, a saved `.eml`, the output of a previous run --
/// and waiting for the archive to publish it in order to act on it
/// is a delay with nothing behind it.
pub fn read_location(location: &str) -> Result<String, String> {
    let raw = if location.starts_with("http://") || location.starts_with("https://") {
        let resp = http_client()
            .get(location)
            .send()
            .map_err(|e| format!("fetching {location}: {e}"))?;
        let resp = sandogasa_cli::http::blocking_ok(resp, "mail")?;
        resp.text()
            .map_err(|e| format!("reading {location}: {e}"))?
    } else {
        std::fs::read_to_string(location).map_err(|e| format!("reading {location}: {e}"))?
    };
    Ok(plain_text(&raw))
}

/// How much of `rest` belongs to the `<div>` it starts inside.
///
/// Counted by nesting, not by the first `</div>`: HyperKitty opens a
/// `<div class="quoted-switch">` immediately inside the message body
/// of any reply, so stopping at the first close cut the 2026-10-20
/// summary down to its opening line and lost every ticket it named.
fn div_len(rest: &str) -> usize {
    let (mut depth, mut at) = (1usize, 0usize);
    while depth > 0 {
        let open = rest[at..].find("<div").map(|i| at + i);
        let Some(close) = rest[at..].find("</div>").map(|i| at + i) else {
            return rest.len();
        };
        match open {
            Some(o) if o < close => {
                depth += 1;
                at = o + "<div".len();
            }
            _ => {
                depth -= 1;
                if depth == 0 {
                    return close;
                }
                at = close + "</div>".len();
            }
        }
    }
    rest.len()
}

/// Replace each `<a>` with the URL it points at, not the text it
/// shows.
///
/// HyperKitty truncates a long URL in the visible text -- the
/// 2026-10-20 summary renders its minutes link as
/// `https://meetbot.fedoraproject.org/...2026-10-2...` -- and keeps
/// the whole one in the `href`. Reading the text would hand
/// `minutes_url` an address that 404s, so the href wins wherever it
/// is an http URL the text is a truncated form of. Any other anchor
/// keeps its text: the archive's own `<a href="#">...</a>` controls
/// are not links to anything.
fn unlink(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(at) = rest.find("<a ") {
        out.push_str(&rest[..at]);
        let Some(open_end) = rest[at..].find('>').map(|i| at + i + 1) else {
            break;
        };
        let Some(close) = rest[open_end..].find("</a>").map(|i| open_end + i) else {
            break;
        };
        let href = rest[at..open_end]
            .split_once("href=\"")
            .and_then(|(_, r)| r.split_once('"'))
            .map(|(u, _)| u);
        let text = &rest[open_end..close];
        match href {
            Some(h) if h.starts_with("http") && h.starts_with(text.trim_end_matches('.')) => {
                out.push_str(h)
            }
            _ => out.push_str(text),
        }
        rest = &rest[close + "</a>".len()..];
    }
    out.push_str(rest);
    out
}

/// The plain text of a mail, whether or not it arrived as a page.
///
/// The list archive serves no raw view -- `/message/<id>/raw/` is a
/// 404 -- so the link a person has to hand is the rendered page, and
/// that is the link they will paste. HyperKitty wraps the message in
/// `<div class="email-body">` and renders each paragraph of the mail
/// as one `<p>`, which is exactly the blank-line-separated block
/// [`parse_agenda`] reads, so the conversion is faithful rather than
/// a best effort at scraping.
///
/// Anything that does not look like a page is returned unchanged:
/// meetbot's `.txt` minutes and a mail saved to disk both arrive
/// here as plain text already.
pub fn plain_text(raw: &str) -> String {
    if !raw.contains("<html") && !raw.contains("<!DOCTYPE") {
        return raw.to_string();
    }
    // Scoped to the message when the page has one, so the archive's
    // own navigation and footer cannot contribute a block.
    const BODY: &str = "class=\"email-body";
    let scoped = match raw
        .find(BODY)
        .and_then(|at| raw[at..].find('>').map(|i| at + i + 1))
    {
        Some(start) => &raw[start..start + div_len(&raw[start..])],
        None => raw,
    };
    let broken = unlink(scoped)
        .replace("</p>", "\n\n")
        .replace("<br>", "\n")
        .replace("<br/>", "\n")
        .replace("<br />", "\n");
    let mut out = String::new();
    let mut in_tag = false;
    for c in broken.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // `&amp;` last, so a literal "&amp;quot;" is not double-decoded.
    for (from, to) in [
        ("&quot;", "\""),
        ("&#x27;", "'"),
        ("&#39;", "'"),
        ("&apos;", "'"),
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&amp;", "&"),
    ] {
        out = out.replace(from, to);
    }
    out
}

/// The meeting date an agenda announces.
///
/// Taken from the `date -d 'TZ="Europe/London" YYYY-MM-DD 18:00'`
/// line the body prints for readers converting the time, which is
/// the only place the date appears in a machine-readable form.
pub fn agenda_date(body: &str) -> Option<NaiveDate> {
    const MARK: &str = "TZ=\"Europe/London\" ";
    let at = body.find(MARK)? + MARK.len();
    NaiveDate::parse_from_str(body.get(at..at + 10)?, "%Y-%m-%d").ok()
}

/// Read an agenda body back into the sections it was rendered from.
///
/// The inverse of [`crate::agenda::render_body`], so that what was
/// announced is what drives the meeting: the tracker can move
/// between the agenda going out and the meeting starting, and a
/// topic list rebuilt from it would then disagree with the mail
/// everybody is reading.
///
/// Only the three ticket sections are read. `Council happenings`
/// and `Open Floor` are standing topics the script emits itself,
/// and the boilerplate before the first heading and after the last
/// carries no entry.
pub fn parse_agenda(body: &str) -> Sections {
    let mut blocks: Vec<(Placement, Vec<&str>)> = Vec::new();
    let mut place: Option<Placement> = None;
    let mut block: Vec<&str> = Vec::new();
    for line in body.lines().chain(std::iter::once("")) {
        let line = line.trim();
        let heading = line
            .strip_prefix("= ")
            .and_then(|h| h.strip_suffix(" ="))
            .map(str::trim);
        if heading.is_some() || line.is_empty() {
            if let (Some(p), false) = (place, block.is_empty()) {
                blocks.push((p, std::mem::take(&mut block)));
            }
            block.clear();
        }
        match heading {
            Some(VOTED_HEADING) => place = Some(Placement::Voted),
            Some(FOLLOWUPS_HEADING) => place = Some(Placement::Followup),
            Some(NEW_BUSINESS_HEADING) => place = Some(Placement::NewBusiness),
            // Any other heading ends the ticket sections.
            Some(_) => place = None,
            None if !line.is_empty() && place.is_some() => block.push(line),
            None => {}
        }
    }
    let mut sections = Sections {
        voted: Vec::new(),
        followups: Vec::new(),
        new_business: Vec::new(),
    };
    for (p, lines) in blocks {
        let Some(ticket) = parse_entry(&lines, p == Placement::Voted) else {
            continue;
        };
        match p {
            Placement::Voted => sections.voted.push(ticket),
            Placement::Followup => sections.followups.push(ticket),
            Placement::NewBusiness => sections.new_business.push(ticket),
        }
    }
    sections
}

/// One agenda entry: the wrapped `#NNNN Title` line(s), its URL, and
/// for a voted entry the decision under it.
///
/// A block with no URL is not an entry -- a chair's note added to
/// the mail by hand lands in a section like anything else -- and is
/// dropped rather than guessed at.
fn parse_entry(lines: &[&str], voted: bool) -> Option<Ticket> {
    let url_at = lines.iter().position(|l| l.starts_with("http"))?;
    // Rejoined because `wrap_line` may have split the title across
    // lines, and it wraps on whitespace with no continuation indent.
    let heading = lines[..url_at].join(" ");
    let (label, title) = heading.split_once(' ')?;
    let (repo, number) = label.split_once('#')?;
    let url = lines[url_at].to_string();
    Some(Ticket {
        number: number.parse().ok()?,
        title: title.to_string(),
        pull: url.contains("/pulls/"),
        url,
        decision: if voted {
            lines.get(url_at + 1).map(|d| d.to_string())
        } else {
            None
        },
        repo: (!repo.is_empty()).then(|| repo.to_string()),
        created: None,
        updated: None,
    })
}

/// Fail if any line of `text` ends in whitespace.
///
/// Nothing else catches this: trailing whitespace is invisible in a
/// terminal unless the line is selected. In a plain-text mail it is
/// not cosmetic either -- under [RFC 3676][] `format=flowed` a line
/// ending in a space is a *soft* break, which a receiving client may
/// join to the line after it. The agenda and the summary are mails.
///
/// [RFC 3676]: https://www.rfc-editor.org/rfc/rfc3676#section-4.1
#[cfg(test)]
pub fn assert_no_trailing_whitespace(what: &str, text: &str) {
    for (i, line) in text.lines().enumerate() {
        assert_eq!(
            line.trim_end(),
            line,
            "{what}: line {} ends in whitespace: {line:?}",
            i + 1
        );
    }
}

/// Split the open docs items into those forced onto the agenda by
/// `--docs` and the rest (offered interactively).
pub fn partition_forced(items: Vec<Ticket>, forced: &[u64]) -> (Vec<Ticket>, Vec<Ticket>) {
    items.into_iter().partition(|t| forced.contains(&t.number))
}

/// Ask a yes/no question on stderr, defaulting to **no**.
pub fn confirm_default_no(question: &str) -> Result<bool, String> {
    sandogasa_cli::confirm(question, false).map_err(|e| e.to_string())
}

/// Open tickets tagged `pending announcement` — decided by an
/// in-ticket vote and awaiting their announcement. Only *open* ones:
/// the flow is tag → announce → untag + close, so a closed ticket
/// still carrying the label is stale state from a past announcement.
pub fn pending_tickets(
    client: &sandogasa_forgejo::Client,
) -> Result<Vec<Ticket>, Box<dyn std::error::Error>> {
    let mut tickets: Vec<Ticket> = client
        .repo_issues(TRACKER_OWNER, TRACKER_REPO, "open", &[PENDING_LABEL])?
        .into_iter()
        .map(Into::into)
        .collect();
    tickets.sort_by_key(|t| t.number);
    Ok(tickets)
}

/// Fill in each ticket's decision from its comments (the vote
/// concludes with e.g. "After a week: APPROVED (+3, 0, 0)" right
/// before the ticket is tagged). Best-effort: a fetch or parse miss
/// warns and leaves the template placeholder for the chair.
///
/// A ticket that already carries a decision is left alone. `agenda`
/// sets one on a vote it has just announced, and re-reading the
/// comments would either race the post or, under `--dry-run` where
/// nothing was posted, replace a known decision with the
/// placeholder.
pub fn fill_decisions(client: &sandogasa_forgejo::Client, tickets: &mut [Ticket], verbose: bool) {
    for ticket in tickets {
        if ticket.decision.is_some() {
            continue;
        }
        if verbose {
            eprintln!("[decision] parsing #{}", ticket.number);
        }
        match client.issue_comments(TRACKER_OWNER, TRACKER_REPO, ticket.number) {
            Ok(comments) => {
                ticket.decision = extract_decision(comments.iter().rev().map(|c| c.body.as_str()));
                if ticket.decision.is_none() {
                    eprintln!(
                        "warning: no APPROVED/REJECTED tally found in #{}'s comments; \
                         fill in the DECISION line by hand",
                        ticket.number
                    );
                }
            }
            Err(e) => eprintln!(
                "warning: could not fetch #{}'s comments ({e}); fill in the \
                 DECISION line by hand",
                ticket.number
            ),
        }
    }
}

/// Open fesco/docs issues and pull requests, as agenda candidates
/// (`repo` set so they render as `fesco/docs#NN`), sorted by number
/// (issues and PRs share one number space on Forgejo).
pub fn fetch_docs_items(
    client: &sandogasa_forgejo::Client,
) -> Result<Vec<Ticket>, Box<dyn std::error::Error>> {
    let mut items: Vec<Ticket> = Vec::new();
    for (pull, batch) in [
        (
            false,
            client.repo_issues(TRACKER_OWNER, DOCS_REPO, "open", &[])?,
        ),
        (true, client.repo_pulls(TRACKER_OWNER, DOCS_REPO, "open")?),
    ] {
        for issue in batch {
            let mut ticket = Ticket::from(issue);
            ticket.repo = Some(format!("{TRACKER_OWNER}/{DOCS_REPO}"));
            ticket.pull = pull;
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
            pull: false,
            created: None,
            updated: None,
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
    fn wrap_line_keeps_bullets_and_aligns_continuations() {
        // A nested AGREED line from the 2026-07-21 minutes: the bullet
        // stays put and continuations align under the text.
        let line = "    * AGREED: FESCo asks the Change Owner to update the wiki page to \
                    ensure critical path packages are fixed before the change is merged \
                    (+6, 0, 0) (@zbyszek:fedora.im, 17:25:10)";
        let wrapped = wrap_line(line);
        let mut lines = wrapped.lines();
        assert!(lines.next().unwrap().starts_with("    * AGREED: FESCo"));
        for cont in lines {
            assert!(cont.starts_with("      "), "not aligned: {cont:?}");
            assert!(
                !cont.trim_start().starts_with('*'),
                "false bullet: {cont:?}"
            );
        }
        assert!(
            wrapped.lines().all(|l| l.chars().count() <= WRAP),
            "{wrapped}"
        );
        // No words lost or duplicated.
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            line.split_whitespace().collect::<Vec<_>>()
        );
        // Short lines, blanks and separators pass through untouched.
        for short in [
            "",
            "---------------",
            "* TOPIC: Init Process (@z, 17:01:36)",
        ] {
            assert_eq!(wrap_line(short), short);
        }
    }

    /// Shaped on a HyperKitty message page: chrome outside the
    /// body, a `quoted-switch` div opening immediately inside it,
    /// a truncated link text, and escaped entities.
    const PAGE: &str = "<!DOCTYPE html>\n<html>\n\
         <!-- https://forge.fedoraproject.org/infra/tickets/issues/10741 -->\n\
         <div class=\"email-body \">\n\
         <p>or run:\n  date -d 'TZ=&quot;Europe/London&quot; 2026-10-20 18:00'</p>\n\
         <div class=\"quoted-switch\"><a href=\"#\">...</a></div>\n\
         <p>= Followups =</p>\n\
         <p>#3693 removal of NetworkManager-{vpnc,fortisslvpn}\n\
         <a target=\"_blank\" href=\"https://forge.fedoraproject.org/fesco/tickets/issues/3693\">\
         https://forge.fedoraproject.org/fesco/tickets/i...</a></p>\n\
         </div>\n\
         <div class=\"footer\">nothing here</div>\n</html>\n";

    #[test]
    fn a_page_becomes_the_text_of_the_mail_it_shows() {
        let text = plain_text(PAGE);
        // Entities decoded, so the date line is readable again.
        assert_eq!(agenda_date(&text), NaiveDate::from_ymd_opt(2026, 10, 20));
        // The link is the href, not the truncated text it showed.
        assert!(
            text.contains("https://forge.fedoraproject.org/fesco/tickets/issues/3693\n"),
            "{text}"
        );
        assert!(!text.contains("issues/i..."), "{text}");
        // Scoped to the message: the archive's own chrome is not part
        // of the mail, and its comment names a ticket of its own.
        assert!(!text.contains("10741"), "{text}");
        assert!(!text.contains("nothing here"), "{text}");
    }

    #[test]
    fn a_quoted_switch_does_not_end_the_message_body() {
        // The reply's `quoted-switch` div opens immediately inside
        // the body; stopping at the first `</div>` lost everything
        // after it.
        let got = parse_agenda(&plain_text(PAGE));
        assert_eq!(got.followups.len(), 1, "{:?}", got.followups);
        assert_eq!(got.followups[0].number, 3693);
    }

    #[test]
    fn plain_text_leaves_plain_text_alone() {
        let minutes = "* TOPIC: #3696 A ticket (chair, 19:05:11)\n  * AGREED: x\n";
        assert_eq!(plain_text(minutes), minutes);
    }

    #[test]
    fn a_location_is_a_file_unless_it_is_a_url() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agenda.txt");
        std::fs::write(&path, "= Followups =\n").unwrap();
        assert_eq!(
            read_location(path.to_str().unwrap()).unwrap(),
            "= Followups =\n"
        );
        // A path that is not there is reported as such, naming it,
        // rather than being attempted as a URL.
        let missing = dir.path().join("nope.txt");
        let err = read_location(missing.to_str().unwrap()).expect_err("no such file");
        assert!(err.contains("nope.txt"), "{err}");
    }

    #[test]
    fn a_block_without_a_url_is_not_an_entry() {
        // A chair's note typed into the mail by hand sits in a
        // section like anything else; it is not a ticket.
        let body = "= Followups =\n\nPlease read the thread before Tuesday.\n\n\
                    #3628 A real one\nhttps://forge.example/3628\n\n\
                    = Council happenings =\n";
        let got = parse_agenda(body);
        assert_eq!(got.followups.len(), 1, "{:?}", got.followups);
        assert_eq!(got.followups[0].number, 3628);
    }

    #[test]
    fn sections_after_the_ticket_sections_are_not_read() {
        let body = "= New business =\n\n#1 One\nhttps://forge.example/1\n\n\
                    = Open Floor =\n\n#2 Two\nhttps://forge.example/2\n";
        let got = parse_agenda(body);
        assert_eq!(got.new_business.len(), 1);
        assert_eq!(got.new_business[0].number, 1);
    }

    #[test]
    #[should_panic(expected = "ends in whitespace")]
    fn the_trailing_whitespace_check_is_not_vacuous() {
        assert_no_trailing_whitespace("fixture", "fine\npadded \nalso fine");
    }

    #[test]
    fn a_rendered_entry_never_ends_a_line_in_whitespace() {
        let mut long = ticket(3628);
        long.title = "Change: libxml2 15, with a deliberately overlong title that \
                      has to be wrapped across several lines"
            .to_string();
        long.decision = Some("APPROVED (+8, 0, -0)".to_string());
        assert_no_trailing_whitespace("entry", &entry(&long));
        // The wrapper is where a stray space would come from: it
        // splits on whitespace and re-joins with an indent.
        assert_no_trailing_whitespace("wrap_line", &wrap_line(&long.title));
    }

    #[test]
    fn entry_wraps_the_title_but_never_the_url() {
        let mut long = ticket(3656);
        long.title = "[FastTrack] Proposal: gate all stable release updates on \
                      rmdepcheck and friends"
            .to_string();
        let rendered = entry(&long);
        let (title, url) = rendered.rsplit_once('\n').unwrap();
        assert!(
            title.lines().count() > 1,
            "expected a wrapped title: {title}"
        );
        assert!(title.lines().all(|l| l.chars().count() <= WRAP), "{title}");
        assert_eq!(url, long.url);
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

    #[test]
    fn extract_ticket_numbers_reads_number_last_topics() {
        // The 2026-07-21 FESCo meeting wrote topics with the ticket
        // number at the end instead of the front.
        let minutes = "\
Meeting summary
---------------
* TOPIC: Init Process (@zbyszek:fedora.im, 17:01:36)
* TOPIC: Change: libxml215 #3628 (@zbyszek:fedora.im, 17:03:55)
* TOPIC: Change: Enable Shadow Stack by Default on x86_64 #3636 (@zbyszek:fedora.im, 17:25:43)
* TOPIC: Next week's chair (@zbyszek:fedora.im, 18:24:00)
";
        let numbers = extract_ticket_numbers(minutes);
        assert_eq!(numbers, BTreeSet::from([3628, 3636]));
    }

    fn docs_item(number: u64) -> Ticket {
        Ticket {
            number,
            title: format!("Docs item {number}"),
            url: format!("{FORGE_URL}/fesco/docs/pulls/{number}"),
            decision: None,
            repo: Some("fesco/docs".to_string()),
            pull: true,
            created: None,
            updated: None,
        }
    }

    #[test]
    fn fill_decisions_leaves_a_known_decision_alone() {
        // `agenda` sets the decision on a vote it has just
        // announced. Re-reading the comments would race the post,
        // and under --dry-run -- where nothing was posted -- it put
        // the DECISION placeholder back on an approved Change.
        let mut tickets = vec![Ticket {
            number: 3691,
            title: "Change: Libical4".into(),
            url: "u".into(),
            decision: Some("APPROVED (+4, 0, -0)".into()),
            repo: None,
            pull: false,
            created: None,
            updated: None,
        }];
        // No client call is made for a ticket that already has one,
        // so an unreachable base URL is safe here and proves it.
        let client = sandogasa_forgejo::Client::new("http://127.0.0.1:1", "t").unwrap();
        fill_decisions(&client, &mut tickets, false);
        assert_eq!(tickets[0].decision.as_deref(), Some("APPROVED (+4, 0, -0)"));
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

    /// Shaped on the 2026-10-20 minutes: a topic that was agreed,
    /// one that only drew an INFO, and one whose agreement decided
    /// nothing.
    const MINUTES: &str = "\
* TOPIC: #3696 Incompatible update exception for gedit (jsmith, 19:05:11)\n\
  * AGREED: Incompatible update exception for gedit and libgedit-* \
approved (+6, 0, -0) (jsmith, 19:12:40)\n\
* TOPIC: #3688 Slow architectures (jsmith, 19:13:02)\n\
  * INFO: s390x builders gained memory and CPU weight (jsmith, 19:20:11)\n\
* TOPIC: #3693 Something else entirely (jsmith, 19:31:44)\n\
  * AGREED: we will revisit this next week (jsmith, 19:40:02)\n\
";

    #[test]
    fn extract_decisions_reads_an_agreed_under_its_topic() {
        let got = extract_decisions(MINUTES);
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), vec![3696]);
        let d = &got[&3696];
        assert_eq!(d.decision, "APPROVED (+6, 0, -0)");
        assert!(
            d.agreed.starts_with("Incompatible update exception"),
            "the verbatim line is kept for the person to check: {}",
            d.agreed
        );
    }

    #[test]
    fn an_agreement_without_a_verdict_decides_nothing() {
        // #3693's meeting agreed something, but agreeing to come
        // back to a ticket must not close it.
        assert!(!extract_decisions(MINUTES).contains_key(&3693));
    }

    #[test]
    fn an_info_line_is_not_a_decision() {
        assert!(!extract_decisions(MINUTES).contains_key(&3688));
    }

    #[test]
    fn an_agreed_before_any_topic_belongs_to_no_ticket() {
        let out = extract_decisions("  * AGREED: APPROVED (+5, 0, -0) (chair, 19:01:00)\n");
        assert!(out.is_empty());
    }

    #[test]
    fn the_first_decision_on_a_topic_wins() {
        let minutes = "\
* TOPIC: #42 A ticket (chair, 19:00:00)\n\
  * AGREED: APPROVED (+5, 0, -0) (chair, 19:05:00)\n\
  * AGREED: REJECTED (+0, 0, -5) (chair, 19:06:00)\n\
";
        assert_eq!(
            extract_decisions(minutes)[&42].decision,
            "APPROVED (+5, 0, -0)"
        );
    }

    #[test]
    fn one_agreement_can_decide_every_ticket_its_topic_names() {
        let minutes = "\
* TOPIC: #10 and #11 taken together (chair, 19:00:00)\n\
  * AGREED: APPROVED (+6, 0, -0) (chair, 19:05:00)\n\
";
        let got = extract_decisions(minutes);
        assert_eq!(got.keys().copied().collect::<Vec<_>>(), vec![10, 11]);
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
    fn extract_decision_uppercases_the_verdict_and_nothing_else() {
        // Ticket 3685, as the chair wrote it: the decision is one word;
        // the qualification between it and the tally is a sentence.
        assert_eq!(
            extract_decision(
                [
                    "the policy exemption to upgrade libxml2 to 2.15.4 in F45 has been approved \
                  under normal ticket vote rule (+5, 1, 0)"
                ]
                .into_iter()
            ),
            Some("APPROVED under normal ticket vote rule (+5, 1, 0)".to_string())
        );
    }

    #[test]
    fn extract_decision_is_case_insensitive() {
        // The real ticket-3634 shape: a lowercase verdict (with the
        // bare word "approve" earlier on the line), normalized to
        // uppercase for the announcement.
        assert_eq!(
            extract_decision(
                [
                    "After a week, @gotmax23's proposal to approve my proposal is \
                  approved (+6, 0, 0). I'll merge the docs PR."
                ]
                .into_iter()
            ),
            Some("APPROVED (+6, 0, 0)".to_string())
        );
        // Still no tally, no decision — even in lowercase.
        assert_eq!(
            extract_decision(["I think this should be approved"].into_iter()),
            None
        );
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
