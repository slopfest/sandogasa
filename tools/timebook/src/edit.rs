// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `timebook edit`: change one of your own Fedora Calendar entries (its
//! title, location, note, or a series' last day) through its edit form,
//! as `hattrack away --update` changes an absence.
//!
//! For a recurring entry fedocal's form shows the series from its next
//! occurrence; saving it for the whole series ("Save all") changes the
//! series from there on, fedocal keeping the earlier occurrences as a
//! separate entry, while `--one` changes that occurrence alone ("Save"),
//! the series going on without it.

use chrono::{Duration, NaiveDate, Utc};
use sandogasa_fedocal::session::{Changes, EntryForm, Session};
use sandogasa_fedocal::{Client, MeetingQuery};
use serde::Serialize;

use crate::pick::Named;

/// How far ahead a meeting named by its calendar is looked for: a
/// quarter, which holds the next occurrence of a monthly meeting.
const COMING_DAYS: i64 = 92;

/// What `edit` was asked to change.
pub struct EditArgs {
    /// The entry: its id or its page's URL, or its meeting's calendar,
    /// for the entry of the meeting's next occurrence.
    pub target: String,
    /// In a calendar with several meetings, part of the title of the one
    /// meant.
    pub meeting: Option<String>,
    pub title: Option<String>,
    pub location: Option<String>,
    pub note: Option<String>,
    /// A series' last day.
    pub until: Option<NaiveDate>,
    /// Change the next occurrence only.
    pub one: bool,
    pub dry_run: bool,
    pub yes: bool,
    /// Print the outcome as JSON, the messages going to stderr.
    pub json: bool,
}

/// What `edit` did, for `--json`.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct EditOutcome {
    /// `changed`, `declined`, `dry_run` or `unchanged`.
    pub outcome: &'static str,
    pub url: String,
}

/// The id of the entry `entry` names: `11067`, or its page,
/// `https://calendar.fedoraproject.org/meeting/11067/` (or its edit
/// page).
pub fn entry_id(entry: &str) -> Result<u64, String> {
    // A link to one occurrence carries `?from_date=...`.
    let entry = entry
        .trim()
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_end_matches('/');
    let last = entry.rsplit('/').next().unwrap_or(entry);
    last.parse()
        .map_err(|_| format!("`{entry}` is not an entry's id or page (…/meeting/<id>/)"))
}

/// The changes `args` ask for that the entry in `form` does not have
/// already, and the series scope; an error when the request makes no
/// sense for it.
pub fn planned(args: &EditArgs, form: &EntryForm) -> Result<Changes, String> {
    if args.until.is_some() && !form.recurring() {
        return Err("--until is the last day of a series, and this entry does not repeat".into());
    }
    if args.one && !form.recurring() {
        return Err(
            "--one changes one occurrence of a series, and this entry does not repeat".into(),
        );
    }
    if args.one && args.until.is_some() {
        return Err("--until ends a whole series, so it does not go with --one".into());
    }
    if let (Some(until), Some(next)) = (
        args.until,
        form.get("meeting_date")
            .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()),
    ) && until < next
    {
        return Err(format!(
            "--until {until} is before the series' next occurrence, {next}"
        ));
    }
    let differs = |field: &str, wanted: &str| !same(form.get(field).unwrap_or(""), wanted);
    let mut changes = Changes::default().whole_series(!args.one);
    if let Some(t) = args.title.as_deref().filter(|t| differs("meeting_name", t)) {
        changes = changes.name(t);
    }
    if let Some(l) = args
        .location
        .as_deref()
        .filter(|l| differs("meeting_location", l))
    {
        changes = changes.location(l);
    }
    if let Some(n) = args.note.as_deref().filter(|n| differs("information", n)) {
        changes = changes.information(n);
    }
    if let Some(u) = args
        .until
        .filter(|u| differs("end_repeats", &u.to_string()))
    {
        changes = changes.until(u);
    }
    Ok(changes)
}

/// Whether `form` shows every change in `changes`: how an edit is read
/// back. (After `--one` the entry no longer repeats, so the checks of
/// [`planned`] would not do.)
pub fn shows(form: &EntryForm, changes: &Changes) -> bool {
    let has = |field: &str, wanted: &Option<String>| {
        wanted
            .as_deref()
            .is_none_or(|w| same(form.get(field).unwrap_or(""), w))
    };
    has("meeting_name", &changes.name)
        && has("meeting_location", &changes.location)
        && has("information", &changes.information)
        && has("end_repeats", &changes.until.map(|u| u.to_string()))
}

