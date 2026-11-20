// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Debian bug tracker (debbugs) reporting: the bugs a person filed in
//! the window, and those they wrote to in it, from bugs.debian.org's
//! SOAP interface, by the profile's Debian email addresses.
//!
//! `get_bugs` finds bugs by submitter or correspondent, and
//! `get_status` reads many bugs' titles and dates in one call. A
//! correspondent search says only that a person wrote to a bug at some
//! time, so for bugs with activity in the window the bug's log is read
//! (a few at a time) to find a message of theirs dated in it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, NaiveDate};
use quick_xml::Reader;
use quick_xml::events::{BytesRef, Event};
use serde::Serialize;

use crate::forge::{self, stat};

/// Where the Debian bug tracker answers.
pub const DEFAULT_INSTANCE: &str = "https://bugs.debian.org";

/// How many bug logs are read at once.
const PARALLEL: usize = 8;

/// How many bugs one `get_status` call asks about.
const STATUS_BATCH: usize = 200;

/// A person's Debian bug tracker activity in a window.
#[derive(Debug, Default, Serialize)]
pub struct BtsReport {
    pub instance: String,
    /// Bugs filed in the window.
    pub filed: Vec<BugRef>,
    /// Bugs written to in the window, not counting those filed in it.
    pub commented: Vec<BugRef>,
}

/// A bug for the report's lists.
#[derive(Debug, Clone, Serialize)]
pub struct BugRef {
    pub number: u64,
    pub package: String,
    pub title: String,
    pub url: String,
    /// Whether it is done (closed), now.
    pub done: bool,
}

/// One bug's state, as `get_status` gives it.
#[derive(Debug, Clone, PartialEq)]
struct Status {
    number: u64,
    package: String,
    title: String,
    /// When it was filed, and last changed (Unix times).
    filed: i64,
    modified: i64,
    done: bool,
}

/// An element of a SOAP answer: its local name, text and children.
#[derive(Debug, Default)]
struct Node {
    name: String,
    text: String,
    children: Vec<Node>,
}

impl Node {
    fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    /// The first element named `name`, this one or below.
    fn find(&self, name: &str) -> Option<&Node> {
        if self.name == name {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(name))
    }
}

/// Build the report: bugs filed and bugs written to in the window, by
/// any of `emails`.
pub fn bts_report(
    instance: &str,
    emails: &[String],
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BtsReport, String> {
    let base = instance.trim_end_matches('/');
    let http = sandogasa_cli::http::blocking_builder("sandogasa-report")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))?;
    eprintln!(
        "Reading the Debian bugs of {} address(es) at {base}...",
        emails.len()
    );
    let mut filed_ids = BTreeSet::new();
    let mut written_ids = BTreeSet::new();
    for email in emails {
        if verbose {
            eprintln!("[debbugs] {base}: get_bugs by submitter and by correspondent, {email}");
        }
        filed_ids.extend(get_bugs(&http, base, "submitter", email)?);
        written_ids.extend(get_bugs(&http, base, "correspondent", email)?);
    }
    let all: Vec<u64> = filed_ids.union(&written_ids).copied().collect();
    let statuses = get_status(&http, base, &all, verbose)?;
    let in_window = |t: i64| {
        DateTime::from_timestamp(t, 0).is_some_and(|d| (since..=until).contains(&d.date_naive()))
    };
    let bug_ref = |s: &Status| BugRef {
        number: s.number,
        package: s.package.clone(),
        title: s.title.clone(),
        url: format!("{base}/{}", s.number),
        done: s.done,
    };
    let filed: Vec<&Status> = statuses
        .iter()
        .filter(|s| filed_ids.contains(&s.number) && in_window(s.filed))
        .collect();
    let filed_now: BTreeSet<u64> = filed.iter().map(|s| s.number).collect();
    // Written to at some time, changed in the window, and not filed in
    // it: whether a message of theirs is in the window, the log says.
    let candidates: Vec<&Status> = statuses
        .iter()
        .filter(|s| written_ids.contains(&s.number) && !filed_now.contains(&s.number))
        .filter(|s| {
            in_window(s.modified)
                || DateTime::from_timestamp(s.modified, 0).is_some_and(|d| d.date_naive() > until)
        })
        .collect();
    if !candidates.is_empty() {
        eprintln!(
            "Reading {} bug log(s) for messages in the period...",
            candidates.len()
        );
    }
    let wrote = forge::in_parallel(&candidates, PARALLEL, |s| {
        message_dates(&http, base, s.number, emails, verbose)
            .map(|dates| dates.into_iter().any(in_window))
    });
    let mut commented = Vec::new();
    let mut unread = 0;
    for (s, wrote) in candidates.iter().zip(wrote) {
        match wrote {
            Ok(true) => commented.push(bug_ref(s)),
            Ok(false) => {}
            Err(_) => unread += 1,
        }
    }
    if unread > 0 {
        eprintln!("warning: cannot read the logs of {unread} bug(s) at {base}; left out");
    }
    Ok(BtsReport {
        instance: base.to_string(),
        filed: filed.into_iter().map(bug_ref).collect(),
        commented,
    })
}

