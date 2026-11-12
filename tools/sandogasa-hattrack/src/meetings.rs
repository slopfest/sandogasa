// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `meetings` — a contributor's attendance at a recurring meetbot
//! meeting (`fesco`), from the "People Present" list of each meeting's
//! minutes.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use sandogasa_meetbot::Meetbot;

/// The cache directory the meeting answers live under.
const TOOL_NAME: &str = "sandogasa-hattrack";
use serde::Serialize;

use crate::ServiceLastSeen;

/// One meeting in the window.
#[derive(Debug, Clone, Serialize)]
pub struct MeetingRow {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub present: bool,
    /// Lines the user said (0 when absent).
    pub lines: u32,
    pub minutes_url: String,
}

/// The `meetings` report.
#[derive(Debug, Serialize)]
pub struct Report {
    pub username: String,
    /// The meetbot topic (`!meetingname`), e.g. `fesco`.
    pub topic: String,
    /// Start of the window, `YYYY-MM-DD`.
    pub since: String,
    /// The Matrix IDs taken as the user.
    pub matrix_ids: Vec<String>,
    pub attended: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attended: Option<String>,
    /// The oldest attended meeting in the window — where a member newer
    /// than the window starts counting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_attended: Option<String>,
    /// Attended and total from `first_attended` on (0, 0 when never).
    pub attended_since_first: usize,
    pub total_since_first: usize,
    /// Newest first.
    pub meetings: Vec<MeetingRow>,
}

/// How the window was asked for, for the heading.
pub enum Window {
    Days(u32),
    Since(chrono::NaiveDate),
}

impl Window {
    fn start(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Window::Days(d) => now - Duration::days(i64::from(*d)),
            Window::Since(date) => date.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
        }
    }

    fn label(&self) -> String {
        match self {
            Window::Days(d) => format!("last {d} days"),
            Window::Since(date) => format!("since {date}"),
        }
    }
}

/// The Matrix IDs that are this user: `@<fas>:fedora.im`, the ones the
/// FAS profile lists, and any given explicitly — deduplicated, in that
/// order.
pub fn matrix_ids(username: &str, from_fas: &[String], extra: &[String]) -> Vec<String> {
    let mut ids = vec![format!("@{username}:fedora.im")];
    for id in from_fas.iter().chain(extra) {
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids
}

/// The Matrix IDs on the user's FAS profile, via FASJSON (Kerberos);
/// a failed lookup is a warning, not an error — the fedora.im
/// assumption still stands.
fn fas_matrix_ids(username: &str) -> Vec<String> {
    match sandogasa_fasjson::FasjsonClient::new().user(username) {
        Ok(user) => user.matrix_ids(),
        Err(e) => {
            eprintln!(
                "warning: FASJSON lookup of {username} failed ({e}); assuming @{username}:fedora.im only"
            );
            Vec::new()
        }
    }
}

/// Every `topic` meeting in `[since, until]`, newest first, one per
/// day, with whether one of `ids` was present (blocking).
fn fetch(
    meetbot: &Meetbot,
    topic: &str,
    ids: &[String],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> Result<Vec<MeetingRow>, String> {
    let mut meetings: Vec<_> = meetbot
        .search(topic)
        .map_err(|e| format!("meetbot search failed: {e}"))?
        .into_iter()
        .filter(|m| {
            let at = m.datetime.and_utc();
            m.topic == topic && at >= since && at <= until
        })
        .collect();
    meetings.sort_by_key(|m| std::cmp::Reverse(m.datetime));
    let mut seen_days = BTreeSet::new();
    let mut rows = Vec::new();
    for m in meetings {
        let date = m.datetime.date().to_string();
        if !seen_days.insert(date.clone()) {
            continue;
        }
        let attendees = meetbot
            .attendees(&m)
            .map_err(|e| format!("minutes of {date}: {e}"))?;
        let lines: u32 = attendees
            .iter()
            .filter(|a| ids.iter().any(|id| id == &a.id))
            .map(|a| a.lines)
            .sum();
        rows.push(MeetingRow {
            date,
            present: lines > 0,
            lines,
            minutes_url: m.summary_url,
        });
    }
    Ok(rows)
}

fn report(
    username: &str,
    topic: &str,
    since: DateTime<Utc>,
    ids: Vec<String>,
    rows: Vec<MeetingRow>,
) -> Report {
    // Rows are newest first: the last present one is the first attended.
    let first_attended = rows
        .iter()
        .rev()
        .find(|r| r.present)
        .map(|r| r.date.clone());
    let since_first: Vec<&MeetingRow> = match &first_attended {
        Some(f) => rows.iter().filter(|r| &r.date >= f).collect(),
        None => Vec::new(),
    };
    Report {
        username: username.to_string(),
        topic: topic.to_string(),
        since: since.format("%Y-%m-%d").to_string(),
        matrix_ids: ids,
        attended: rows.iter().filter(|r| r.present).count(),
        total: rows.len(),
        last_attended: rows.iter().find(|r| r.present).map(|r| r.date.clone()),
        first_attended,
        attended_since_first: since_first.iter().filter(|r| r.present).count(),
        total_since_first: since_first.len(),
        meetings: rows,
    }
}

/// `attended 13 of 14; since first seen 2026-04-14, 13 of 13` — the
/// second clause only when meetings in the window predate the first
/// attendance, which is what a member newer than the window looks like.
fn attendance_summary(r: &Report) -> String {
    let mut s = format!("attended {} of {}", r.attended, r.total);
    if let Some(first) = &r.first_attended
        && r.total_since_first < r.total
    {
        s.push_str(&format!(
            "; since first seen {first}, {} of {}",
            r.attended_since_first, r.total_since_first
        ));
    }
    s
}

/// The human-readable `meetings` report.
fn render(r: &Report, window: &Window) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Meetings: {} in '{}' ({})\n",
        r.username,
        r.topic,
        window.label()
    );
    let _ = writeln!(out, "  Matrix IDs: {}", r.matrix_ids.join(", "));
    let mut summary = attendance_summary(r);
    if let Some(first) = summary.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    match &r.last_attended {
        Some(d) => {
            let _ = writeln!(out, "  {summary} meeting(s); last attended {d}\n");
        }
        None => {
            let _ = writeln!(out, "  {summary} meeting(s)\n");
        }
    }
    for m in &r.meetings {
        if m.present {
            let _ = writeln!(out, "  {}  present  ({} line(s) said)", m.date, m.lines);
        } else {
            let _ = writeln!(out, "  {}  absent", m.date);
        }
    }
    out
}

