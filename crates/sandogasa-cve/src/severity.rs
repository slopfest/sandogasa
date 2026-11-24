// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How severe a CVE is, according to every source that says.
//!
//! No one source is enough. Red Hat's rating is the one an RHEL issue
//! is weighed by, but Red Hat may not have assessed a CVE yet, which is
//! often exactly when a CentOS Proposed Update is filed. NVD's answer
//! carries the CNA's score, and other parties', even before NVD has
//! analyzed a CVE, but a CNA may give none, and NVD may have no record
//! yet or not answer. cve.org's record is there first, with the CNA's
//! score and those other parties added to it. GitHub's advisory
//! database has a severity for what it has reviewed. So [`ratings`]
//! lists them in that order and keeps every answer: the first is the
//! one to use, and the rest show whether the sources agree.

use crate::cache::NvdCache;
use crate::cveorg;
use crate::sources::Sources;

/// Who NVD names as the source of a score of its own, as opposed to
/// one it carries for a CNA or another party.
const NVD_ITSELF: &str = "nvd@nist.gov";

/// A severity, in the vocabulary CVSS and GitHub use; Red Hat's
/// `Important` and `Moderate` read as `High` and `Medium`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Low,
    Medium,
    High,
    Critical,
}

impl Level {
    /// The level a source's word names, case-insensitively: `critical`,
    /// `high` or `important`, `medium` or `moderate`, `low`. `None` for
    /// `none`, `unknown` and anything else.
    pub fn parse(word: &str) -> Option<Level> {
        match word.trim().to_ascii_lowercase().as_str() {
            "critical" => Some(Level::Critical),
            "high" | "important" => Some(Level::High),
            "medium" | "moderate" => Some(Level::Medium),
            "low" => Some(Level::Low),
            _ => None,
        }
    }

    /// `critical`, `high`, `medium` or `low`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Critical => "critical",
            Level::High => "high",
            Level::Medium => "medium",
            Level::Low => "low",
        }
    }
}

/// One source's rating of a CVE.
#[derive(Debug, Clone, PartialEq)]
pub struct Rating {
    pub level: Level,
    /// Who says so: `Red Hat`, `NVD`, `cve.org`, a GHSA id.
    pub source: String,
    /// What they say, in their own words: `Important (CVSS 3: 8.1)`,
    /// `CVSS 3.1 8.1 high (nvd@nist.gov)`,
    /// `CVSS 3.1 7.8 high (CISA-ADP)`.
    pub detail: String,
}

impl std::fmt::Display for Rating {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.source, self.detail)
    }
}

/// Where Red Hat publishes its assessment of a CVE.
pub fn redhat_url(cve_id: &str) -> String {
    format!("https://access.redhat.com/hydra/rest/securitydata/cve/{cve_id}.json")
}

/// Red Hat's rating in its security data for a CVE: its
/// `threat_severity` (Critical, Important, Moderate, Low), with its CVSS
/// 3 score when it gives one.
pub fn redhat_rating(json: &str) -> Option<Rating> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let word = v["threat_severity"].as_str()?;
    let level = Level::parse(word)?;
    let score = v["cvss3"]["cvss3_base_score"]
        .as_str()
        .map(str::to_string)
        .or_else(|| {
            v["cvss3"]["cvss3_base_score"]
                .as_f64()
                .map(|s| s.to_string())
        });
    let detail = match score {
        Some(s) => format!("{word} (CVSS 3: {s})"),
        None => word.to_string(),
    };
    Some(Rating {
        level,
        source: "Red Hat".to_string(),
        detail,
    })
}

/// The rating NVD's answer gives a CVE: the score [`CveResponse::cvss`]
/// picks, NVD's own or another's.
///
/// [`CveResponse::cvss`]: sandogasa_nvd::models::CveResponse::cvss
pub fn nvd_rating(resp: &sandogasa_nvd::models::CveResponse) -> Option<Rating> {
    let cvss = resp.cvss()?;
    Some(Rating {
        level: Level::parse(&cvss.severity)?,
        source: "NVD".to_string(),
        detail: format!(
            "CVSS {} {} {} ({})",
            cvss.version, cvss.score, cvss.severity, cvss.source
        ),
    })
}

/// The rating of the first GitHub advisory for a CVE with a severity,
/// from GitHub's answer to a search by CVE (a list of advisories).
pub fn ghsa_rating(json: &str) -> Option<Rating> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    v.as_array()?.iter().find_map(|a| {
        let word = a["severity"].as_str()?;
        let level = Level::parse(word)?;
        let score = a["cvss_severities"]["cvss_v3"]["score"]
            .as_f64()
            .or_else(|| a["cvss"]["score"].as_f64())
            .filter(|s| *s > 0.0);
        let detail = match score {
            Some(s) => format!("{word} (CVSS 3: {s})"),
            None => word.to_string(),
        };
        Some(Rating {
            level,
            source: a["ghsa_id"].as_str().unwrap_or("GitHub").to_string(),
            detail,
        })
    })
}