/// Format the section as Markdown.
pub fn format_markdown(report: &BtsReport, detail: u8) -> String {
    let heading = format!(
        "### Debian BTS ({})\n\n",
        forge::instance_host(&report.instance)
    );
    let mut stats = String::new();
    stat(&mut stats, "Bugs filed", report.filed.len());
    stat(&mut stats, "Bugs commented on", report.commented.len());
    if stats.is_empty() {
        return format!("{heading}No Debian bug tracker activity.\n\n");
    }
    let mut out = heading;
    out.push_str(&stats);
    out.push('\n');
    if detail == 0 {
        return out;
    }
    let list = |out: &mut String, heading: &str, bugs: &[BugRef]| {
        if bugs.is_empty() {
            return;
        }
        out.push_str(&format!("#### {heading}\n\n"));
        if detail >= 2 {
            for b in bugs {
                let done = if b.done { " (done)" } else { "" };
                // As the forges write `repo#N`; the title usually opens
                // with the package already.
                out.push_str(&format!(
                    "- [{}#{}]({}) {}{done}\n",
                    b.package, b.number, b.url, b.title
                ));
            }
            out.push('\n');
        } else {
            forge::write_counts_by(out, bugs, |b| b.package.as_str());
        }
    };
    list(&mut out, "Bugs filed", &report.filed);
    list(&mut out, "Bugs commented on", &report.commented);
    out
}

/// A tag's local name, its namespace prefix dropped.
fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

/// What an entity or character reference stands for.
fn resolve_ref(r: &BytesRef) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    quick_xml::escape::resolve_predefined_entity(r).map(str::to_string)
}

/// `xml` as a tree of local names (prefixes dropped) and texts; an
/// answer cut off before its end is an error.
fn parse(xml: &str) -> Result<Node, String> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Start(e) => stack.push(Node {
                name: local(e.name().as_ref()),
                ..Default::default()
            }),
            Event::Empty(e) => {
                let node = Node {
                    name: local(e.name().as_ref()),
                    ..Default::default()
                };
                stack.last_mut().expect("root").children.push(node);
            }
            Event::End(_) => {
                let node = stack.pop().expect("open element");
                stack
                    .last_mut()
                    .ok_or("unbalanced XML")?
                    .children
                    .push(node);
            }
            Event::Text(t) => stack.last_mut().expect("root").text.push_str(&t),
            Event::CData(c) => stack.last_mut().expect("root").text.push_str(&c),
            Event::GeneralRef(r) => {
                if let Some(s) = resolve_ref(&r) {
                    stack.last_mut().expect("root").text.push_str(&s);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err("the answer ends before its XML does".to_string());
    }
    stack.pop().ok_or_else(|| "empty XML".to_string())
}

