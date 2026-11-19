// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `timebook cancel`: take occurrences of one of your own meetings off
//! Fedora Calendar, past or coming: a meeting that was not held, or will
//! not be, so the calendar says what happened and the room is free.
//!
//! fedocal cancels one occurrence of a series through its delete form:
//! it ends the series the day before and starts it again, as a new entry,
//! after that occurrence. So each later occurrence may be in another
//! entry than the one named; occurrences are found by the meeting's
//! calendar and title, and each again before it is cancelled.

use std::collections::BTreeSet;

use chrono::{Duration, NaiveDate, NaiveTime};
use sandogasa_fedocal::session::Session;
use sandogasa_fedocal::{Client, Meeting, MeetingQuery};

use crate::pick::Named;
use serde::Serialize;

/// What `cancel` was asked to do.
pub struct CancelArgs {
    /// The meeting: its calendar, or the id or page of any of its
    /// entries.
    pub target: String,
    /// In a calendar with several meetings, part of the title of the one
    /// meant.
    pub meeting: Option<String>,
    /// The days of the occurrences to cancel, in the meeting's own time
    /// zone.
    pub dates: Vec<NaiveDate>,
    pub dry_run: bool,
    pub yes: bool,
    /// Print the outcome as JSON, the messages going to stderr.
    pub json: bool,
}

/// What `cancel` did, for `--json`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct CancelOutcome {
    /// `cancelled`, `declined` or `dry_run`.
    pub outcome: &'static str,
    pub cancelled: Vec<NaiveDate>,
}

/// The meeting whose occurrences are cancelled: its calendar and title,
/// and the time it starts at in its own time zone, which tells it from
/// another entry of the same title on a day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Which {
    pub calendar: String,
    pub name: String,
    pub at: Option<NaiveTime>,
}

/// The occurrence of the meeting `which` names on `date` (its own day, in
/// its time zone) among `meetings`; an error when more than one entry
/// could be it.
pub fn occurrence_on<'a>(
    meetings: &'a [Meeting],
    which: &Which,
    date: NaiveDate,
) -> Result<Option<&'a Meeting>, String> {
    let mut found: Vec<&Meeting> = meetings
        .iter()
        .filter(|m| {
            m.calendar_name == which.calendar
                && m.meeting_name
                    .trim()
                    .eq_ignore_ascii_case(which.name.trim())
                && m.local_date() == Some(date)
        })
        .collect();
    if found.len() > 1
        && let Some(at) = which.at
    {
        found.retain(|m| local_time(m) == Some(at));
    }
    match found.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(one)),
        more => {
            let ids: Vec<String> = more.iter().map(|m| m.meeting_id.to_string()).collect();
            Err(format!(
                "more than one \"{}\" on {date} (entries {}); name the one meant by its id",
                which.name,
                ids.join(", ")
            ))
        }
    }
}

/// An occurrence for a list: its day and time in UTC, and its entry.
pub fn describe(m: &Meeting) -> String {
    let when = m
        .starts_at()
        .map(|t| t.format("%a %Y-%m-%d %H:%M UTC").to_string())
        .unwrap_or_else(|| m.meeting_date.to_string());
    format!("{when} (entry {})", m.meeting_id)
}