/// Which of `scores` NVD passes on as `cvss`, when it passes one on:
/// of those with its CVSS version and score, the one by the party NVD
/// names as the source. NVD names an ADP by its cve.org organization id
/// (CISA's), and a CNA by an address, whose domain carries the CNA's
/// short name (`secalert@redhat.com` for `redhat`). Failing both, the
/// first, which is the CNA's, as the record lists it first.
fn passed_on(scores: &[cveorg::Score], cvss: &sandogasa_nvd::Cvss) -> Option<usize> {
    let same: Vec<usize> = scores
        .iter()
        .enumerate()
        .filter(|(_, s)| s.version == cvss.version && s.score == cvss.score)
        .map(|(i, _)| i)
        .collect();
    let source = cvss.source.to_ascii_lowercase();
    let domain = source.split_once('@').map(|(_, d)| d);
    let named = |s: &cveorg::Score| {
        s.org_id
            .as_deref()
            .is_some_and(|id| id.eq_ignore_ascii_case(&source))
            || domain.is_some_and(|d| {
                let by = s.by.to_ascii_lowercase();
                !by.is_empty() && d.split('.').any(|label| label == by)
            })
    };
    same.iter()
        .copied()
        .find(|&i| named(&scores[i]))
        .or_else(|| same.first().copied())
}

/// The ratings cve.org's record gives a CVE: its CNA's score, then each
/// other party's (CISA's, for many CVEs their CNA did not score).
///
/// `nvd` is the score NVD's answer was rated by, when it was. NVD
/// carries the CNA's and other parties' scores until it has one of its
/// own, so the score here NVD only passes on is the same assessment and
/// is not listed a second time. Only that one is left out: another
/// party's equal score is an assessment of its own.
pub fn cveorg_ratings(record: &cveorg::Record, nvd: Option<&sandogasa_nvd::Cvss>) -> Vec<Rating> {
    let dropped = nvd
        .filter(|c| !c.source.eq_ignore_ascii_case(NVD_ITSELF))
        .and_then(|c| passed_on(&record.scores, c));
    record
        .scores
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != dropped)
        .map(|(_, s)| s)
        .filter_map(|s| {
            Some(Rating {
                level: Level::parse(&s.severity)?,
                source: "cve.org".to_string(),
                detail: format!("CVSS {} {} {} ({})", s.version, s.score, s.severity, s.by),
            })
        })
        .collect()
}

/// The ratings in the sources' answers, in the order to trust them:
/// Red Hat's security data, NVD's answer, cve.org's record, GitHub's
/// advisories. For a caller that has asked the sources already and
/// reads more than the ratings from their answers, so it asks each
/// once. A cve.org score NVD only passes on is not listed twice.
pub fn ratings_from(
    redhat: Option<&str>,
    nvd: Option<&sandogasa_nvd::models::CveResponse>,
    cveorg: Option<&cveorg::Record>,
    ghsa: Option<&str>,
) -> Vec<Rating> {
    let mut out = Vec::new();
    out.extend(redhat.and_then(redhat_rating));
    out.extend(nvd.and_then(nvd_rating));
    if let Some(record) = cveorg {
        let nvd_score = nvd.and_then(|r| r.cvss());
        out.extend(cveorg_ratings(record, nvd_score.as_ref()));
    }
    out.extend(ghsa.and_then(ghsa_rating));
    out
}