fn clone(n: &Node) -> Node {
    Node {
        name: n.name.clone(),
        text: n.text.clone(),
        children: n.children.iter().map(clone).collect(),
    }
}

/// The text of a string parameter, escaped.
fn string_param(name: &str, value: &str) -> String {
    format!(
        "<{name} xsi:type=\"xsd:string\">{}</{name}>",
        quick_xml::escape::escape(value)
    )
}

/// The fault in a SOAP answer, as its text says it.
fn fault(tree: &Node) -> Option<String> {
    let fault = tree.find("Fault")?;
    Some(
        fault
            .find("faultstring")
            .map_or("", |f| f.text.as_str())
            .to_string(),
    )
}

/// Call `method` with `params` (already XML), returning the answer's
/// body. A fault, which comes with HTTP 500, is an error with its text.
fn soap(
    http: &reqwest::blocking::Client,
    base: &str,
    method: &str,
    params: &str,
) -> Result<Node, String> {
    let envelope = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
         <soap:Envelope xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\" \
         xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
         xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" \
         xmlns:soapenc=\"http://schemas.xmlsoap.org/soap/encoding/\">\
         <soap:Body><ns1:{method} xmlns:ns1=\"Debbugs/SOAP\">{params}</ns1:{method}>\
         </soap:Body></soap:Envelope>"
    );
    let what = format!("debbugs {method}");
    let resp = http
        .post(format!("{base}/cgi-bin/soap.cgi"))
        .header("Content-Type", "text/xml; charset=utf-8")
        .header("SOAPAction", format!("\"Debbugs/SOAP#{method}\""))
        .body(envelope)
        .send()
        .map_err(|e| format!("{what}: {e}"))?;
    let status = resp.status();
    let text = resp.text().map_err(|e| format!("{what}: {e}"))?;
    let tree = parse(&text);
    if let Some(why) = tree.as_ref().ok().and_then(fault) {
        return Err(format!("{what}: {why}"));
    }
    if !status.is_success() {
        return Err(format!("{what}: HTTP {status}"));
    }
    let tree = tree.map_err(|e| format!("{what}: {e}"))?;
    tree.find("Body")
        .map(clone)
        .ok_or_else(|| format!("{what}: no body in the answer"))
}

fn collect_items(node: &Node, out: &mut Vec<u64>) {
    for c in &node.children {
        if c.name == "item" && c.children.is_empty() {
            if let Ok(n) = c.text.trim().parse() {
                out.push(n);
            }
        } else {
            collect_items(c, out);
        }
    }
}

/// The bug numbers in an answer's array.
fn numbers(body: &Node) -> Vec<u64> {
    let mut out = Vec::new();
    collect_items(body, &mut out);
    out
}

/// The bugs whose `key` (`submitter`, `correspondent`) is `value`,
/// archived ones too.
fn get_bugs(
    http: &reqwest::blocking::Client,
    base: &str,
    key: &str,
    value: &str,
) -> Result<Vec<u64>, String> {
    let params = [
        string_param("k1", key),
        string_param("v1", value),
        string_param("k2", "archive"),
        string_param("v2", "both"),
    ]
    .concat();
    let body = soap(http, base, "get_bugs", &params)?;
    Ok(numbers(&body))
}

/// The bugs a `get_status` answer describes: each an item of a key and
/// a value holding the bug's fields.
fn statuses(body: &Node) -> Vec<Status> {
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        for c in &node.children {
            match (c.name.as_str(), c.child("value")) {
                ("item", Some(value)) => {
                    let field = |name: &str| value.child(name).map_or("", |f| f.text.trim());
                    let int = |name: &str| field(name).parse::<i64>().unwrap_or(0);
                    let Ok(number) = field("bug_num").parse() else {
                        continue;
                    };
                    out.push(Status {
                        number,
                        package: field("package").to_string(),
                        title: field("subject").to_string(),
                        filed: int("date"),
                        modified: int("log_modified"),
                        done: !field("done").is_empty(),
                    });
                }
                _ => stack.push(c),
            }
        }
    }
    out.sort_by_key(|s| s.number);
    out
}

