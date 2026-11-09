// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether an update is public yet, for the SIG's site (#80).
//!
//! An embargo ends with public disclosure, so that is what is asked,
//! from sources anyone can read, rather than whether a Jira issue can be
//! read anonymously, which says nothing either way: Red Hat Product
//! Security's CVE trackers stay restricted after the CVE is public
//! (RHEL-83109 answers 404), and an embargoed issue is unreadable to us
//! logged in or not. A CVE is public when Red Hat's security data has it
//! with a `public_date` that has passed, or, when Red Hat has no record,
//! NVD has published one. A source that cannot be reached is not an
//! answer: the caller leaves the update out of that run.

use chrono::{DateTime, Utc};

/// What one source says of a CVE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    /// Disclosed.
    Public,
    /// Known, but with a disclosure date still to come.
    Pending,
    /// No record at all.
    Missing,
}

/// Red Hat's security data: `CPU_SIG_TRACKER_REDHAT_BASE`, for tests,
/// else access.redhat.com.
pub fn redhat_base() -> String {
    std::env::var("CPU_SIG_TRACKER_REDHAT_BASE")
        .unwrap_or_else(|_| "https://access.redhat.com".to_string())
}

/// NVD's API: `CPU_SIG_TRACKER_NVD_BASE`, for tests, else NVD's.
pub fn nvd_base() -> String {
    std::env::var("CPU_SIG_TRACKER_NVD_BASE")
        .unwrap_or_else(|_| "https://services.nvd.nist.gov".to_string())
}

/// What Red Hat's record for a CVE says, at `now`: public once its
/// `public_date` has passed. A record without one is taken as pending,
/// rather than guessed public.
pub fn redhat_record(json: &str, now: DateTime<Utc>) -> Record {
    let date = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("public_date")?.as_str().map(str::to_string))
        .and_then(|d| {
            DateTime::parse_from_rfc3339(&d)
                .ok()
                .map(|d| d.with_timezone(&Utc))
                .or_else(|| {
                    // `2026-10-28T00:00:00` without an offset is UTC.
                    chrono::NaiveDateTime::parse_from_str(&d, "%Y-%m-%dT%H:%M:%S")
                        .ok()
                        .map(|n| n.and_utc())
                })
        });
    match date {
        Some(d) if d <= now => Record::Public,
        _ => Record::Pending,
    }
}

/// What an NVD `cves/2.0?cveId=` answer says: published when it has a
/// record.
pub fn nvd_record(json: &str) -> Record {
    let total = serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("totalResults")?.as_u64())
        .unwrap_or(0);
    if total > 0 {
        Record::Public
    } else {
        Record::Missing
    }
}

/// Whether `cve` is public: Red Hat's record decides when there is one,
/// NVD's otherwise. An error when a source cannot be reached.
pub async fn is_public(http: &reqwest::Client, cve: &str) -> Result<bool, String> {
    let redhat = format!(
        "{}/hydra/rest/securitydata/cve/{cve}.json",
        redhat_base().trim_end_matches('/')
    );
    let resp = http
        .get(&redhat)
        .send()
        .await
        .map_err(|e| format!("cannot reach Red Hat's security data for {cve}: {e}"))?;
    match resp.status().as_u16() {
        200 => {
            let text = resp
                .text()
                .await
                .map_err(|e| format!("cannot read Red Hat's record of {cve}: {e}"))?;
            return Ok(redhat_record(&text, Utc::now()) == Record::Public);
        }
        404 => {}
        other => {
            return Err(format!(
                "Red Hat's security data answered HTTP {other} for {cve}"
            ));
        }
    }
    let nvd = format!(
        "{}/rest/json/cves/2.0?cveId={cve}",
        nvd_base().trim_end_matches('/')
    );
    let resp = http
        .get(&nvd)
        .send()
        .await
        .map_err(|e| format!("cannot reach NVD for {cve}: {e}"))?;
    match resp.status().as_u16() {
        200 => {
            let text = resp
                .text()
                .await
                .map_err(|e| format!("cannot read NVD's answer for {cve}: {e}"))?;
            Ok(nvd_record(&text) == Record::Public)
        }
        // NVD answers 404 for an id it has never heard of.
        404 => Ok(false),
        other => Err(format!("NVD answered HTTP {other} for {cve}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn red_hat_decides_by_the_disclosure_date() {
        let now = at("2026-11-09T12:00:00Z");
        assert_eq!(
            redhat_record(r#"{"public_date": "2026-10-28T00:00:00Z"}"#, now),
            Record::Public
        );
        assert_eq!(
            redhat_record(r#"{"public_date": "2026-10-28T00:00:00"}"#, now),
            Record::Public
        );
        assert_eq!(
            redhat_record(r#"{"public_date": "2026-12-01T00:00:00Z"}"#, now),
            Record::Pending
        );
        assert_eq!(redhat_record(r#"{"name": "CVE-1"}"#, now), Record::Pending);
    }

    #[test]
    fn nvd_decides_by_whether_it_has_a_record() {
        assert_eq!(
            nvd_record(r#"{"totalResults": 1, "vulnerabilities": []}"#),
            Record::Public
        );
        assert_eq!(nvd_record(r#"{"totalResults": 0}"#), Record::Missing);
    }

    /// Red Hat first; NVD only for a CVE Red Hat has no record of; and a
    /// source that cannot answer is an error, not a "no".
    #[test]
    #[serial_test::serial]
    fn a_cve_is_public_by_red_hat_then_nvd() {
        use wiremock::matchers::{method, path, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        let rh = |cve: &str| format!("/hydra/rest/securitydata/cve/{cve}.json");
        rt.block_on(async {
            for (cve, status, body) in [
                (
                    "CVE-2026-1",
                    200,
                    r#"{"public_date": "2026-01-01T00:00:00Z"}"#,
                ),
                (
                    "CVE-2026-2",
                    200,
                    r#"{"public_date": "2999-01-01T00:00:00Z"}"#,
                ),
                ("CVE-2026-3", 404, ""),
                ("CVE-2026-4", 404, ""),
                ("CVE-2026-5", 503, ""),
            ] {
                Mock::given(method("GET"))
                    .and(path(rh(cve)))
                    .respond_with(ResponseTemplate::new(status).set_body_string(body))
                    .mount(&server)
                    .await;
            }
            for (cve, total) in [("CVE-2026-3", 1), ("CVE-2026-4", 0)] {
                Mock::given(method("GET"))
                    .and(path("/rest/json/cves/2.0"))
                    .and(query_param("cveId", cve))
                    .respond_with(
                        ResponseTemplate::new(200)
                            .set_body_string(format!(r#"{{"totalResults": {total}}}"#)),
                    )
                    .mount(&server)
                    .await;
            }
        });
        let _env = crate::test_support::EnvGuard::new(&[
            ("CPU_SIG_TRACKER_REDHAT_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_NVD_BASE", &server.uri()),
        ]);
        sandogasa_cli::init();
        let http = reqwest::Client::new();
        let public = |cve: &str| rt.block_on(is_public(&http, cve));
        assert_eq!(public("CVE-2026-1"), Ok(true));
        assert_eq!(public("CVE-2026-2"), Ok(false), "disclosure still to come");
        assert_eq!(public("CVE-2026-3"), Ok(true), "Red Hat has none, NVD does");
        assert_eq!(public("CVE-2026-4"), Ok(false), "reserved, undisclosed");
        assert!(
            public("CVE-2026-5").is_err(),
            "unreachable is not an answer"
        );
    }
}
