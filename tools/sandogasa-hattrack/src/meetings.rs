// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `meetings` — a contributor's attendance at a recurring meetbot
//! meeting (`fesco`), from the "People Present" list of each meeting's
//! minutes.

use std::collections::BTreeSet;

use chrono::{DateTime, Duration, Utc};
use sandogasa_meetbot::Meetbot;
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
    pub days: u32,
    /// The Matrix IDs taken as the user.
    pub matrix_ids: Vec<String>,
    pub attended: usize,
    pub total: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_attended: Option<String>,
    /// Newest first.
    pub meetings: Vec<MeetingRow>,
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
    days: u32,
    ids: Vec<String>,
    rows: Vec<MeetingRow>,
) -> Report {
    Report {
        username: username.to_string(),
        topic: topic.to_string(),
        days,
        matrix_ids: ids,
        attended: rows.iter().filter(|r| r.present).count(),
        total: rows.len(),
        last_attended: rows.iter().find(|r| r.present).map(|r| r.date.clone()),
        meetings: rows,
    }
}

/// The `meetings` subcommand.
#[allow(clippy::too_many_arguments)]
pub async fn cmd_meetings(
    username: &str,
    topic: &str,
    days: u32,
    extra_ids: &[String],
    no_fas: bool,
    url: &str,
    json: bool,
    now: DateTime<Utc>,
) -> Result<(), Box<dyn std::error::Error>> {
    let since = now - Duration::days(i64::from(days));
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
        fetch(&Meetbot::with_base_url(&base), &t, &ids, since, now).map(|rows| (ids, rows))
    })
    .await??;
    let r = report(username, topic, days, ids, rows);
    if json {
        println!("{}", serde_json::to_string_pretty(&r)?);
        return Ok(());
    }
    println!("Meetings: {username} in '{topic}' (last {days} days)\n");
    println!("  Matrix IDs: {}", r.matrix_ids.join(", "));
    match &r.last_attended {
        Some(d) => println!(
            "  Attended {} of {} meeting(s); last attended {d}\n",
            r.attended, r.total
        ),
        None => println!("  Attended 0 of {} meeting(s)\n", r.total),
    }
    for m in &r.meetings {
        if m.present {
            println!("  {}  present  ({} line(s) said)", m.date, m.lines);
        } else {
            println!("  {}  absent", m.date);
        }
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
) -> ServiceLastSeen {
    let ids = matrix_ids(username, from_fas, extra_ids);
    let (t, i) = (topic.to_string(), ids);
    let since = now - Duration::days(365);
    let result =
        tokio::task::spawn_blocking(move || fetch(&Meetbot::new(), &t, &i, since, now)).await;
    let service = "Meetings".to_string();
    match result {
        Ok(Ok(rows)) => {
            let attended = rows.iter().filter(|r| r.present).count();
            match rows.iter().find(|r| r.present) {
                Some(r) => ServiceLastSeen {
                    service,
                    last_active: Some(format!("{}T00:00:00+00:00", r.date)),
                    detail: Some(format!(
                        "{topic}: attended {attended} of {} in the last year",
                        rows.len()
                    )),
                    ..Default::default()
                },
                None => ServiceLastSeen {
                    service,
                    detail: Some(format!(
                        "{topic}: attended none of {} in the last year",
                        rows.len()
                    )),
                    ..Default::default()
                },
            }
        }
        Ok(Err(e)) => ServiceLastSeen {
            service,
            error: Some(e),
            ..Default::default()
        },
        Err(e) => ServiceLastSeen {
            service,
            error: Some(e.to_string()),
            ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let r = report("salimma", "fesco", 30, ids, rows);
        assert_eq!(r.attended, 2);
        assert_eq!(r.total, 3);
        assert_eq!(r.last_attended.as_deref(), Some("2026-08-25"));
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
}