/// The state of each of `bugs`, a batch of them at a time.
fn get_status(
    http: &reqwest::blocking::Client,
    base: &str,
    bugs: &[u64],
    verbose: bool,
) -> Result<Vec<Status>, String> {
    let mut out = Vec::new();
    for batch in bugs.chunks(STATUS_BATCH) {
        if verbose {
            eprintln!("[debbugs] {base}: get_status for {} bug(s)", batch.len());
        }
        let items: String = batch
            .iter()
            .map(|n| format!("<item xsi:type=\"xsd:int\">{n}</item>"))
            .collect();
        let params = format!(
            "<bugs soapenc:arrayType=\"xsd:int[{}]\" xsi:type=\"soapenc:Array\">{items}</bugs>",
            batch.len()
        );
        let body = soap(http, base, "get_status", &params)?;
        out.extend(statuses(&body));
    }
    Ok(out)
}

/// A message's headers, by lowercased name, the first of each name kept
/// with its continuation lines.
fn header_map(text: &str) -> BTreeMap<String, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    // The header continuation lines belong to; none for a repeated one.
    let mut last: Option<String> = None;
    for line in text.lines() {
        if line.starts_with([' ', '\t']) {
            if let Some(name) = &last
                && let Some(v) = out.get_mut(name)
            {
                v.push(' ');
                v.push_str(line.trim());
            }
            continue;
        }
        last = None;
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_lowercase();
            if !out.contains_key(&name) {
                out.insert(name.clone(), value.trim().to_string());
                last = Some(name);
            }
        }
    }
    out
}

/// The address in a `From:` value, lowercased: the part in angle
/// brackets, or the whole value.
fn address_of(from: &str) -> String {
    let from = from.trim();
    let address = match (from.rfind('<'), from.rfind('>')) {
        (Some(open), Some(close)) if open < close => &from[open + 1..close],
        _ => from,
    };
    address.trim().to_lowercase()
}

/// The dates of the messages in a `get_bug_log` answer sent from one of
/// `emails`, matched exactly, ignoring case.
fn dates_from(body: &Node, emails: &[String]) -> Vec<i64> {
    let wanted: BTreeSet<String> = emails.iter().map(|e| e.trim().to_lowercase()).collect();
    let mut out = Vec::new();
    let mut stack = vec![body];
    while let Some(node) = stack.pop() {
        for c in &node.children {
            match c.child("header") {
                Some(header) if c.name == "item" => {
                    let headers = header_map(&header.text);
                    let from = headers
                        .get("from")
                        .map(|f| address_of(f))
                        .unwrap_or_default();
                    if wanted.contains(&from)
                        && let Some(date) = headers
                            .get("date")
                            .and_then(|d| DateTime::parse_from_rfc2822(d.trim()).ok())
                    {
                        out.push(date.timestamp());
                    }
                }
                _ => stack.push(c),
            }
        }
    }
    out
}

