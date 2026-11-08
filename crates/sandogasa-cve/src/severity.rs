// SPDX-License-Identifier: Apache-2.0 OR MIT

//! How severe a CVE is, according to every source that says.
//!
//! No one source is enough. Red Hat's rating is the one an RHEL issue
//! is weighed by, but Red Hat may not have assessed a CVE yet, which is
//! often exactly when a CentOS Proposed Update is filed. NVD's answer
//! carries the CNA's score, and other parties', even before NVD has
//! analysed a CVE, but a CNA may give none. GitHub's advisory database
//! has a severity for what it has reviewed. So [`ratings`] asks them in
//! that order and keeps every answer: the first is the one to use, and
//! the rest show whether the sources agree.

use crate::cache::NvdCache;
use crate::sources::Sources;

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
    /// Who says so: `Red Hat`, `NVD`, a GHSA id.
    pub source: String,
    /// What they say, in their own words: `Important (CVSS 3: 8.1)`,
    /// `CVSS 3.1 8.1 high (nvd@nist.gov)`.
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

/// Every rating the sources give `cve_id`, in the order to trust them:
/// Red Hat, NVD, GitHub. Empty when none has rated it.
pub async fn ratings(
    cve_id: &str,
    nvd: &mut NvdCache<sandogasa_nvd::models::CveResponse>,
    sources: &Sources<'_>,
) -> Vec<Rating> {
    let mut out = Vec::new();
    if let Some(r) = sources
        .redhat_cve(cve_id)
        .await
        .as_deref()
        .and_then(redhat_rating)
    {
        out.push(r);
    }
    if let Some(r) = nvd.get(cve_id, |r| r).await.and_then(nvd_rating) {
        out.push(r);
    }
    if let Some(r) = sources
        .ghsa_json(cve_id)
        .await
        .as_deref()
        .and_then(ghsa_rating)
    {
        out.push(r);
    }
    out
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
}
