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
//! cve.org has published one. cve.org is where a CVE is published, and
//! NVD copies its records from there, so NVD is asked only when cve.org
//! does not answer. A source that cannot be reached is not an answer:
//! the caller leaves the update out of that run.

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

/// cve.org's API: `CPU_SIG_TRACKER_CVEORG_BASE`, for tests, else
/// cve.org's.
pub fn cveorg_base() -> String {
    std::env::var("CPU_SIG_TRACKER_CVEORG_BASE")
        .unwrap_or_else(|_| "https://cveawg.mitre.org".to_string())
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

/// What cve.org's answer for a CVE says: published when it is a record,
/// whatever its state. A record its CNA has since rejected was
/// published all the same, and NVD's count includes it too.
pub fn cveorg_record(json: &str) -> Record {
    match sandogasa_cve::cveorg::parse(json) {
        Some(_) => Record::Public,
        None => Record::Missing,
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
/// cve.org's otherwise, and NVD's when cve.org does not answer. An error
/// when no source can be reached, after the workspace's usual retries:
/// NVD in particular answers 503 under load, and an error leaves the
/// update out of the run.
pub async fn is_public(http: &reqwest::Client, cve: &str) -> Result<bool, String> {
    is_public_with(http, cve, &sandogasa_cli::retry::Policy::DEFAULT).await
}

/// [`is_public`] under `policy`.
pub async fn is_public_with(
    http: &reqwest::Client,
    cve: &str,
    policy: &sandogasa_cli::retry::Policy,
) -> Result<bool, String> {
    let redhat = format!(
        "{}/hydra/rest/securitydata/cve/{cve}.json",
        redhat_base().trim_end_matches('/')
    );
    let resp = sandogasa_cli::retry::send("Red Hat security data", policy, || http.get(&redhat))
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
    // cve.org next: a CVE is published there before NVD copies it, and
    // an id it has published nothing under is one NVD cannot have.
    let cveorg_failed = match cveorg_answer(http, cve, policy).await {
        Ok(public) => return Ok(public),
        Err(why) => why,
    };
    let nvd = format!(
        "{}/rest/json/cves/2.0?cveId={cve}",
        nvd_base().trim_end_matches('/')
    );
    let resp = sandogasa_cli::retry::send("NVD", policy, || http.get(&nvd))
        .await
        .map_err(|e| format!("{cveorg_failed}, and cannot reach NVD for {cve}: {e}"))?;
    match resp.status().as_u16() {
        200 => {
            let text = resp.text().await.map_err(|e| {
                format!("{cveorg_failed}, and cannot read NVD's answer for {cve}: {e}")
            })?;
            Ok(nvd_record(&text) == Record::Public)
        }
        // NVD answers 404 for an id it has never heard of.
        404 => Ok(false),
        other => Err(format!(
            "{cveorg_failed}, and NVD answered HTTP {other} for {cve}"
        )),
    }
}

/// Whether cve.org has published `cve`: a record says yes, its 404 says
/// no. An error, saying why, when it gave neither answer, which leaves
/// the question to NVD.
async fn cveorg_answer(
    http: &reqwest::Client,
    cve: &str,
    policy: &sandogasa_cli::retry::Policy,
) -> Result<bool, String> {
    let url = format!("{}/api/cve/{cve}", cveorg_base().trim_end_matches('/'));
    let resp = sandogasa_cli::retry::send("cve.org", policy, || http.get(&url))
        .await
        .map_err(|e| format!("cannot reach cve.org for {cve}: {e}"))?;
    match resp.status().as_u16() {
        200 => {
            let text = resp
                .text()
                .await
                .map_err(|e| format!("cannot read cve.org's record of {cve}: {e}"))?;
            match cveorg_record(&text) {
                Record::Missing => Err(format!("cve.org's answer for {cve} is not a record")),
                _ => Ok(true),
            }
        }
        404 => Ok(false),
        other => Err(format!("cve.org answered HTTP {other} for {cve}")),
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
    fn cve_org_decides_by_whether_it_has_a_record() {
        let record = |state: &str| {
            format!(r#"{{"cveMetadata": {{"cveId": "CVE-2026-1", "state": "{state}"}}}}"#)
        };
        assert_eq!(cveorg_record(&record("PUBLISHED")), Record::Public);
        assert_eq!(cveorg_record(&record("REJECTED")), Record::Public);
        assert_eq!(
            cveorg_record(r#"{"error": "CVE_RECORD_DNE"}"#),
            Record::Missing
        );
    }

    #[test]
    fn nvd_decides_by_whether_it_has_a_record() {
        assert_eq!(
            nvd_record(r#"{"totalResults": 1, "vulnerabilities": []}"#),
            Record::Public
        );
        assert_eq!(nvd_record(r#"{"totalResults": 0}"#), Record::Missing);
    }

    /// Red Hat first; cve.org for a CVE Red Hat has no record of; NVD
    /// only when cve.org does not answer; and a source that cannot
    /// answer is an error, not a "no".
    #[test]
    #[serial_test::serial]
    fn a_cve_is_public_by_red_hat_then_cve_org_then_nvd() {
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
                ("CVE-2026-6", 404, ""),
                ("CVE-2026-7", 404, ""),
                ("CVE-2026-8", 404, ""),
                ("CVE-2026-9", 404, ""),
            ] {
                Mock::given(method("GET"))
                    .and(path(rh(cve)))
                    .respond_with(ResponseTemplate::new(status).set_body_string(body))
                    .mount(&server)
                    .await;
            }
            let record = |cve: &str| {
                format!(r#"{{"cveMetadata": {{"cveId": "{cve}", "state": "PUBLISHED"}}}}"#)
            };
            for (cve, status, body) in [
                ("CVE-2026-3", 200, record("CVE-2026-3")),
                (
                    "CVE-2026-4",
                    404,
                    r#"{"error": "CVE_RECORD_DNE"}"#.to_string(),
                ),
                ("CVE-2026-6", 503, String::new()),
                ("CVE-2026-7", 503, String::new()),
                ("CVE-2026-8", 200, "<html>busy</html>".to_string()),
                ("CVE-2026-9", 503, String::new()),
            ] {
                Mock::given(method("GET"))
                    .and(path(format!("/api/cve/{cve}")))
                    .respond_with(ResponseTemplate::new(status).set_body_string(body))
                    .mount(&server)
                    .await;
            }
            // NVD is asked only for the CVEs cve.org gave no answer on:
            // nothing is mounted for the others, and an unmounted path
            // answers 404, which would read as "not public".
            for (cve, status, total) in [
                ("CVE-2026-6", 200, 1),
                ("CVE-2026-7", 200, 0),
                ("CVE-2026-8", 200, 1),
                ("CVE-2026-9", 503, 0),
            ] {
                Mock::given(method("GET"))
                    .and(path("/rest/json/cves/2.0"))
                    .and(query_param("cveId", cve))
                    .respond_with(
                        ResponseTemplate::new(status)
                            .set_body_string(format!(r#"{{"totalResults": {total}}}"#)),
                    )
                    .mount(&server)
                    .await;
            }
        });
        let _env = crate::test_support::EnvGuard::new(&[
            ("CPU_SIG_TRACKER_REDHAT_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_CVEORG_BASE", &server.uri()),
            ("CPU_SIG_TRACKER_NVD_BASE", &server.uri()),
        ]);
        sandogasa_cli::init();
        let http = reqwest::Client::new();
        let policy = sandogasa_cli::retry::Policy::immediate(2);
        let public = |cve: &str| rt.block_on(is_public_with(&http, cve, &policy));
        assert_eq!(public("CVE-2026-1"), Ok(true));
        assert_eq!(public("CVE-2026-2"), Ok(false), "disclosure still to come");
        assert_eq!(
            public("CVE-2026-3"),
            Ok(true),
            "Red Hat has none, cve.org does"
        );
        assert_eq!(public("CVE-2026-4"), Ok(false), "reserved, undisclosed");
        assert!(
            public("CVE-2026-5").is_err(),
            "unreachable is not an answer"
        );
        assert_eq!(
            public("CVE-2026-6"),
            Ok(true),
            "cve.org is down, NVD has it"
        );
        assert_eq!(
            public("CVE-2026-7"),
            Ok(false),
            "cve.org is down, NVD has none"
        );
        assert_eq!(
            public("CVE-2026-8"),
            Ok(true),
            "cve.org's answer is not a record, NVD has one"
        );
        let neither = public("CVE-2026-9").unwrap_err();
        assert!(
            neither.contains("cve.org answered HTTP 503")
                && neither.contains("NVD answered HTTP 503"),
            "{neither}"
        );
        // cve.org's own word was enough for these two: NVD was not asked.
        let asked_nvd: Vec<String> = rt
            .block_on(server.received_requests())
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == "/rest/json/cves/2.0")
            .filter_map(|r| r.url.query().map(str::to_string))
            .collect();
        assert!(
            !asked_nvd
                .iter()
                .any(|q| q.ends_with("CVE-2026-3") || q.ends_with("CVE-2026-4")),
            "{asked_nvd:?}"
        );
    }
}