/// When each message in bug `number`'s log from one of `emails` was
/// sent (Unix times), by its `From:` and `Date:` headers.
fn message_dates(
    http: &reqwest::blocking::Client,
    base: &str,
    number: u64,
    emails: &[String],
    verbose: bool,
) -> Result<Vec<i64>, String> {
    if verbose {
        eprintln!("[debbugs] {base}: get_bug_log for #{number}");
    }
    let params = format!("<bugnumber xsi:type=\"xsd:int\">{number}</bugnumber>");
    let body = soap(http, base, "get_bug_log", &params)?;
    Ok(dates_from(&body, emails))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A SOAP answer around `body`, as bugs.debian.org writes one.
    fn envelope(body: &str) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?><soap:Envelope \
             xmlns:soap=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             xmlns:soapenc=\"http://schemas.xmlsoap.org/soap/encoding/\" \
             xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"><soap:Body>{body}</soap:Body></soap:Envelope>"
        )
    }

    fn bugs(numbers: &[u64]) -> String {
        let items: String = numbers
            .iter()
            .map(|n| format!("<item xsi:type=\"xsd:int\">{n}</item>"))
            .collect();
        envelope(&format!(
            "<get_bugsResponse xmlns=\"Debbugs/SOAP\"><soapenc:Array>{items}</soapenc:Array></get_bugsResponse>"
        ))
    }

    fn status(bugs: &[(u64, &str, &str, i64, i64, &str)]) -> String {
        let items: String = bugs
            .iter()
            .map(|(n, package, subject, date, modified, done)| {
                format!(
                    "<item><key xsi:type=\"xsd:int\">{n}</key><value>\
                     <bug_num xsi:type=\"xsd:int\">{n}</bug_num><package>{package}</package>\
                     <subject>{subject}</subject><date>{date}</date>\
                     <log_modified>{modified}</log_modified><done>{done}</done></value></item>"
                )
            })
            .collect();
        envelope(&format!(
            "<get_statusResponse xmlns=\"Debbugs/SOAP\"><s-gensym3>{items}</s-gensym3></get_statusResponse>"
        ))
    }

    fn log(messages: &[(&str, &str)]) -> String {
        let items: String = messages
            .iter()
            .map(|(from, date)| {
                format!(
                    "<item><header>Received: by x\nFrom: {from}\nSubject: Re: a bug\n\
                     Date: {date}\n</header><body>text</body><msg_num>2</msg_num></item>"
                )
            })
            .collect();
        envelope(&format!(
            "<get_bug_logResponse xmlns=\"Debbugs/SOAP\"><soapenc:Array>{items}</soapenc:Array></get_bug_logResponse>"
        ))
    }

    // 2026-08-01T00:00:00Z and a day.
    const AUG1: i64 = 1_785_542_400;
    const DAY: i64 = 86_400;

    #[test]
    fn answers_are_read_from_their_soap() {
        let body = parse(&bugs(&[3, 1, 2])).unwrap();
        assert_eq!(numbers(&body), [3, 1, 2]);
        let body = parse(&status(&[
            (7, "rust-x", "x: crashes &amp; burns", AUG1, AUG1 + DAY, ""),
            (
                5,
                "y",
                "y: wrong",
                AUG1,
                AUG1,
                "Someone &lt;s@example.org&gt;",
            ),
        ]))
        .unwrap();
        let read = statuses(&body);
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].number, 5);
        assert!(read[0].done);
        assert_eq!(read[1].title, "x: crashes & burns");
        assert_eq!(read[1].modified, AUG1 + DAY);
        let body = parse(&log(&[
            (
                "Me &lt;me@example.org&gt;",
                "Wed, 05 Aug 2026 10:00:00 +0000",
            ),
            (
                "Other &lt;o@example.org&gt;",
                "Thu, 06 Aug 2026 10:00:00 +0000",
            ),
            ("me@example.org", "not a date"),
            // Another address that ends in this one.
            ("reuben@example.org", "Fri, 07 Aug 2026 10:00:00 +0000"),
        ]))
        .unwrap();
        assert_eq!(
            dates_from(&body, &["ME@example.org".to_string()]),
            [AUG1 + 4 * DAY + 10 * 3600]
        );
        assert_eq!(
            dates_from(&body, &["ben@example.org".to_string()]),
            Vec::<i64>::new()
        );
        let headers = header_map("Subject: a long\n  one\nFrom: x\nFrom: y\n  z\nTo: t\n");
        assert_eq!(headers["subject"], "a long one");
        // A repeated header, and its continuation, leave the first alone.
        assert_eq!(headers["from"], "x");
        assert_eq!(headers["to"], "t");
        assert_eq!(address_of("Me <ME@Example.org>"), "me@example.org");
        assert_eq!(address_of(" me@example.org "), "me@example.org");
        assert_eq!(address_of("odd> <"), "odd> <");
        assert!(parse("<a><b></a>").is_err());
        // An answer cut off partway.
        assert!(parse("<a><b>text</b>").is_err());
    }

    #[test]
    fn bugs_filed_and_written_to_in_the_window_are_listed() {
        use wiremock::matchers::{body_string_contains, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let answer = |marker: &str, xml: String| {
            Mock::given(method("POST"))
                .and(path("/cgi-bin/soap.cgi"))
                .and(body_string_contains(marker.to_string()))
                .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        };
        rt.block_on(async {
            answer(">submitter<", bugs(&[10, 11])).mount(&server).await;
            answer(">correspondent<", bugs(&[10, 20, 21, 22]))
                .mount(&server)
                .await;
            answer(
                "get_status",
                status(&[
                    // Filed in the window, and before it.
                    (10, "a", "a: one", AUG1 + DAY, AUG1 + DAY, ""),
                    (11, "b", "b: old", AUG1 - 90 * DAY, AUG1 - 80 * DAY, "x"),
                    // Written to: in the window, before it, and quiet.
                    (20, "c", "c: talked", AUG1 - 90 * DAY, AUG1 + 5 * DAY, ""),
                    (21, "d", "d: earlier", AUG1 - 90 * DAY, AUG1 + 6 * DAY, ""),
                    (22, "e", "e: quiet", AUG1 - 90 * DAY, AUG1 - 60 * DAY, ""),
                ]),
            )
            .mount(&server)
            .await;
            answer(
                "<bugnumber xsi:type=\"xsd:int\">20</bugnumber>",
                log(&[("me@example.org", "Wed, 05 Aug 2026 10:00:00 +0000")]),
            )
            .mount(&server)
            .await;
            answer(
                "<bugnumber xsi:type=\"xsd:int\">21</bugnumber>",
                log(&[("me@example.org", "Mon, 01 Jun 2026 10:00:00 +0000")]),
            )
            .mount(&server)
            .await;
        });
        let since = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let until = NaiveDate::from_ymd_opt(2026, 8, 31).unwrap();
        let r = bts_report(
            &server.uri(),
            &["me@example.org".into()],
            since,
            until,
            true,
        )
        .unwrap();
        let ids = |v: &[BugRef]| v.iter().map(|b| b.number).collect::<Vec<_>>();
        assert_eq!(ids(&r.filed), [10]);
        assert_eq!(ids(&r.commented), [20]);
        assert_eq!(r.filed[0].url, format!("{}/10", server.uri()));
        let md = format_markdown(&r, 2);
        assert!(
            md.contains("- **Bugs filed:** 1\n- **Bugs commented on:** 1\n"),
            "{md}"
        );
        assert!(
            md.contains(&format!("- [c#20]({}/20) c: talked\n", server.uri())),
            "{md}"
        );
        assert!(format_markdown(&r, 1).contains("#### Bugs filed\n\n- `a`: 1\n"));
        assert!(
            format_markdown(&BtsReport::default(), 0).contains("No Debian bug tracker activity.")
        );
        // A fault is an error, with its text.
        let faulty = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(500).set_body_string(envelope(
                    "<soap:Fault><faultstring>no such method</faultstring></soap:Fault>",
                )))
                .mount(&faulty),
        );
        let err = bts_report(
            &faulty.uri(),
            &["me@example.org".into()],
            since,
            until,
            false,
        )
        .unwrap_err();
        assert!(err.contains("debbugs get_bugs: no such method"), "{err}");
        // An HTTP error without a fault is reported by its status.
        let broken = rt.block_on(MockServer::start());
        rt.block_on(
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(503).set_body_string("down"))
                .mount(&broken),
        );
        let err = bts_report(
            &broken.uri(),
            &["me@example.org".into()],
            since,
            until,
            false,
        )
        .unwrap_err();
        assert!(err.contains("debbugs get_bugs: HTTP 503"), "{err}");
    }
}
