// SPDX-License-Identifier: Apache-2.0 OR MIT

//! HTTP client for [meetbot.fedoraproject.org][meetbot]'s
//! meeting search endpoint.
//!
//! Meetbot exposes a single lightweight JSON endpoint for the
//! "Search for conversations" UI in its web frontend. This
//! crate wraps that endpoint behind a typed client.
//!
//! [meetbot]: https://meetbot.fedoraproject.org/
//!
//! ```no_run
//! use sandogasa_meetbot::Meetbot;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Meetbot::new();
//! for meeting in client.search("centos-hyperscale-sig")? {
//!     println!("{} {}", meeting.datetime, meeting.summary_url);
//! }
//! # Ok(())
//! # }
//! ```

use serde::Deserialize;

/// Default production base URL for meetbot's web frontend.
pub const DEFAULT_BASE_URL: &str = "https://meetbot.fedoraproject.org";

/// A single meeting discovered via the search endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meeting {
    /// Matrix room / channel the meeting was logged from.
    pub channel: String,
    /// When the meeting started.
    pub datetime: chrono::NaiveDateTime,
    /// The `!startmeeting <topic>` argument.
    pub topic: String,
    /// Public URL for the meeting's summary HTML.
    pub summary_url: String,
    /// Public URL for the meeting's full log HTML.
    pub logs_url: String,
}

/// One attendee of a meeting, as the minutes' "People Present (lines
/// said)" list them: a Matrix ID (`@salimma:fedora.im`) and how many
/// lines they said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub id: String,
    pub lines: u32,
}

/// The `.txt` twin of a meetbot `.html` artefact URL (minutes or log).
pub fn txt_url(html_url: &str) -> String {
    match html_url.strip_suffix(".html") {
        Some(base) => format!("{base}.txt"),
        None => html_url.to_string(),
    }
}

/// The fedora.im localpart of a Matrix ID — the FAS username, since
/// Fedora's homeserver issues accounts by FAS login: `@salimma:fedora.im`
/// → `salimma`. `None` for any other homeserver.
pub fn fedora_localpart(mxid: &str) -> Option<&str> {
    mxid.strip_prefix('@')?.strip_suffix(":fedora.im")
}

/// The attendees listed in a meeting's text minutes, under
/// "People Present (lines said)": one `* @id (N)` line each, up to the
/// first blank line.
pub fn parse_attendees(minutes: &str) -> Vec<Attendee> {
    let mut lines = minutes.lines();
    lines.find(|l| l.trim().starts_with("People Present"));
    lines
        .skip_while(|l| l.trim().chars().all(|c| c == '-'))
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let entry = l.trim().strip_prefix("* ")?;
            let (id, count) = entry.rsplit_once(" (")?;
            Some(Attendee {
                id: id.trim().to_string(),
                lines: count.trim_end_matches(')').parse().ok()?,
            })
        })
        .collect()
}

/// Blocking meetbot client.
pub struct Meetbot {
    http: reqwest::blocking::Client,
    base_url: String,
}

impl Default for Meetbot {
    fn default() -> Self {
        Self::new()
    }
}

impl Meetbot {
    /// Client pointed at production meetbot.
    pub fn new() -> Self {
        Self::with_base_url(DEFAULT_BASE_URL)
    }