/// Whether two field values are the same, line breaks and surrounding
/// blanks aside: a browser sends a textarea's line breaks as CRLF.
fn same(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace("\r\n", "\n").trim().to_string();
    norm(a) == norm(b)
}

/// What the edit covers, and each change before and after.
pub fn describe(form: &EntryForm, changes: &Changes, url: &str) -> String {
    let shown = |s: &str| {
        if s.trim().is_empty() {
            "(none)".to_string()
        } else {
            format!("\"{}\"", s.trim())
        }
    };
    let next = form.get("meeting_date").unwrap_or("?");
    let mut out = format!("Changes to {url}:\n");
    if form.recurring() {
        out.push_str(&if changes.whole_series {
            format!(
                "  covers:    the series from its next occurrence, {next}, on; fedocal keeps \
                 the earlier ones as a separate entry\n"
            )
        } else {
            format!("  covers:    only the occurrence on {next}; the series goes on without it\n")
        });
    }
    let mut line = |label: &str, field: &str, new: &str| {
        out.push_str(&format!(
            "  {label:<10} {} -> {}\n",
            shown(form.get(field).unwrap_or("")),
            shown(new)
        ));
    };
    if let Some(t) = &changes.name {
        line("title:", "meeting_name", t);
    }
    if let Some(l) = &changes.location {
        line("location:", "meeting_location", l);
    }
    if let Some(n) = &changes.information {
        line("note:", "information", n);
    }
    if let Some(u) = changes.until {
        line("until:", "end_repeats", &u.to_string());
    }
    out
}

/// The id of the entry `target` names: an entry's id or page, or a
/// calendar, for the entry of the next occurrence in the coming
/// quarter of the meeting `meeting` picks by title (any, when the
/// calendar holds one).
pub async fn entry_named(
    client: &Client,
    target: &str,
    meeting: Option<&str>,
) -> Result<u64, String> {
    match crate::pick::named(target)? {
        Named::Entry(_) if meeting.is_some() => {
            Err("--meeting picks a meeting in a calendar; an entry names one already".into())
        }
        Named::Entry(id) => Ok(id),
        Named::Calendar(calendar) => {
            let now = Utc::now();
            let today = now.date_naive();
            let meetings = client
                .meetings(
                    &MeetingQuery::default()
                        .calendar(calendar.clone())
                        .between(today, today + Duration::days(COMING_DAYS)),
                )
                .await
                .map_err(|e| e.to_string())?;
            let when = format!("in the next {COMING_DAYS} days");
            crate::pick::next_entry(&meetings, &calendar, meeting, now, &when).map_err(|e| {
                // Not found: perhaps a meeting of the past.
                if e.starts_with("no ") || e.starts_with("calendar ") {
                    format!("{e} (an entry's id or page names a past one)")
                } else {
                    e
                }
            })
        }
    }
}

