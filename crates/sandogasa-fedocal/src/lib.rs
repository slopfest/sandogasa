// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Client for [Fedora Calendar][fedocal] (fedocal): reading its calendars'
//! entries through the public JSON API, and adding entries as a logged-in
//! user ([`session`]).
//!
//! fedocal's API only reads. Entries are added through its web form, so
//! [`session::Session`] logs in the way a browser does, through
//! id.fedoraproject.org, but with a Kerberos ticket (`fkinit`) instead of
//! a password.
//!
//! [fedocal]: https://calendar.fedoraproject.org/
//!
//! ```no_run
//! use sandogasa_fedocal::{Client, VACATION};
//!
//! # async fn run() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::new();
//! let from = chrono::NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
//! let to = chrono::NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
//! for away in client.absences("salimma", from, to).await? {
//!     println!("{} to {}", away.meeting_date, away.last_day());
//! }
//! # Ok(())
//! # }
//! ```

pub mod session;

use chrono::NaiveDate;
use serde::Deserialize;

/// Fedora Calendar's production instance.
pub const DEFAULT_BASE_URL: &str = "https://calendar.fedoraproject.org";

/// The calendar contributors record their planned time off in.
pub const VACATION: &str = "vacation";

/// One entry of a calendar, as the API gives it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[non_exhaustive]
pub struct Meeting {
    pub meeting_id: u64,
    pub calendar_name: String,
    pub meeting_name: String,
    /// The first day.
    pub meeting_date: NaiveDate,
    /// The day it ends on; for a full-day entry, the day after its last
    /// (see [`Meeting::last_day`]).
    pub meeting_date_end: NaiveDate,
    /// `HH:MM:SS` in `meeting_timezone`.
    pub meeting_time_start: String,
    pub meeting_time_stop: String,
    pub meeting_timezone: String,
    /// The FAS usernames that manage the entry: for a vacation entry,
    /// whose absence it is.
    #[serde(default)]
    pub meeting_manager: Vec<String>,
    #[serde(default)]
    pub meeting_information: String,
    #[serde(default)]
    pub meeting_location: String,
}

/// Which entries [`Client::meetings`] asks for. Any field left `None` is
/// not filtered on; fedocal's own default range is the coming weeks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct MeetingQuery {
    pub calendar: Option<String>,
    pub location: Option<String>,
    /// Entries overlapping `start..=end` are returned.
    pub start: Option<NaiveDate>,
    pub end: Option<NaiveDate>,
}

/// An error from Fedora Calendar.
#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// The request failed, or the answer could not be read.
    Http(String),
    /// Logging in did not succeed (no ticket, no curl, a refused
    /// ticket).
    Login(String),
    /// fedocal refused what was submitted, with its reasons.
    Refused(Vec<String>),
}

/// Fedora Calendar's API.
#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    base_url: String,
}

impl Meeting {
    /// Whether it takes whole days: fedocal stores those from 00:00 to
    /// 00:00 UTC.
    pub fn is_full_day(&self) -> bool {
        self.meeting_time_start.starts_with("00:00")
            && self.meeting_time_stop.starts_with("00:00")
            && self.meeting_date_end > self.meeting_date
    }

    /// The last day it takes: for a full-day entry the day before
    /// `meeting_date_end`, which fedocal stores as the day after.
    pub fn last_day(&self) -> NaiveDate {
        if self.is_full_day() {
            self.meeting_date_end
                .pred_opt()
                .unwrap_or(self.meeting_date_end)
        } else {
            self.meeting_date_end
        }
    }

    /// Whether it covers any of `from..=to`.
    pub fn overlaps(&self, from: NaiveDate, to: NaiveDate) -> bool {
        self.meeting_date <= to && self.last_day() >= from
    }
}

impl MeetingQuery {
    pub fn calendar(mut self, calendar: impl Into<String>) -> Self {
        self.calendar = Some(calendar.into());
        self
    }

    pub fn location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    pub fn between(mut self, start: NaiveDate, end: NaiveDate) -> Self {
        self.start = Some(start);
        self.end = Some(end);
        self
    }

    /// The query string the API takes.
    fn params(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(c) = &self.calendar {
            out.push(("calendar", c.clone()));
        }
        if let Some(l) = &self.location {
            out.push(("location", l.clone()));
        }
        if let Some(d) = self.start {
            out.push(("start", d.to_string()));
        }
        if let Some(d) = self.end {
            out.push(("end", d.to_string()));
        }
        out
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Http(e) => write!(f, "Fedora Calendar: {e}"),
            Error::Login(e) => write!(f, "cannot log in to Fedora Calendar: {e}"),
            Error::Refused(reasons) if reasons.is_empty() => {
                write!(f, "Fedora Calendar refused the entry, without saying why")
            }
            Error::Refused(reasons) => {
                write!(
                    f,
                    "Fedora Calendar refused the entry: {}",
                    reasons.join("; ")
                )
            }
        }
    }
}