    /// Override the search endpoint base URL (for tests).
    /// Artefact URLs returned by the search are rewritten to
    /// the same base so callers can mock the whole surface in
    /// a single wiremock server.
    pub fn with_base_url(base_url: &str) -> Self {
        let http = sandogasa_cli::http::blocking_builder(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .expect("build reqwest client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Fetch the `Content-Length` header for a meetbot artefact
    /// URL via HEAD. Used to compare log sizes when the same
    /// channel recorded multiple `!startmeeting` fragments on
    /// the same day and the longest one is taken as "the real
    /// meeting".
    pub fn content_length(&self, url: &str) -> Result<u64, Box<dyn std::error::Error>> {
        let resp = self.http.head(url).send()?;
        if !resp.status().is_success() {
            return Err(format!("meetbot HEAD {url} failed: {}", resp.status()).into());
        }
        resp.headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| format!("meetbot HEAD {url}: missing Content-Length").into())
    }

    /// Return every meeting whose topic contains `topic`.
    /// Results are whatever meetbot returns, sorted by date
    /// ascending.
    pub fn search(&self, topic: &str) -> Result<Vec<Meeting>, Box<dyn std::error::Error>> {
        let url = format!("{}/fragedpt/", self.base_url);
        let resp = self
            .http
            .get(&url)
            .query(&[("rqstdata", "srchmeet"), ("srchtext", topic)])
            .send()?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text()?;
            return Err(format!("meetbot GET {url} failed: {status}: {text}").into());
        }
        let raw: Vec<RawMeeting> = resp.json()?;
        let mut meetings: Vec<Meeting> = raw
            .into_iter()
            .filter_map(|r| r.into_meeting(&self.base_url))
            .collect();
        meetings.sort_by_key(|a| a.datetime);
        Ok(meetings)
    }

    /// Who attended `meeting`, from its text minutes.
    pub fn attendees(
        &self,
        meeting: &Meeting,
    ) -> Result<Vec<Attendee>, Box<dyn std::error::Error>> {
        let url = txt_url(&meeting.summary_url);
        let resp = self.http.get(&url).send()?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("meetbot GET {url} failed: {status}").into());
        }
        Ok(parse_attendees(&resp.text()?))
    }
}

/// Raw JSON shape as returned by meetbot's `/fragedpt/`
/// endpoint. Translated into [`Meeting`] by `into_meeting`.
#[derive(Debug, Deserialize)]
struct RawMeeting {
    channel: String,
    datetime: String,
    topic: String,
    url: RawUrls,
}

#[derive(Debug, Deserialize)]
struct RawUrls {
    logs: String,
    summary: String,
}

impl RawMeeting {
    fn into_meeting(self, artefact_base: &str) -> Option<Meeting> {
        let datetime =
            chrono::NaiveDateTime::parse_from_str(&self.datetime, "%Y-%m-%dT%H:%M:%S").ok()?;
        Some(Meeting {
            channel: self.channel,
            datetime,
            topic: self.topic,
            summary_url: rewrite_artefact_url(&self.url.summary, artefact_base),
            logs_url: rewrite_artefact_url(&self.url.logs, artefact_base),
        })
    }
}

/// Collapse same-day duplicates in a meeting list by keeping
/// the entry whose `logs_url` is the largest, as a rough proxy
/// for "the meeting that actually happened" when `!startmeeting`
/// was run multiple times (on the same channel, or across
/// overlapping channels) on a single day. `on_warning` is
/// invoked once per collapsed group with `(winner, dropped)` so
/// the caller can surface it to the user.
pub fn dedup_by_longest_log<F>(
    client: &Meetbot,
    meetings: Vec<Meeting>,
    mut on_warning: F,
) -> Vec<Meeting>
where
    F: FnMut(&Meeting, &[Meeting]),
{
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<chrono::NaiveDate, Vec<Meeting>> = BTreeMap::new();
    for m in meetings {
        groups.entry(m.datetime.date()).or_default().push(m);
    }
    let mut out: Vec<Meeting> = Vec::new();
    for (_, group) in groups {
        if group.len() == 1 {
            out.extend(group);
            continue;
        }
        let mut sized: Vec<(u64, Meeting)> = group
            .into_iter()
            .map(|m| {
                let size = client.content_length(&m.logs_url).unwrap_or(0);
                (size, m)
            })
            .collect();
        sized.sort_by_key(|(s, _)| std::cmp::Reverse(*s));
        let winner = sized.remove(0).1;
        let dropped: Vec<Meeting> = sized.into_iter().map(|(_, m)| m).collect();
        on_warning(&winner, &dropped);
        out.push(winner);
    }
    out.sort_by_key(|a| a.datetime);
    out
}