/// Cancel the occurrences `args` names: log in, find each, list them,
/// confirm, and cancel them one by one, checking each is gone. `confirm`
/// asks at the terminal; `login` logs in (blocking). An error part way
/// says which were cancelled before it.
pub async fn run(
    client: &Client,
    args: &CancelArgs,
    confirm: impl Fn(&str, bool) -> Result<bool, String>,
    login: impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> + Send + 'static,
) -> Result<CancelOutcome, String> {
    let say = |line: String| {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let named = crate::pick::named(&args.target)?;
    if args.meeting.is_some() && matches!(named, Named::Entry(_)) {
        return Err("--meeting picks a meeting in a calendar; an entry names one already".into());
    }
    let mut dates = args.dates.clone();
    dates.sort();
    dates.dedup();
    let (Some(first), Some(last)) = (dates.first().copied(), dates.last().copied()) else {
        return Err("give the days of the occurrences to cancel".into());
    };
    // A day either side: dates are the meeting's own, the API's UTC.
    let read = |calendar: String| async move {
        client
            .meetings(
                &MeetingQuery::default()
                    .calendar(calendar)
                    .between(first - Duration::days(1), last + Duration::days(1)),
            )
            .await
            .map_err(|e| e.to_string())
    };
    // A calendar is read before logging in, so a meeting it does not
    // name clearly costs no login.
    // The meeting and its occurrences, or the entry to read them from.
    let picked: Result<(Which, Vec<Meeting>), u64> = match &named {
        Named::Calendar(calendar) => {
            let meetings = read(calendar.clone()).await?;
            let on_dates: Vec<Meeting> = meetings
                .iter()
                .filter(|m| m.local_date().is_some_and(|d| dates.contains(&d)))
                .cloned()
                .collect();
            let when = format!("on {}", list_dates(&dates));
            let name = crate::pick::title(&on_dates, calendar, args.meeting.as_deref(), &when)?;
            Ok((
                Which {
                    calendar: calendar.clone(),
                    name,
                    at: None,
                },
                meetings,
            ))
        }
        Named::Entry(id) => Err(*id),
    };
    eprintln!("Logging in to Fedora Calendar with your Kerberos ticket...");
    let session = tokio::task::spawn_blocking(login)
        .await
        .map_err(|e| format!("the login stopped: {e}"))?
        .map_err(|e| e.to_string())?;
    let (which, meetings) = match picked {
        Ok(picked) => picked,
        Err(id) => {
            // The meeting, by its calendar, title and time: the API does
            // not look an entry up by its id.
            let form = session.entry(id).await.map_err(|e| e.to_string())?;
            let which = Which {
                calendar: form.get("calendar_name").unwrap_or("").to_string(),
                name: form.get("meeting_name").unwrap_or("").to_string(),
                at: form
                    .get("meeting_time_start")
                    .and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok()),
            };
            let meetings = read(which.calendar.clone()).await?;
            (which, meetings)
        }
    };
    let mut found: Vec<&Meeting> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    for date in &dates {
        match occurrence_on(&meetings, &which, *date)? {
            Some(m) => found.push(m),
            None => missing.push(date.to_string()),
        }
    }
    if !missing.is_empty() {
        return Err(format!(
            "\"{}\" ({}) has no occurrence on {}",
            which.name,
            which.calendar,
            missing.join(", ")
        ));
    }
    let mut list = format!("To cancel in {}, \"{}\":", which.calendar, which.name);
    for m in &found {
        list.push_str(&format!("\n  {}", describe(m)));
    }
    say(list);
    if args.dry_run {
        let entries: BTreeSet<u64> = found.iter().map(|m| m.meeting_id).collect();
        for entry in entries {
            session.can_cancel(entry).await.map_err(|e| e.to_string())?;
        }
        say("Dry run: you may cancel them; nothing cancelled.".to_string());
        return Ok(CancelOutcome {
            outcome: "dry_run",
            cancelled: vec![],
        });
    }
    let question = format!("Cancel these {} occurrence(s)?", found.len());
    if !args.yes && !confirm(&question, false)? {
        say("Nothing cancelled.".to_string());
        return Ok(CancelOutcome {
            outcome: "declined",
            cancelled: vec![],
        });
    }
    let mut cancelled: Vec<NaiveDate> = Vec::new();
    // Any error from here on says what was cancelled before it.
    let failed = |why: String, cancelled: &[NaiveDate]| {
        if cancelled.is_empty() {
            why
        } else {
            format!("{why} (cancelled before it: {})", list_dates(cancelled))
        }
    };
    let mut current = meetings.clone();
    for date in &dates {
        // Found again: each cancellation starts the series anew.
        let m = match occurrence_on(&current, &which, *date) {
            Ok(Some(m)) => m.clone(),
            Ok(None) => {
                return Err(failed(
                    format!("the occurrence on {date} is no longer there"),
                    &cancelled,
                ));
            }
            Err(e) => return Err(failed(e, &cancelled)),
        };
        session.cancel(m.meeting_id, *date).await.map_err(|e| {
            failed(
                format!("cannot cancel the occurrence on {date}: {e}"),
                &cancelled,
            )
        })?;
        current = read(which.calendar.clone()).await.map_err(|e| {
            failed(
                format!("cancelled {date}, but cannot read the calendar back: {e}"),
                &cancelled,
            )
        })?;
        if matches!(occurrence_on(&current, &which, *date), Ok(Some(_))) {
            return Err(failed(
                format!("Fedora Calendar did not cancel the occurrence on {date}"),
                &cancelled,
            ));
        }
        say(format!("Cancelled: {date}"));
        cancelled.push(*date);
    }
    Ok(CancelOutcome {
        outcome: "cancelled",
        cancelled,
    })
}

/// When `m` starts, in its own time zone.
fn local_time(m: &Meeting) -> Option<NaiveTime> {
    let tz: chrono_tz::Tz = m.meeting_timezone.parse().ok()?;
    Some(m.starts_at()?.with_timezone(&tz).time())
}

