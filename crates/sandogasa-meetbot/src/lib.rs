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

/// Default base URL used for public meeting artefacts (log /
/// summary HTML). Distinct from the web frontend: the JSON
/// endpoint returns URLs on `meetbot-raw.fedoraproject.org`,
/// but users typically want to link to the public-facing
/// `meetbot.fedoraproject.org` host instead. `Meetbot::search`
/// applies this rewrite.
pub const PUBLIC_ARTEFACT_BASE: &str = "https://meetbot.fedoraproject.org";

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

/// Blocking meetbot client.
pub struct Meetbot {
    http: reqwest::blocking::Client,
    base_url: String,
    artefact_base: String,
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
        let http = reqwest::blocking::Client::builder()
            .user_agent(concat!("sandogasa-meetbot/", env!("CARGO_PKG_VERSION"),))
            .build()
            .expect("build reqwest client");
        Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            artefact_base: base_url.trim_end_matches('/').to_string(),
        }
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
            .filter_map(|r| r.into_meeting(&self.artefact_base))
            .collect();
        meetings.sort_by(|a, b| a.datetime.cmp(&b.datetime));
        Ok(meetings)
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