/// Rewrite the host portion of a meetbot artefact URL so
/// callers get a stable `meetbot.fedoraproject.org`-style
/// link regardless of whether the raw API returns
/// `meetbot-raw.fedoraproject.org`. In tests this routes to
/// the mock server as well.
fn rewrite_artefact_url(url: &str, base: &str) -> String {
    // Find the first single '/' after the scheme and splice
    // the path onto the chosen base. Leaves anything we don't
    // recognize untouched.
    let rest = match url.strip_prefix("https://") {
        Some(r) => r,
        None => match url.strip_prefix("http://") {
            Some(r) => r,
            None => return url.to_string(),
        },
    };
    let Some(slash) = rest.find('/') else {
        return url.to_string();
    };
    format!("{}{}", base, &rest[slash..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attendees_come_from_the_people_present_block() {
        let minutes = "Meeting summary\n---------------\n* stuff\n\n\
People Present (lines said)\n---------------------------\n\
* @siosm:fedora.im (8)\n* @meetbot:fedora.im (3)\n* @nirik:matrix.scrye.com (1)\n\n\
Generated by MeetBot\n";
        let got = parse_attendees(minutes);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            Attendee {
                id: "@siosm:fedora.im".to_string(),
                lines: 8
            }
        );
        assert_eq!(got[2].id, "@nirik:matrix.scrye.com");
        assert!(parse_attendees("no such block").is_empty());
    }

    #[test]
    fn txt_url_and_fedora_localpart() {
        assert_eq!(
            txt_url("https://m/x/fesco.2026-04-07-17.00.html"),
            "https://m/x/fesco.2026-04-07-17.00.txt"
        );
        assert_eq!(txt_url("https://m/x/f.log.html"), "https://m/x/f.log.txt");
        assert_eq!(fedora_localpart("@salimma:fedora.im"), Some("salimma"));
        assert_eq!(fedora_localpart("@nirik:matrix.scrye.com"), None);
        assert_eq!(fedora_localpart("salimma"), None);
    }
    use serde_json::json;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn rewrite_artefact_url_strips_host() {
        let out = rewrite_artefact_url(
            "https://meetbot-raw.fedoraproject.org/meeting/2026-01-01/foo.html",
            "https://example.org",
        );
        assert_eq!(out, "https://example.org/meeting/2026-01-01/foo.html",);
    }

    #[test]
    fn rewrite_artefact_url_handles_non_url() {
        // Not a URL — left untouched.
        assert_eq!(rewrite_artefact_url("not-a-url", "https://x"), "not-a-url");
    }

    /// Bring up a wiremock server synchronously so the rest of
    /// the test body can call the blocking `search` directly.
    fn start_mock() -> (tokio::runtime::Runtime, MockServer) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        (runtime, server)
    }

    #[test]
    fn search_returns_parsed_meetings() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/fragedpt/"))
                .and(query_param("rqstdata", "srchmeet"))
                .and(query_param("srchtext", "centos-hyperscale-sig"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {
                        "channel": "meeting_matrix_fedoraproject-org",
                        "datetime": "2026-04-22T15:08:00",
                        "topic": "centos-hyperscale-sig",
                        "url": {
                            "logs": "https://meetbot-raw.fedoraproject.org/meeting_matrix_fedoraproject-org/2026-04-22/centos-hyperscale-sig.2026-04-22-15.08.log.html",
                            "summary": "https://meetbot-raw.fedoraproject.org/meeting_matrix_fedoraproject-org/2026-04-22/centos-hyperscale-sig.2026-04-22-15.08.html"
                        }
                    },
                    {
                        "channel": "meeting_matrix_fedoraproject-org",
                        "datetime": "2023-12-20T16:00:00",
                        "topic": "centos-hyperscale-sig",
                        "url": {
                            "logs": "https://meetbot-raw.fedoraproject.org/meeting_matrix_fedoraproject-org/2023-12-20/centos-hyperscale-sig.2023-12-20-16.00.log.html",
                            "summary": "https://meetbot-raw.fedoraproject.org/meeting_matrix_fedoraproject-org/2023-12-20/centos-hyperscale-sig.2023-12-20-16.00.html"
                        }
                    }
                ])))
                .mount(&server)
                .await;
        });
        let meetings = Meetbot::with_base_url(&server.uri())
            .search("centos-hyperscale-sig")
            .expect("search");
        assert_eq!(meetings.len(), 2);
        assert_eq!(meetings[0].datetime.to_string(), "2023-12-20 16:00:00");
        assert_eq!(meetings[1].datetime.to_string(), "2026-04-22 15:08:00");
        assert!(meetings[0].summary_url.starts_with(&server.uri()));
    }

    #[test]
    fn search_empty_result() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/fragedpt/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .mount(&server)
                .await;
        });
        let meetings = Meetbot::with_base_url(&server.uri())
            .search("nothing")
            .expect("search");
        assert!(meetings.is_empty());
    }

    #[test]
    fn search_surfaces_http_error() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/fragedpt/"))
                .respond_with(ResponseTemplate::new(500))
                .mount(&server)
                .await;
        });
        let err = Meetbot::with_base_url(&server.uri())
            .search("x")
            .unwrap_err();
        assert!(err.to_string().contains("meetbot GET"));
    }

    #[test]
    fn content_length_from_head() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("HEAD"))
                .and(path("/logs/foo.html"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "4242"))
                .mount(&server)
                .await;
        });
        let client = Meetbot::with_base_url(&server.uri());
        let url = format!("{}/logs/foo.html", server.uri());
        assert_eq!(client.content_length(&url).unwrap(), 4242);
    }

    #[test]
    fn content_length_missing_header_is_error() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("HEAD"))
                .and(path("/logs/bar.html"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&server)
                .await;
        });
        let client = Meetbot::with_base_url(&server.uri());
        let url = format!("{}/logs/bar.html", server.uri());
        let err = client.content_length(&url).unwrap_err();
        assert!(err.to_string().contains("Content-Length"));
    }

    fn meeting(ts: &str, channel: &str, logs: &str) -> Meeting {
        Meeting {
            channel: channel.into(),
            datetime: chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S").unwrap(),
            topic: "t".into(),
            summary_url: "https://s/".into(),
            logs_url: logs.into(),
        }
    }

    #[test]
    fn dedup_keeps_longest_log_on_same_date() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("HEAD"))
                .and(path("/a"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "100"))
                .mount(&server)
                .await;
            Mock::given(method("HEAD"))
                .and(path("/b"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "500"))
                .mount(&server)
                .await;
            Mock::given(method("HEAD"))
                .and(path("/c"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "300"))
                .mount(&server)
                .await;
        });
        let client = Meetbot::with_base_url(&server.uri());
        let uri = server.uri();
        let meetings = vec![
            meeting("2026-02-11T16:01:00", "main", &format!("{uri}/a")),
            meeting("2026-02-11T16:05:00", "main", &format!("{uri}/b")),
            meeting("2026-02-11T16:02:00", "main", &format!("{uri}/c")),
        ];
        let mut warnings = 0;
        let out = dedup_by_longest_log(&client, meetings, |w, d| {
            warnings += 1;
            assert_eq!(w.logs_url, format!("{uri}/b"));
            assert_eq!(d.len(), 2);
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].logs_url, format!("{uri}/b"));
        assert_eq!(warnings, 1);
    }

    #[test]
    fn dedup_collapses_across_channels_on_same_date() {
        // Two rooms, same date: still collapsed — the SIG only
        // ever runs one meeting per day, so the shorter one is
        // assumed to be a mis-started fragment.
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("HEAD"))
                .and(path("/a"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "900"))
                .mount(&server)
                .await;
            Mock::given(method("HEAD"))
                .and(path("/b"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-length", "200"))
                .mount(&server)
                .await;
        });
        let client = Meetbot::with_base_url(&server.uri());
        let uri = server.uri();
        let meetings = vec![
            meeting("2026-02-11T16:05:00", "main", &format!("{uri}/a")),
            meeting("2026-02-11T16:08:00", "other", &format!("{uri}/b")),
        ];
        let mut warnings = 0;
        let out = dedup_by_longest_log(&client, meetings, |_, _| warnings += 1);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].channel, "main");
        assert_eq!(warnings, 1);
    }

    #[test]
    fn search_rejects_unparseable_datetime() {
        let (runtime, server) = start_mock();
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path("/fragedpt/"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                    {
                        "channel": "c",
                        "datetime": "not a timestamp",
                        "topic": "t",
                        "url": {
                            "logs": "https://meetbot-raw.fedoraproject.org/l",
                            "summary": "https://meetbot-raw.fedoraproject.org/s"
                        }
                    }
                ])))
                .mount(&server)
                .await;
        });
        let meetings = Meetbot::with_base_url(&server.uri())
            .search("x")
            .expect("search");
        assert!(meetings.is_empty(), "malformed entry should be dropped");
    }
}