fn list_dates(dates: &[NaiveDate]) -> String {
    dates
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn occurrence(id: u64, date: &str, time: &str, tz: &str) -> serde_json::Value {
        serde_json::json!({
            "meeting_id": id, "calendar_name": "hs", "meeting_name": "Hyperscale SIG",
            "meeting_date": date, "meeting_date_end": date,
            "meeting_time_start": time, "meeting_time_stop": time,
            "meeting_timezone": tz, "meeting_manager": ["me"]
        })
    }

    fn which(at: Option<&str>) -> Which {
        Which {
            calendar: "hs".into(),
            name: "hyperscale sig".into(),
            at: at.map(|t| NaiveTime::parse_from_str(t, "%H:%M").unwrap()),
        }
    }

    #[test]
    fn occurrences_are_found_by_meeting_and_own_day() {
        let meetings: Vec<Meeting> = serde_json::from_value(serde_json::json!([
            occurrence(1, "2026-11-04", "16:00:00", "US/Eastern"),
            // 04:00 UTC on the 12th is the 11th in New York.
            occurrence(2, "2026-11-12", "04:00:00", "America/New_York"),
        ]))
        .unwrap();
        let on = |d| {
            occurrence_on(&meetings, &which(None), day(d))
                .unwrap()
                .map(|m| m.meeting_id)
        };
        assert_eq!(on("2026-11-04"), Some(1));
        assert_eq!(on("2026-11-11"), Some(2));
        assert_eq!(on("2026-11-12"), None);
        let mut other = which(None);
        other.calendar = "other".into();
        assert_eq!(
            occurrence_on(&meetings, &other, day("2026-11-04")).unwrap(),
            None
        );
        assert_eq!(describe(&meetings[0]), "Wed 2026-11-04 16:00 UTC (entry 1)");
    }

    #[test]
    fn two_entries_of_a_title_on_a_day_are_told_apart_by_time() {
        let meetings: Vec<Meeting> = serde_json::from_value(serde_json::json!([
            occurrence(42, "2026-11-11", "20:00:00", "UTC"),
            occurrence(11, "2026-11-11", "16:00:00", "UTC"),
        ]))
        .unwrap();
        let found = occurrence_on(&meetings, &which(Some("16:00")), day("2026-11-11")).unwrap();
        assert_eq!(found.map(|m| m.meeting_id), Some(11));
        // Without a time to go by, or both at it: not guessed.
        let err = occurrence_on(&meetings, &which(None), day("2026-11-11")).unwrap_err();
        assert!(err.contains("entries 42, 11"), "{err}");
        let same: Vec<Meeting> = serde_json::from_value(serde_json::json!([
            occurrence(42, "2026-11-11", "16:00:00", "UTC"),
            occurrence(11, "2026-11-11", "16:00:00", "UTC"),
        ]))
        .unwrap();
        assert!(occurrence_on(&same, &which(Some("16:00")), day("2026-11-11")).is_err());
    }

    /// A login that leaves a session on `uri`, as if the ticket worked.
    fn fake_login(uri: String) -> impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> {
        move || {
            let host = uri
                .trim_start_matches("http://")
                .split(':')
                .next()
                .unwrap()
                .to_string();
            let jar = format!("{host}\tFALSE\t/\tFALSE\t0\tfedocal\ts\n");
            Session::login_with(&uri, move |argv| {
                let at = argv.iter().position(|a| a == "--cookie-jar").unwrap() + 1;
                std::fs::write(&argv[at], jar).unwrap();
                Ok(format!(
                    "{}/",
                    argv.last().unwrap().trim_end_matches("login/")
                ))
            })
        }
    }

    const EDIT: &str = r#"<form method="post"><select name="calendar_name"><option selected value="hs">hs</option></select>
        <input name="meeting_name" type="text" value="Hyperscale SIG">
        <input name="csrf_token" type="hidden" value="t"></form>"#;

    /// The calendar as it stands: the occurrences left, which a
    /// cancellation takes one of out of.
    #[derive(Clone)]
    struct Calendar(Arc<Mutex<Vec<serde_json::Value>>>);

    impl Respond for Calendar {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            let left = self.0.lock().unwrap().clone();
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"arguments": {}, "meetings": left}))
        }
    }

    /// A delete that takes the occurrence on its `from_date` away.
    struct Delete(Calendar, bool);

    impl Respond for Delete {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body = String::from_utf8_lossy(&request.body).to_string();
            if self.1
                && let Some(date) = body.split("from_date=").nth(1)
            {
                let date = date.split('&').next().unwrap().to_string();
                self.0
                    .0
                    .lock()
                    .unwrap()
                    .retain(|o| o["meeting_date"] != date.as_str());
            }
            ResponseTemplate::new(302).insert_header("location", "/hs/")
        }
    }

    async fn fedocal(deletes: bool) -> (MockServer, Calendar) {
        let server = MockServer::start().await;
        let calendar = Calendar(Arc::new(Mutex::new(vec![
            occurrence(11, "2026-11-04", "16:00:00", "UTC"),
            occurrence(11, "2026-11-11", "16:00:00", "UTC"),
            occurrence(11, "2026-11-18", "16:00:00", "UTC"),
        ])));
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(calendar.clone())
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/11/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(EDIT))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/delete/11/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(EDIT))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/delete/11/"))
            .and(body_string_contains("confirm_delete=y"))
            .respond_with(Delete(calendar.clone(), deletes))
            .mount(&server)
            .await;
        (server, calendar)
    }

    fn args(dates: &[&str]) -> CancelArgs {
        CancelArgs {
            target: "11".into(),
            meeting: None,
            dates: dates.iter().map(|d| day(d)).collect(),
            dry_run: false,
            yes: false,
            json: false,
        }
    }

    #[tokio::test]
    async fn occurrences_are_cancelled_after_a_confirmation() {
        let (server, calendar) = fedocal(true).await;
        let client = Client::with_base_url(&server.uri());
        let yes = |q: &str, default: bool| {
            assert_eq!(q, "Cancel these 2 occurrence(s)?");
            assert!(!default);
            Ok(true)
        };
        let done = run(
            &client,
            &args(&["2026-11-18", "2026-11-04", "2026-11-04"]),
            yes,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "cancelled");
        assert_eq!(done.cancelled, [day("2026-11-04"), day("2026-11-18")]);
        let left: Vec<String> = calendar
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|o| o["meeting_date"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(left, ["2026-11-11"]);
    }

    #[tokio::test]
    async fn a_meeting_is_named_by_its_calendar() {
        let (server, calendar) = fedocal(true).await;
        let client = Client::with_base_url(&server.uri());
        let yes = |_: &str, _: bool| Ok(true);
        let mut a = args(&["2026-11-11"]);
        a.target = "hs".into();
        a.meeting = Some("hyperscale".into());
        let done = run(&client, &a, yes, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.cancelled, [day("2026-11-11")]);
        assert_eq!(calendar.0.lock().unwrap().len(), 2);
        // A title the calendar does not have: found out before logging
        // in, which would panic here.
        a.meeting = Some("eln".into());
        a.dates = vec![day("2026-11-04")];
        let no_login = || -> Result<Session, sandogasa_fedocal::Error> { panic!("no login") };
        let err = run(&client, &a, yes, no_login).await.unwrap_err();
        assert!(
            err.starts_with("no meeting in `hs` on 2026-11-04 is titled like \"eln\""),
            "{err}"
        );
        // An entry names its meeting already.
        a.target = "11".into();
        let err = run(&client, &a, yes, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(
            err.starts_with("--meeting picks a meeting in a calendar"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn nothing_is_cancelled_unasked_or_unchecked() {
        let (server, _) = fedocal(false).await;
        let client = Client::with_base_url(&server.uri());
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        // A dry run, a "no", a day without an occurrence, no day at all.
        let mut a = args(&["2026-11-11"]);
        a.dry_run = true;
        a.json = true;
        let done = run(&client, &a, asked, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.outcome, "dry_run");
        let no = |_: &str, _: bool| Ok(false);
        let done = run(
            &client,
            &args(&["2026-11-11"]),
            no,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "declined");
        let err = run(
            &client,
            &args(&["2026-11-12"]),
            no,
            fake_login(server.uri()),
        )
        .await
        .unwrap_err();
        assert!(err.contains("has no occurrence on 2026-11-12"), "{err}");
        let err = run(&client, &args(&[]), no, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(err.starts_with("give the days"), "{err}");
        // fedocal redirects as if it had, but the occurrence is still
        // there: an error, not "cancelled".
        let yes = |_: &str, _: bool| Ok(true);
        let err = run(
            &client,
            &args(&["2026-11-11"]),
            yes,
            fake_login(server.uri()),
        )
        .await
        .unwrap_err();
        assert!(
            err.starts_with("Fedora Calendar did not cancel the occurrence on 2026-11-11"),
            "{err}"
        );
        assert_eq!(
            list_dates(&[day("2026-11-04"), day("2026-11-11")]),
            "2026-11-04, 2026-11-11"
        );
    }
}
