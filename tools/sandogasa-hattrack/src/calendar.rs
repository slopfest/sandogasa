// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fedora Calendar's vacation calendar: a contributor's planned absences,
//! shown by `last-seen` and `group`, and `away`, which files one's own.

use chrono::{Duration, NaiveDate};
use sandogasa_fedocal::session::{NewEntry, Session};
use sandogasa_fedocal::{Client, Meeting, VACATION};

use crate::ServiceLastSeen;

/// The service's name in `last-seen`.
pub const SERVICE: &str = "Calendar";

/// How far ahead `last-seen` looks for a planned absence.
pub const LOOKAHEAD_DAYS: i64 = 90;

/// What `away` was asked to file.
pub struct AwayArgs {
    pub first: NaiveDate,
    pub last: NaiveDate,
    pub title: Option<String>,
    pub note: Option<String>,
    pub dry_run: bool,
    pub yes: bool,
    /// Print the outcome as JSON, the messages going to stderr.
    pub json: bool,
}

/// What `away` did, for `--json`.
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
pub struct AwayOutcome {
    /// `already_filed`, `declined`, `dry_run` or `filed`.
    pub outcome: &'static str,
    pub first: NaiveDate,
    pub last: NaiveDate,
    /// The entry's page: the one filed, or the one covering the days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// `user`'s absences from `today` to [`LOOKAHEAD_DAYS`] ahead, as a
/// `last-seen` row: the current or next one as the status, any later
/// ones in the detail. No status and no error when none is filed.
pub async fn check_calendar(client: &Client, user: &str, today: NaiveDate) -> ServiceLastSeen {
    let mut row = ServiceLastSeen {
        service: SERVICE.to_string(),
        ..Default::default()
    };
    let until = today + Duration::days(LOOKAHEAD_DAYS);
    match client.absences(user, today, until).await {
        Ok(found) => {
            let mut lines = found.iter().map(|m| describe(m, user, today));
            // The status says until when; no separate expiry, whose
            // format is Discourse's.
            row.status = lines.next();
            let later: Vec<String> = lines.collect();
            if !later.is_empty() {
                row.detail = Some(format!("then {}", later.join("; ")));
            }
        }
        Err(e) => row.error = Some(e.to_string()),
    }
    row
}

/// Whether a `last-seen` row says nothing: no absence filed, and no
/// error. Such a row is left out.
pub fn is_empty(row: &ServiceLastSeen) -> bool {
    row.service == SERVICE && row.status.is_none() && row.error.is_none()
}

/// File `user`'s absence: check what is filed already, show the entry,
/// log in with the Kerberos ticket, confirm, add it, and read it back.
/// `confirm` asks at the terminal; `login` gets a ticket if needed and
/// logs in (blocking), so nothing asks for a password before the days
/// are known to need filing.
pub async fn away(
    client: &Client,
    user: &str,
    args: &AwayArgs,
    confirm: impl Fn(&str, bool) -> Result<bool, String>,
    login: impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> + Send + 'static,
) -> Result<AwayOutcome, String> {
    // Under --json, stdout carries only the outcome.
    let say = |line: String| {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    let outcome = |outcome, url| AwayOutcome {
        outcome,
        first: args.first,
        last: args.last,
        url,
    };
    if args.last < args.first {
        return Err(format!(
            "the last day ({}) is before the first ({})",
            args.last, args.first
        ));
    }
    // The API returns the entries overlapping the days asked about, an
    // absence under way included (checked against the live calendar).
    let filed = client
        .absences(user, args.first, args.last)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(m) = filed
        .iter()
        .find(|m| m.meeting_date <= args.first && m.last_day() >= args.last)
    {
        let url = entry_url(client, m);
        say(format!("Already filed: {} ({url}).", describe_span(m)));
        return Ok(outcome("already_filed", Some(url)));
    }
    if !filed.is_empty() {
        say("Filed already, overlapping:".to_string());
        for m in &filed {
            say(format!("  {} ({})", describe_span(m), entry_url(client, m)));
        }
        // A dry run files nothing, so there is nothing to decide yet.
        if !args.dry_run && !args.yes && !confirm("File another entry anyway?", false)? {
            say("Nothing filed.".to_string());
            return Ok(outcome("declined", None));
        }
    }
    let mut entry = NewEntry::full_days(
        VACATION,
        args.title.clone().unwrap_or_else(|| user.to_string()),
        args.first,
        args.last,
    );
    if let Some(note) = &args.note {
        entry = entry.information(note.clone());
    }
    say(preview(client, &entry));

    eprintln!("Logging in to Fedora Calendar with your Kerberos ticket...");
    let session = tokio::task::spawn_blocking(login)
        .await
        .map_err(|e| format!("the login stopped: {e}"))?
        .map_err(|e| e.to_string())?;
    session.can_add(VACATION).await.map_err(|e| e.to_string())?;
    if args.dry_run {
        say("Dry run: you may add to the vacation calendar; nothing filed.".to_string());
        return Ok(outcome("dry_run", None));
    }
    if !args.yes && !confirm("File this absence?", true)? {
        say("Nothing filed.".to_string());
        return Ok(outcome("declined", None));
    }
    session.add(&entry).await.map_err(|e| e.to_string())?;
    // Read back, so "filed" means the calendar shows it.
    let now_filed = client
        .absences(user, args.first, args.last)
        .await
        .map_err(|e| format!("filed, but cannot read it back: {e}"))?;
    match now_filed
        .iter()
        .find(|m| m.meeting_date == args.first && m.last_day() == args.last)
    {
        Some(m) => {
            let url = entry_url(client, m);
            say(format!("Filed: {url}"));
            Ok(outcome("filed", Some(url)))
        }
        None => Err(format!(
            "Fedora Calendar accepted the entry but does not list it yet; \
             check {}/{VACATION}/",
            client.base_url()
        )),
    }
}

/// The entry as it will be filed, every field shown.
fn preview(client: &Client, entry: &NewEntry) -> String {
    let mut out = format!(
        "To file in {}/{}/:\n  title:     {}\n  days:      {} to {} \
         (full days, {} day(s))\n",
        client.base_url(),
        entry.calendar,
        entry.name,
        entry.first_day,
        entry.last_day,
        (entry.last_day - entry.first_day).num_days() + 1
    );
    if !entry.information.is_empty() {
        out.push_str(&format!("  note:      {}\n", entry.information));
    }
    out
}

/// One absence for `last-seen`: current ("away until") or coming
/// ("away from ... to ..."), with its title when it says more than the
/// user's name.
fn describe(m: &Meeting, user: &str, today: NaiveDate) -> String {
    let when = if m.meeting_date <= today {
        format!("away until {}", m.last_day())
    } else {
        format!("away {} to {}", m.meeting_date, m.last_day())
    };
    if m.meeting_name.is_empty() || m.meeting_name == user {
        when
    } else {
        format!("{when} ({})", m.meeting_name)
    }
}

/// An absence's days, with its title.
fn describe_span(m: &Meeting) -> String {
    format!(
        "{} to {}, \"{}\"",
        m.meeting_date,
        m.last_day(),
        m.meeting_name
    )
}

/// The web page of an entry.
fn entry_url(client: &Client, m: &Meeting) -> String {
    format!("{}/meeting/{}/", client.base_url(), m.meeting_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn entry(id: u64, who: &str, title: &str, from: &str, end: &str) -> serde_json::Value {
        serde_json::json!({
            "meeting_id": id, "calendar_name": "vacation", "meeting_name": title,
            "meeting_date": from, "meeting_date_end": end,
            "meeting_time_start": "00:00:00", "meeting_time_stop": "00:00:00",
            "meeting_timezone": "UTC", "meeting_manager": [who]
        })
    }

    async fn calendar(entries: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"arguments": {}, "meetings": entries})),
            )
            .mount(&server)
            .await;
        server
    }

    #[tokio::test]
    async fn last_seen_shows_the_current_absence_and_later_ones() {
        let server = calendar(serde_json::json!([
            entry(1, "me", "me", "2026-11-24", "2027-01-04"),
            entry(2, "me", "FOSDEM", "2027-01-30", "2027-02-02"),
        ]))
        .await;
        let client = Client::with_base_url(&server.uri());
        let row = check_calendar(&client, "me", day("2026-12-01")).await;
        assert_eq!(row.service, "Calendar");
        assert_eq!(row.status.as_deref(), Some("away until 2027-01-03"));
        assert_eq!(row.status_expires, None);
        assert_eq!(
            row.detail.as_deref(),
            Some("then away 2027-01-30 to 2027-02-01 (FOSDEM)")
        );
        assert!(!is_empty(&row));
        // Nothing filed: an empty row, left out.
        let none = calendar(serde_json::json!([])).await;
        let row =
            check_calendar(&Client::with_base_url(&none.uri()), "me", day("2026-12-01")).await;
        assert!(is_empty(&row));
        // A failing calendar: an error row.
        let row = check_calendar(
            &Client::with_base_url("http://127.0.0.1:9"),
            "me",
            day("2026-12-01"),
        )
        .await;
        assert!(row.error.is_some());
    }

    fn args(first: &str, last: &str) -> AwayArgs {
        AwayArgs {
            first: day(first),
            last: day(last),
            title: None,
            note: Some("Back on the 5th".into()),
            dry_run: false,
            yes: false,
            json: false,
        }
    }

    fn no_login() -> Result<Session, sandogasa_fedocal::Error> {
        panic!("no login expected")
    }

    #[tokio::test]
    async fn an_absence_filed_already_is_not_filed_again() {
        let server = calendar(serde_json::json!([entry(
            7,
            "me",
            "me",
            "2026-11-24",
            "2027-01-04"
        )]))
        .await;
        let client = Client::with_base_url(&server.uri());
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        let done = away(
            &client,
            "me",
            &args("2026-12-21", "2027-01-03"),
            asked,
            no_login,
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "already_filed");
        assert_eq!(
            done.url.as_deref(),
            Some(format!("{}/meeting/7/", server.uri()).as_str())
        );
        // Overlapping only in part: asked, and "no" files nothing.
        let declined = |q: &str, default: bool| {
            assert_eq!(q, "File another entry anyway?");
            assert!(!default);
            Ok(false)
        };
        let done = away(
            &client,
            "me",
            &args("2027-01-02", "2027-01-10"),
            declined,
            no_login,
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "declined");
        assert_eq!(
            serde_json::to_value(&done).unwrap(),
            serde_json::json!({"outcome": "declined", "first": "2027-01-02", "last": "2027-01-10"})
        );
        // Days the wrong way round.
        let err = away(
            &client,
            "me",
            &args("2027-01-10", "2027-01-02"),
            asked,
            no_login,
        )
        .await
        .unwrap_err();
        assert!(err.contains("is before the first"), "{err}");
    }

    #[tokio::test]
    async fn an_absence_is_filed_after_a_confirmation_and_read_back() {
        let server = MockServer::start().await;
        // Nothing filed at first, then the new entry.
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"arguments": {}, "meetings": []})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "arguments": {},
                "meetings": [entry(9, "me", "me", "2026-12-21", "2027-01-04")]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<input name="csrf_token" value="t">"#),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/vacation/add/"))
            .and(wiremock::matchers::body_string_contains(
                "information=Back+on+the+5th",
            ))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "/vacation/2026/12/21/"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let uri = server.uri();
        let login = move || {
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
        };
        let yes = |q: &str, default: bool| {
            assert_eq!(q, "File this absence?");
            assert!(default);
            Ok(true)
        };
        let done = away(&client, "me", &args("2026-12-21", "2027-01-03"), yes, login)
            .await
            .unwrap();
        assert_eq!(done.outcome, "filed");
        assert!(done.url.unwrap().ends_with("/meeting/9/"));
    }

    #[tokio::test]
    async fn a_dry_run_lists_an_overlap_without_asking() {
        let server = calendar(serde_json::json!([entry(
            7,
            "me",
            "me",
            "2026-12-28",
            "2027-01-04"
        )]))
        .await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<input name="csrf_token" value="t">"#),
            )
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let uri = server.uri();
        let login = move || {
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
        };
        let mut a = args("2026-12-21", "2027-01-10");
        a.dry_run = true;
        a.json = true;
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        assert_eq!(
            away(&client, "me", &a, asked, login).await.unwrap().outcome,
            "dry_run"
        );
    }

    #[tokio::test]
    async fn a_dry_run_logs_in_but_files_nothing() {
        let server = calendar(serde_json::json!([])).await;
        Mock::given(method("GET"))
            .and(path("/vacation/add/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"<input name="csrf_token" value="t">"#),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302))
            .expect(0)
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let uri = server.uri();
        let login = move || {
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
        };
        let mut a = args("2026-12-21", "2027-01-03");
        a.dry_run = true;
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        assert_eq!(
            away(&client, "me", &a, asked, login).await.unwrap().outcome,
            "dry_run"
        );
        let entry = NewEntry::full_days(VACATION, "me", day("2026-12-21"), day("2027-01-03"))
            .information("note");
        let shown = preview(&client, &entry);
        assert!(
            shown.contains("days:      2026-12-21 to 2027-01-03 (full days, 14 day(s))"),
            "{shown}"
        );
        assert!(shown.contains("note:      note"), "{shown}");
    }
}