/// The `last-seen` entry for `topic` from a year of meetings, or from
/// why they could not be read.
fn last_seen(
    username: &str,
    topic: &str,
    since: DateTime<Utc>,
    rows: Result<Vec<MeetingRow>, String>,
) -> ServiceLastSeen {
    let service = "Meetings".to_string();
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            return ServiceLastSeen {
                service,
                error: Some(e),
                ..Default::default()
            };
        }
    };
    let r = report(username, topic, since, Vec::new(), rows);
    match &r.last_attended {
        Some(d) => ServiceLastSeen {
            service,
            last_active: Some(format!("{d}T00:00:00+00:00")),
            detail: Some(format!(
                "{topic}: {} in the last year",
                attendance_summary(&r)
            )),
            ..Default::default()
        },
        None => ServiceLastSeen {
            service,
            detail: Some(format!(
                "{topic}: attended none of {} in the last year",
                r.total
            )),
            ..Default::default()
        },
    }
}

/// The `meetings` subcommand.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_meetings(
    username: &str,
    topic: &str,
    window: Window,
    extra_ids: &[String],
    no_fas: bool,
    url: &str,
    json: bool,
    now: DateTime<Utc>,
    refresh: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let since = window.start(now);
    let (u, t, x, base) = (
        username.to_string(),
        topic.to_string(),
        extra_ids.to_vec(),
        url.to_string(),
    );
    let (ids, rows) = tokio::task::spawn_blocking(move || {
        let from_fas = if no_fas {
            Vec::new()
        } else {
            fas_matrix_ids(&u)
        };
        let ids = matrix_ids(&u, &from_fas, &x);
        let meetbot = Meetbot::with_base_url(&base).with_cache(TOOL_NAME, refresh);
        fetch(&meetbot, &t, &ids, since, now).map(|rows| (ids, rows))
    })
    .await??;
    let r = report(username, topic, since, ids, rows);
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
    } else {
        print!("{}", render(&r, &window));
    }
    Ok(())
}