/// Every rating the sources give `cve_id`, in the order to trust them:
/// Red Hat, NVD, cve.org, GitHub. Empty when none has rated it. The
/// sources are asked together, so none waits for another's answer.
pub async fn ratings(
    cve_id: &str,
    nvd: &mut NvdCache<sandogasa_nvd::models::CveResponse>,
    sources: &Sources<'_>,
) -> Vec<Rating> {
    let (redhat, nvd, record, ghsa) = tokio::join!(
        sources.redhat_cve(cve_id),
        nvd.get(cve_id, |r| r),
        sources.cveorg(cve_id),
        sources.ghsa_json(cve_id),
    );
    ratings_from(redhat.as_deref(), nvd, record.as_ref(), ghsa.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_hat_s_words_read_as_the_cvss_levels() {
        assert_eq!(Level::parse("Important"), Some(Level::High));
        assert_eq!(Level::parse("Moderate"), Some(Level::Medium));
        assert_eq!(Level::parse("CRITICAL"), Some(Level::Critical));
        assert_eq!(Level::parse("low"), Some(Level::Low));
        assert_eq!(Level::parse("unknown"), None);
        assert!(Level::Critical > Level::High && Level::Medium > Level::Low);
    }

    #[test]
    fn each_source_s_answer_is_read() {
        // Recorded shapes: Red Hat's security data and GitHub's search
        // by CVE, for CVE-2025-27363 (freetype).
        let redhat = r#"{"threat_severity": "Important",
            "cvss3": {"cvss3_base_score": "8.1", "status": "verified"}}"#;
        let r = redhat_rating(redhat).expect("rated");
        assert_eq!(r.level, Level::High);
        assert_eq!(r.to_string(), "Red Hat: Important (CVSS 3: 8.1)");

        let ghsa = r#"[{"ghsa_id": "GHSA-g8qj-jv5h-78cp", "severity": "high",
            "cvss_severities": {"cvss_v3": {"score": 8.1}, "cvss_v4": {"score": 0.0}}}]"#;
        let g = ghsa_rating(ghsa).expect("rated");
        assert_eq!(g.level, Level::High);
        assert_eq!(g.to_string(), "GHSA-g8qj-jv5h-78cp: high (CVSS 3: 8.1)");

        // An advisory GitHub has not rated is passed over.
        assert!(ghsa_rating(r#"[{"ghsa_id": "GHSA-x", "severity": "unknown"}]"#).is_none());
        // Red Hat has not assessed it: no rating, not an error.
        assert!(redhat_rating(r#"{"message": "Not Found"}"#).is_none());
        assert!(redhat_rating("not json").is_none());
    }

    #[test]
    fn nvd_s_rating_names_the_score_and_who_gave_it() {
        let resp: sandogasa_nvd::models::CveResponse =
            serde_json::from_value(serde_json::json!({"vulnerabilities": [{"cve": {
                "id": "CVE-2026-2297",
                "metrics": {"cvssMetricV40": [{
                    "source": "cna@python.org", "type": "Secondary",
                    "cvssData": {"version": "4.0", "baseScore": 5.7, "baseSeverity": "MEDIUM"},
                }]},
            }}]}))
            .unwrap();
        let r = nvd_rating(&resp).expect("rated");
        assert_eq!(r.level, Level::Medium);
        assert_eq!(r.to_string(), "NVD: CVSS 4.0 5.7 medium (cna@python.org)");
    }

    /// A record whose CNA scored it 8.1 and CISA 7.8, with one more
    /// party whose word is not a level.
    fn scored_record() -> cveorg::Record {
        cveorg::parse(
            r#"{"cveMetadata": {"cveId": "CVE-2026-1", "state": "PUBLISHED",
                    "assignerShortName": "redhat"},
                "containers": {
                    "cna": {"metrics": [{"cvssV3_1": {"baseScore": 8.1, "baseSeverity": "HIGH"}}]},
                    "adp": [
                        {"metrics": [{"cvssV3_1": {"baseScore": 7.8, "baseSeverity": "HIGH"}}],
                         "providerMetadata": {"shortName": "CISA-ADP"}},
                        {"metrics": [{"cvssV3_1": {"baseScore": 0.0, "baseSeverity": "NONE"}}],
                         "providerMetadata": {"shortName": "other"}}]}}"#,
        )
        .expect("a record")
    }

    fn nvd_score(score: f64, source: &str) -> sandogasa_nvd::Cvss {
        sandogasa_nvd::Cvss {
            version: "3.1".to_string(),
            score,
            severity: "high".to_string(),
            source: source.to_string(),
        }
    }

    #[test]
    fn cve_org_s_ratings_are_the_cna_s_then_each_other_party_s() {
        let listed: Vec<String> = cveorg_ratings(&scored_record(), None)
            .iter()
            .map(Rating::to_string)
            .collect();
        assert_eq!(
            listed,
            [
                "cve.org: CVSS 3.1 8.1 high (redhat)",
                "cve.org: CVSS 3.1 7.8 high (CISA-ADP)",
            ]
        );
        assert!(
            cveorg_ratings(&scored_record(), None)
                .iter()
                .all(|r| r.level == Level::High)
        );
        assert!(cveorg_ratings(&cveorg::Record::default(), None).is_empty());
    }

    #[test]
    fn a_score_nvd_only_passes_on_is_not_listed_again() {
        let by = |nvd: &sandogasa_nvd::Cvss| -> Vec<String> {
            cveorg_ratings(&scored_record(), Some(nvd))
                .into_iter()
                .map(|r| r.detail)
                .collect()
        };
        // NVD is quoting the CNA: only CISA's is news.
        assert_eq!(
            by(&nvd_score(8.1, "secalert@redhat.com")),
            ["CVSS 3.1 7.8 high (CISA-ADP)"]
        );
        // NVD scored it itself, and happens to agree with the CNA: the
        // CNA's is a second opinion, and stays.
        assert_eq!(by(&nvd_score(8.1, "nvd@nist.gov")).len(), 2);
        // Another party's score that differs from both.
        assert_eq!(by(&nvd_score(9.8, "secalert@redhat.com")).len(), 2);
        // The same number in another CVSS version is another score.
        let mut v4 = nvd_score(8.1, "secalert@redhat.com");
        v4.version = "4.0".to_string();
        assert_eq!(by(&v4).len(), 2);
    }

    /// #243: the CNA and CISA give the same score, as they often do, and
    /// NVD passes on one of them. Only that one is left out; the other
    /// is an assessment of its own.
    #[test]
    fn an_equal_score_by_another_party_is_kept() {
        const CISA: &str = "134c704f-9b21-4f2e-91b3-4a467353bcc0";
        let record = |cna: &str| {
            cveorg::parse(&format!(
                r#"{{"cveMetadata": {{"cveId": "CVE-2026-2", "state": "PUBLISHED",
                        "assignerShortName": "{cna}"}},
                    "containers": {{
                        "cna": {{"metrics": [{{"cvssV3_1": {{"baseScore": 7.5, "baseSeverity": "HIGH"}}}}]}},
                        "adp": [{{"metrics": [{{"cvssV3_1": {{"baseScore": 7.5, "baseSeverity": "HIGH"}}}}],
                            "providerMetadata": {{"shortName": "CISA-ADP", "orgId": "{CISA}"}}}}]}}}}"#
            ))
            .expect("a record")
        };
        let by = |cna: &str, source: &str| -> Vec<String> {
            cveorg_ratings(&record(cna), Some(&nvd_score(7.5, source)))
                .into_iter()
                .map(|r| r.detail)
                .collect()
        };
        // NVD names the CNA by its address: CISA's stays.
        assert_eq!(
            by("redhat", "secalert@redhat.com"),
            ["CVSS 3.1 7.5 high (CISA-ADP)"]
        );
        // NVD names CISA by its organization id: the CNA's stays.
        assert_eq!(by("redhat", CISA), ["CVSS 3.1 7.5 high (redhat)"]);
        // An address that does not carry the CNA's short name is still
        // the CNA's, the one NVD passes on when it has one.
        assert_eq!(
            by("Linux", "416baaa9@kernel.org"),
            ["CVSS 3.1 7.5 high (CISA-ADP)"]
        );
    }

    /// Every source answering, from a primed cache and a local NVD.
    #[tokio::test]
    async fn ratings_list_red_hat_nvd_cve_org_then_github() {
        use sandogasa_cli::cache::{DiskCache, url_key};
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let cve = "CVE-2026-1";
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("cveId", cve))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "vulnerabilities": [{"cve": {"id": cve, "metrics": {"cvssMetricV31": [{
                    "source": "secalert@redhat.com", "type": "Secondary",
                    "cvssData": {"version": "3.1", "baseScore": 8.1, "baseSeverity": "HIGH"},
                }]}}}]
            })))
            .mount(&server)
            .await;
        let dir = tempfile::tempdir().unwrap();
        let cache = DiskCache::at(Some(dir.path().to_path_buf()), false, None);
        let record = r#"{"cveMetadata": {"cveId": "CVE-2026-1", "state": "PUBLISHED",
                "assignerShortName": "redhat"},
            "containers": {
                "cna": {"metrics": [{"cvssV3_1": {"baseScore": 8.1, "baseSeverity": "HIGH"}}]},
                "adp": [{"metrics": [{"cvssV3_1": {"baseScore": 7.8, "baseSeverity": "HIGH"}}],
                    "providerMetadata": {"shortName": "CISA-ADP"}}]}}"#;
        for (url, body) in [
            (redhat_url(cve), r#"{"threat_severity": "Important"}"#),
            (cveorg::record_url(cve), record),
            (
                crate::advisory::ghsa_search_url(cve),
                r#"[{"ghsa_id": "GHSA-aaaa-bbbb-cccc", "severity": "high"}]"#,
            ),
        ] {
            cache.store(&url_key(&url), body);
        }
        sandogasa_cli::install_crypto_provider();
        let http = reqwest::Client::new();
        let sources = Sources::new(&http, &cache, None);
        let mut nvd = NvdCache::with(
            sandogasa_nvd::NvdClient::with_base_url(&server.uri()),
            DiskCache::at(None, false, None),
            std::time::Duration::ZERO,
            false,
        );
        let listed: Vec<String> = ratings(cve, &mut nvd, &sources)
            .await
            .iter()
            .map(Rating::to_string)
            .collect();
        assert_eq!(
            listed,
            [
                "Red Hat: Important",
                "NVD: CVSS 3.1 8.1 high (secalert@redhat.com)",
                // The CNA's 8.1 is the one NVD quotes.
                "cve.org: CVSS 3.1 7.8 high (CISA-ADP)",
                "GHSA-aaaa-bbbb-cccc: high",
            ]
        );
    }
}