/// Change the entry `args` names: log in, read it, show the changes,
/// confirm, save, and read it back. `confirm` asks at the terminal;
/// `login` logs in (blocking).
pub async fn run(
    client: &Client,
    args: &EditArgs,
    confirm: impl Fn(&str, bool) -> Result<bool, String>,
    login: impl FnOnce() -> Result<Session, sandogasa_fedocal::Error> + Send + 'static,
) -> Result<EditOutcome, String> {
    let say = |line: String| {
        if args.json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    if args.title.is_none()
        && args.location.is_none()
        && args.note.is_none()
        && args.until.is_none()
    {
        return Err("nothing to change: give --title, --location, --note or --until".into());
    }
    let id = entry_named(client, &args.target, args.meeting.as_deref()).await?;
    let url = format!("{}/meeting/{id}/", client.base_url());
    let outcome = |outcome| EditOutcome {
        outcome,
        url: url.clone(),
    };
    eprintln!("Logging in to Fedora Calendar with your Kerberos ticket...");
    let session = tokio::task::spawn_blocking(login)
        .await
        .map_err(|e| format!("the login stopped: {e}"))?
        .map_err(|e| e.to_string())?;
    let form = session.entry(id).await.map_err(|e| e.to_string())?;
    let changes = planned(args, &form)?;
    if changes.is_empty() {
        say(format!("Nothing to change: {url} is as asked."));
        return Ok(outcome("unchanged"));
    }
    say(describe(&form, &changes, &url));
    if args.dry_run {
        say("Dry run: you may edit the entry; nothing changed.".to_string());
        return Ok(outcome("dry_run"));
    }
    if !args.yes && !confirm("Save these changes?", true)? {
        say("Nothing changed.".to_string());
        return Ok(outcome("declined"));
    }
    session
        .edit(id, &changes)
        .await
        .map_err(|e| e.to_string())?;
    // Read back, so "changed" means the calendar shows it.
    let now = session
        .entry(id)
        .await
        .map_err(|e| format!("saved, but cannot read it back: {e}"))?;
    if !shows(&now, &changes) {
        return Err(format!(
            "Fedora Calendar accepted the change but does not show it yet; check {url}"
        ));
    }
    say(format!("Changed: {url}"));
    Ok(outcome("changed"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn args(entry: &str) -> EditArgs {
        EditArgs {
            target: entry.into(),
            meeting: None,
            title: None,
            location: None,
            note: None,
            until: None,
            one: false,
            dry_run: false,
            yes: false,
            json: false,
        }
    }

    fn form(recurring: bool, location: &str) -> EntryForm {
        let mut f = EntryForm::default();
        f.fields = vec![
            ("meeting_name".into(), "Hangout".into()),
            ("meeting_date".into(), "2026-12-02".into()),
            ("meeting_location".into(), location.into()),
            ("information".into(), "".into()),
            ("end_repeats".into(), "2037-12-31".into()),
        ];
        f.submits = if recurring {
            vec!["Save".into(), "Save all".into()]
        } else {
            vec!["Save".into()]
        };
        f
    }

    #[test]
    fn an_entry_is_named_by_its_id_or_page() {
        assert_eq!(entry_id("11067").unwrap(), 11067);
        assert_eq!(
            entry_id("https://calendar.fedoraproject.org/meeting/11067/").unwrap(),
            11067
        );
        assert_eq!(
            entry_id("https://calendar.fedoraproject.org/meeting/edit/11067").unwrap(),
            11067
        );
        assert_eq!(
            entry_id("https://calendar.fedoraproject.org/meeting/11171/?from_date=2026-12-02")
                .unwrap(),
            11171
        );
        assert!(
            entry_id("hangout")
                .unwrap_err()
                .contains("not an entry's id")
        );
    }

    #[test]
    fn only_what_differs_is_changed_and_the_scope_is_said() {
        let mut a = args("1");
        a.location = Some("https://meet.example/x".into());
        a.title = Some("Hangout".into());
        a.until = NaiveDate::from_ymd_opt(2027, 6, 30);
        let f = form(true, "");
        let c = planned(&a, &f).unwrap();
        assert!(c.name.is_none());
        assert_eq!(c.location.as_deref(), Some("https://meet.example/x"));
        assert!(c.whole_series);
        assert_eq!(
            describe(&f, &c, "U"),
            "Changes to U:\n  covers:    the series from its next occurrence, 2026-12-02, on; \
             fedocal keeps the earlier ones as a separate entry\n  location:  (none) -> \
             \"https://meet.example/x\"\n  until:     \"2037-12-31\" -> \"2027-06-30\"\n"
        );
        a.until = None;
        a.one = true;
        a.note = Some("Special".into());
        let c = planned(&a, &f).unwrap();
        assert!(!c.whole_series);
        assert!(describe(&f, &c, "U").contains("only the occurrence on 2026-12-02"));
        assert!(describe(&f, &c, "U").contains("  note:      (none) -> \"Special\"\n"));
        // --until with --one, or before the next occurrence: refused.
        let mut both = args("1");
        both.one = true;
        both.until = NaiveDate::from_ymd_opt(2027, 6, 30);
        assert!(
            planned(&both, &f)
                .unwrap_err()
                .contains("does not go with --one")
        );
        both.one = false;
        both.until = NaiveDate::from_ymd_opt(2026, 11, 1);
        assert!(
            planned(&both, &f)
                .unwrap_err()
                .contains("before the series' next occurrence")
        );
        // Line breaks as a browser sends them are the same note.
        assert!(same("a\r\nb ", "a\nb"));
        let mut read_back = form(false, "x");
        for field in read_back.fields.iter_mut() {
            if field.0 == "information" {
                field.1 = "a\r\nb".into();
            }
        }
        let c = Changes::default().location("x").information("a\nb");
        assert!(shows(&read_back, &c));
        assert!(!shows(&read_back, &Changes::default().name("other")));
        // A single entry: no series to end, or occurrence to single out.
        let single = form(false, "");
        let mut ended = args("1");
        ended.until = NaiveDate::from_ymd_opt(2027, 6, 30);
        assert!(planned(&ended, &single).unwrap_err().contains("--until"));
        assert!(planned(&a, &single).unwrap_err().contains("--one"));
        a.one = false;
        assert!(!describe(&single, &planned(&a, &single).unwrap(), "U").contains("covers"));
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

    fn page(location: &str) -> String {
        format!(
            "<form action=\"\" method=\"post\">\
             <input name=\"meeting_name\" type=\"text\" value=\"Hangout\">\
             <input name=\"meeting_date\" type=\"date\" value=\"2026-12-02\">\
             <input name=\"meeting_location\" type=\"text\" value=\"{location}\">\
             <input name=\"end_repeats\" type=\"date\" value=\"2037-12-31\">\
             <input name=\"csrf_token\" type=\"hidden\" value=\"t\">\
             <input name=\"action\" type=\"submit\" value=\"Save\">\
             <input name=\"action\" type=\"submit\" value=\"Save all\"></form>"
        )
    }

    #[tokio::test]
    async fn a_series_location_is_set_and_read_back() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("")))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(page("https://meet.example/x")),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/meeting/edit/7/"))
            .and(body_string_contains("action=Save+all"))
            .and(body_string_contains(
                "meeting_location=https%3A%2F%2Fmeet.example%2Fx",
            ))
            .respond_with(ResponseTemplate::new(302).insert_header("location", "/meeting/7/"))
            .expect(1)
            .mount(&server)
            .await;
        let mut a = args(&format!("{}/meeting/7/", server.uri()));
        a.location = Some("https://meet.example/x".into());
        let yes = |q: &str, default: bool| {
            assert_eq!(q, "Save these changes?");
            assert!(default);
            Ok(true)
        };
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            yes,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "changed");
        assert_eq!(done.url, format!("{}/meeting/7/", server.uri()));
        // Now as asked: nothing to change, nothing asked.
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            asked,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "unchanged");
    }

    #[tokio::test]
    async fn an_entry_is_found_by_its_calendar() {
        let server = MockServer::start().await;
        let coming = |id: u64, date: &str| {
            serde_json::json!({
                "meeting_id": id, "calendar_name": "hs", "meeting_name": "Hyperscale SIG",
                "meeting_date": date, "meeting_date_end": date,
                "meeting_time_start": "16:00:00", "meeting_time_stop": "17:00:00",
                "meeting_timezone": "UTC"
            })
        };
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "arguments": {},
                "meetings": [coming(7, "2099-01-14"), coming(7, "2099-01-07")]
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("")))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        let mut a = args("hs");
        a.note = Some("Video on request".into());
        a.dry_run = true;
        let done = run(&client, &a, asked, fake_login(server.uri()))
            .await
            .unwrap();
        assert_eq!(done.url, format!("{}/meeting/7/", server.uri()));
        a.meeting = Some("eln".into());
        let err = run(&client, &a, asked, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(
            err.starts_with("no meeting in `hs` in the next 92 days is titled like \"eln\"")
                && err.ends_with("(an entry's id or page names a past one)"),
            "{err}"
        );
        a.target = "7".into();
        let err = run(&client, &a, asked, fake_login(server.uri()))
            .await
            .unwrap_err();
        assert!(
            err.starts_with("--meeting picks a meeting in a calendar"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_dry_run_or_a_no_saves_nothing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/meeting/edit/7/"))
            .respond_with(ResponseTemplate::new(200).set_body_string(page("")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(302))
            .expect(0)
            .mount(&server)
            .await;
        let mut a = args("7");
        a.note = Some("Video on request".into());
        a.dry_run = true;
        a.json = true;
        let asked = |_: &str, _: bool| -> Result<bool, String> { panic!("no question expected") };
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            asked,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "dry_run");
        a.dry_run = false;
        let no = |_: &str, _: bool| Ok(false);
        let done = run(
            &Client::with_base_url(&server.uri()),
            &a,
            no,
            fake_login(server.uri()),
        )
        .await
        .unwrap();
        assert_eq!(done.outcome, "declined");
        // Nothing asked for, or no entry named.
        let err = run(
            &Client::with_base_url(&server.uri()),
            &args("7"),
            no,
            fake_login(server.uri()),
        )
        .await
        .unwrap_err();
        assert!(err.starts_with("nothing to change"), "{err}");
        let mut bad = args("x");
        bad.note = Some("n".into());
        assert!(
            run(
                &Client::with_base_url(&server.uri()),
                &bad,
                no,
                fake_login(server.uri())
            )
            .await
            .is_err()
        );
    }
}