impl std::error::Error for Error {}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// The production instance.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Another instance, such as a test server.
    pub fn with_base_url(base_url: &str) -> Self {
        Self {
            http: user_agent_client(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// The instance's base URL, without a trailing slash.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The entries `query` selects.
    pub async fn meetings(&self, query: &MeetingQuery) -> Result<Vec<Meeting>, Error> {
        #[derive(Deserialize)]
        struct Answer {
            meetings: Vec<Meeting>,
        }
        let url = format!("{}/api/meetings/", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&query.params())
            .send()
            .await
            .map_err(|e| Error::Http(format!("GET {url}: {e}")))?;
        let answer: Answer = sandogasa_cli::http::json_ok(resp, &format!("GET {url}"))
            .await
            .map_err(Error::Http)?;
        Ok(answer.meetings)
    }

    /// `user`'s entries in the vacation calendar that cover any of
    /// `from..=to`, earliest first.
    pub async fn absences(
        &self,
        user: &str,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<Meeting>, Error> {
        let query = MeetingQuery::default().calendar(VACATION).between(from, to);
        let mut found: Vec<Meeting> = self
            .meetings(&query)
            .await?
            .into_iter()
            .filter(|m| m.meeting_manager.iter().any(|u| u == user))
            .filter(|m| m.overlaps(from, to))
            .collect();
        found.sort_by_key(|m| (m.meeting_date, m.meeting_id));
        found.dedup_by_key(|m| m.meeting_id);
        Ok(found)
    }
}

/// An async client with sandogasa's user agent and timeout.
fn user_agent_client() -> reqwest::Client {
    sandogasa_cli::http::builder(concat!(
        env!("CARGO_PKG_NAME"),
        "/",
        env!("CARGO_PKG_VERSION")
    ))
    .build()
    .expect("build reqwest client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn entry(id: u64, who: &str, from: &str, end: &str, start: &str) -> serde_json::Value {
        serde_json::json!({
            "meeting_id": id, "calendar_name": "vacation", "meeting_name": "PTO",
            "meeting_date": from, "meeting_date_end": end,
            "meeting_time_start": start, "meeting_time_stop": "00:00:00",
            "meeting_timezone": "UTC", "meeting_manager": [who],
            "meeting_information": "", "meeting_location": ""
        })
    }

    #[test]
    fn a_full_day_entry_ends_the_day_before_its_stored_end() {
        let m: Meeting =
            serde_json::from_value(entry(1, "me", "2026-11-12", "2026-11-23", "00:00:00")).unwrap();
        assert!(m.is_full_day());
        assert_eq!(m.last_day(), day("2026-11-22"));
        assert!(m.overlaps(day("2026-11-22"), day("2026-12-01")));
        assert!(!m.overlaps(day("2026-11-23"), day("2026-12-01")));
        // A timed entry ends on its end date.
        let timed: Meeting =
            serde_json::from_value(entry(2, "me", "2026-11-12", "2026-11-12", "15:00:00")).unwrap();
        assert!(!timed.is_full_day());
        assert_eq!(timed.last_day(), day("2026-11-12"));
    }

    #[tokio::test]
    async fn absences_are_a_users_vacation_entries_in_the_period() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/meetings/"))
            .and(query_param("calendar", "vacation"))
            .and(query_param("start", "2026-11-23"))
            .and(query_param("end", "2026-12-31"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "arguments": {},
                "meetings": [
                    entry(3, "me", "2026-12-20", "2027-01-06", "00:00:00"),
                    entry(1, "me", "2026-11-12", "2026-11-23", "00:00:00"),
                    entry(2, "someone", "2026-12-01", "2026-12-05", "00:00:00"),
                    entry(3, "me", "2026-12-20", "2027-01-06", "00:00:00"),
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let client = Client::with_base_url(&format!("{}/", server.uri()));
        let found = client
            .absences("me", day("2026-11-23"), day("2026-12-31"))
            .await
            .unwrap();
        // Entry 1 ended on the 22nd; entry 2 is someone else's; entry 3
        // once, though listed twice.
        let ids: Vec<u64> = found.iter().map(|m| m.meeting_id).collect();
        assert_eq!(ids, [3]);
        assert_eq!(found[0].last_day(), day("2027-01-05"));
    }

    #[tokio::test]
    async fn a_failing_calendar_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let client = Client::with_base_url(&server.uri());
        let err = client
            .meetings(&MeetingQuery::default().location("room"))
            .await
            .unwrap_err();
        assert!(
            err.to_string().starts_with("Fedora Calendar: GET "),
            "{err}"
        );
        assert_eq!(client.base_url(), server.uri());
    }

    #[test]
    fn errors_say_what_failed() {
        assert_eq!(
            Error::Refused(vec![]).to_string(),
            "Fedora Calendar refused the entry, without saying why"
        );
        assert_eq!(
            Error::Refused(vec!["a".into(), "b".into()]).to_string(),
            "Fedora Calendar refused the entry: a; b"
        );
        assert_eq!(
            Error::Login("no ticket".into()).to_string(),
            "cannot log in to Fedora Calendar: no ticket"
        );
    }
}