/// The last `topic` meeting the user attended, for `last-seen`
/// (looks back one year).
pub async fn check_meetings(
    username: &str,
    topic: &str,
    from_fas: &[String],
    extra_ids: &[String],
    now: DateTime<Utc>,
    refresh: bool,
) -> ServiceLastSeen {
    let ids = matrix_ids(username, from_fas, extra_ids);
    let (t, i) = (topic.to_string(), ids);
    let since = now - Duration::days(365);
    let result = tokio::task::spawn_blocking(move || {
        fetch(
            &Meetbot::new().with_cache(TOOL_NAME, refresh),
            &t,
            &i,
            since,
            now,
        )
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    last_seen(username, topic, since, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(date: &str, lines: u32) -> MeetingRow {
        MeetingRow {
            date: date.to_string(),
            present: lines > 0,
            lines,
            minutes_url: String::new(),
        }
    }

    #[test]
    fn a_member_newer_than_the_window_is_counted_from_their_first_meeting() {
        // Joined in July: absent from the two June meetings by definition.
        let rows = vec![
            row("2026-07-21", 5),
            row("2026-07-14", 0),
            row("2026-07-07", 3),
            row("2026-06-30", 0),
            row("2026-06-23", 0),
        ];
        let since = "2026-06-20T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let r = report("newbie", "fesco", since, Vec::new(), rows);
        assert_eq!((r.attended, r.total), (2, 5));
        assert_eq!(r.first_attended.as_deref(), Some("2026-07-07"));
        assert_eq!((r.attended_since_first, r.total_since_first), (2, 3));
        assert_eq!(
            attendance_summary(&r),
            "attended 2 of 5; since first seen 2026-07-07, 2 of 3"
        );
        assert_eq!(Window::Days(90).label(), "last 90 days");
        assert_eq!(
            Window::Since("2026-06-20".parse().unwrap()).label(),
            "since 2026-06-20"
        );
    }

    #[test]
    fn report_counts_attendance_newest_first() {
        let rows = vec![
            MeetingRow {
                date: "2026-09-01".into(),
                present: false,
                lines: 0,
                minutes_url: String::new(),
            },
            MeetingRow {
                date: "2026-08-25".into(),
                present: true,
                lines: 7,
                minutes_url: String::new(),
            },
            MeetingRow {
                date: "2026-08-18".into(),
                present: true,
                lines: 1,
                minutes_url: String::new(),
            },
        ];
        let ids = matrix_ids(
            "salimma",
            &["@salimma:fedora.im".into(), "@michel-slm:matrix.org".into()],
            &["@x:example.org".into()],
        );
        let since = "2026-08-10T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let r = report("salimma", "fesco", since, ids, rows);
        assert_eq!(r.attended, 2);
        assert_eq!(r.total, 3);
        assert_eq!(r.since, "2026-08-10");
        assert_eq!(r.last_attended.as_deref(), Some("2026-08-25"));
        // Attended the oldest meeting in the window: no "since first seen".
        assert_eq!(r.first_attended.as_deref(), Some("2026-08-18"));
        assert_eq!(attendance_summary(&r), "attended 2 of 3");
        // fedora.im first, FAS's next (the duplicate dropped), then --matrix.
        assert_eq!(
            r.matrix_ids,
            [
                "@salimma:fedora.im",
                "@michel-slm:matrix.org",
                "@x:example.org"
            ]
        );
    }

    /// `(datetime, topic, [(matrix id, lines said)])`.
    type MeetingFixture<'a> = (&'a str, &'a str, &'a [(&'a str, u32)]);

    /// A meetbot whose search for `fesco` answers `meetings`
    /// (`(datetime, topic)`), each with minutes listing `present`
    /// (`(matrix id, lines)`).
    fn meetbot(meetings: &[MeetingFixture]) -> (tokio::runtime::Runtime, wiremock::MockServer) {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(MockServer::start());
        let mut found = Vec::new();
        for (i, (at, topic, present)) in meetings.iter().enumerate() {
            let base = format!("https://meetbot-raw.fedoraproject.org/m/{topic}.{i}");
            found.push(serde_json::json!({
                "channel": "m",
                "datetime": at,
                "topic": topic,
                "url": {"logs": format!("{base}.log.html"), "summary": format!("{base}.html")},
            }));
            let mut minutes = String::from("People Present (lines said)\n---\n");
            for (id, lines) in *present {
                minutes.push_str(&format!("* {id} ({lines})\n"));
            }
            rt.block_on(
                Mock::given(method("GET"))
                    .and(path(format!("/m/{topic}.{i}.txt")))
                    .respond_with(ResponseTemplate::new(200).set_body_string(minutes))
                    .mount(&server),
            );
        }
        rt.block_on(
            Mock::given(method("GET"))
                .and(path("/fragedpt/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(found))
                .mount(&server),
        );
        (rt, server)
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn meetings_in_the_window_are_read_newest_first_one_a_day() {
        let me: &[(&str, u32)] = &[("@me:fedora.im", 4), ("@other:fedora.im", 9)];
        let alt: &[(&str, u32)] = &[("@me:matrix.org", 2)];
        let none: &[(&str, u32)] = &[("@other:fedora.im", 1)];
        let (_rt, server) = meetbot(&[
            ("2026-06-01T15:00:00", "fesco", me),
            // A second meeting the same day is not counted again.
            ("2026-06-01T18:00:00", "fesco", none),
            ("2026-06-08T15:00:00", "fesco", none),
            ("2026-06-15T15:00:00", "fesco", alt),
            // Outside the window, and another topic the search matched.
            ("2026-05-01T15:00:00", "fesco", me),
            ("2026-06-10T15:00:00", "fesco-extra", me),
        ]);
        let ids = matrix_ids("me", &[], &["@me:matrix.org".to_string()]);
        let rows = fetch(
            &Meetbot::with_base_url(&server.uri()),
            "fesco",
            &ids,
            at("2026-05-20T00:00:00Z"),
            at("2026-06-20T00:00:00Z"),
        )
        .unwrap();
        let got: Vec<(&str, bool, u32)> = rows
            .iter()
            .map(|r| (r.date.as_str(), r.present, r.lines))
            .collect();
        assert_eq!(
            got,
            [
                ("2026-06-15", true, 2),
                ("2026-06-08", false, 0),
                ("2026-06-01", false, 0),
            ],
            "the later of the two 2026-06-01 meetings is the one read"
        );
        assert!(rows[0].minutes_url.starts_with(&server.uri()));
    }

    #[test]
    fn a_failed_search_says_so() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let server = rt.block_on(wiremock::MockServer::start());
        let err = fetch(
            &Meetbot::with_base_url(&server.uri()),
            "fesco",
            &[],
            at("2026-05-20T00:00:00Z"),
            at("2026-06-20T00:00:00Z"),
        )
        .unwrap_err();
        assert!(err.starts_with("meetbot search failed"), "{err}");
    }

    #[test]
    fn matrix_ids_start_with_fedora_im_and_drop_duplicates() {
        let fas = ["@me:fedora.im".to_string(), "@me:matrix.org".to_string()];
        let extra = ["@me:matrix.org".to_string(), "@me:example.org".to_string()];
        assert_eq!(
            matrix_ids("me", &fas, &extra),
            ["@me:fedora.im", "@me:matrix.org", "@me:example.org"]
        );
    }

    #[test]
    fn a_window_is_days_back_or_a_date() {
        let now = at("2026-06-20T12:00:00Z");
        assert_eq!(Window::Days(30).start(now), at("2026-05-21T12:00:00Z"));
        assert_eq!(Window::Days(30).label(), "last 30 days");
        let since = Window::Since("2026-01-01".parse().unwrap());
        assert_eq!(since.start(now), at("2026-01-01T00:00:00Z"));
        assert_eq!(since.label(), "since 2026-01-01");
    }

    #[test]
    fn the_report_lists_each_meeting_under_the_summary() {
        let since = at("2026-06-01T00:00:00Z");
        let rows = vec![row("2026-06-15", 0), row("2026-06-08", 3)];
        let r = report("me", "fesco", since, vec!["@me:fedora.im".into()], rows);
        assert_eq!(
            render(&r, &Window::Days(30)),
            "Meetings: me in 'fesco' (last 30 days)\n\n\
             \x20 Matrix IDs: @me:fedora.im\n\
             \x20 Attended 1 of 2 meeting(s); last attended 2026-06-08\n\n\
             \x20 2026-06-15  absent\n\
             \x20 2026-06-08  present  (3 line(s) said)\n"
        );
        let r = report("me", "fesco", since, vec![], vec![row("2026-06-08", 0)]);
        assert!(render(&r, &Window::Days(30)).contains("  Attended 0 of 1 meeting(s)\n\n"));
    }

    #[test]
    fn last_seen_is_the_newest_attended_meeting() {
        let since = at("2025-06-20T00:00:00Z");
        let seen = last_seen(
            "me",
            "fesco",
            since,
            Ok(vec![row("2026-06-15", 0), row("2026-06-08", 3)]),
        );
        assert_eq!(seen.service, "Meetings");
        assert_eq!(
            seen.last_active.as_deref(),
            Some("2026-06-08T00:00:00+00:00")
        );
        assert_eq!(
            seen.detail.as_deref(),
            Some("fesco: attended 1 of 2 in the last year")
        );
        let never = last_seen("me", "fesco", since, Ok(vec![row("2026-06-08", 0)]));
        assert_eq!(never.last_active, None);
        assert_eq!(
            never.detail.as_deref(),
            Some("fesco: attended none of 1 in the last year")
        );
        let failed = last_seen("me", "fesco", since, Err("meetbot down".into()));
        assert_eq!(failed.error.as_deref(), Some("meetbot down"));
    }
}
