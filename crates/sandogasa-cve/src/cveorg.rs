// SPDX-License-Identifier: Apache-2.0 OR MIT

//! cve.org (<https://www.cve.org>), the CVE Program's own record of a
//! CVE, as a source beside NVD.
//!
//! NVD copies its records from here, with a delay, and adds its own
//! analysis to them: the CPE product data and a score. So a record is
//! here first, and it is all there is while NVD has none or does not
//! answer. It carries what the CVE's CNA published (a title, a
//! description, references, often a score) and what other parties added
//! in containers of their own (ADPs: CISA scores many CVEs their CNA
//! did not).
//!
//! `affected`, the CNA's own account of which versions are vulnerable,
//! is not read: CNAs write it too many ways to judge a build by (see
//! DEVELOPMENT.md).

use serde_json::Value;

/// The status a record standing in for NVD's carries (see
/// [`Record::as_nvd`]), where NVD's own would say `Analyzed` or
/// `Awaiting Analysis`: it is shown to a person as NVD's status, so it
/// says NVD gave none.
pub const STAND_IN_STATUS: &str = "no record; read from cve.org";

/// The CVSS versions a container may score a CVE in, as cve.org's
/// records key them, in the order to quote them: 3.1 first, as Red
/// Hat's issues quote it and as `CveResponse::cvss` takes NVD's.
const CVSS_KEYS: [(&str, &str); 4] = [
    ("cvssV3_1", "3.1"),
    ("cvssV4_0", "4.0"),
    ("cvssV3_0", "3.0"),
    ("cvssV2_0", "2.0"),
];

/// What cve.org's record says of a CVE.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct Record {
    pub id: String,
    /// `PUBLISHED` or `REJECTED`. An id that is only reserved has no
    /// record at all.
    pub state: String,
    /// The CNA that assigned the CVE, by its short name (`redhat`,
    /// `GitHub_M`).
    pub assigner: String,
    /// The same CNA by its id in the CVE Program, which is what NVD
    /// gives as a record's `sourceIdentifier` for the CNAs it does not
    /// know by a mail address.
    pub assigner_org_id: String,
    /// The CNA's title. Many records have none.
    pub title: Option<String>,
    /// The CNA's description in English or, for a rejected record,
    /// its reason for the rejection.
    pub description: Option<String>,
    /// The CNA's references, then every other party's, each once.
    pub references: Vec<String>,
    /// One score per party that gave one, the CNA's first.
    pub scores: Vec<Score>,
}

impl Record {
    /// Whether the record was rejected: its CNA withdrew it, so there
    /// is no vulnerability.
    pub fn is_rejected(&self) -> bool {
        self.state.eq_ignore_ascii_case("REJECTED")
    }

    /// The record in the shape of NVD's answer, for the code that reads
    /// a CVE through `sandogasa-nvd`: the id, the CNA, the description
    /// and the references, with no CPE data and no score. That is what
    /// NVD itself serves for a CVE it has not analyzed yet, so whatever
    /// reads one of those reads this. Its status is
    /// [`STAND_IN_STATUS`], never `Rejected`: a rejection is asked of
    /// the record itself ([`Self::is_rejected`]), so that it is not
    /// reported as NVD's word.
    pub fn as_nvd(&self) -> Option<sandogasa_nvd::models::CveResponse> {
        let descriptions: Vec<Value> = self
            .description
            .iter()
            .map(|d| serde_json::json!({"lang": "en", "value": d}))
            .collect();
        let references: Vec<Value> = self
            .references
            .iter()
            .map(|u| serde_json::json!({"url": u}))
            .collect();
        serde_json::from_value(serde_json::json!({"vulnerabilities": [{"cve": {
            "id": self.id,
            "sourceIdentifier": self.assigner_org_id,
            "vulnStatus": STAND_IN_STATUS,
            "descriptions": descriptions,
            "references": references,
        }}]}))
        .ok()
    }
}

/// One party's CVSS score of a CVE.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Score {
    /// `3.1`, `4.0`, `3.0` or `2.0`.
    pub version: String,
    pub score: f64,
    /// Lower case: `critical`, `high`, `medium`, `low`, `none`.
    pub severity: String,
    /// Who scored it, by short name: the CNA (`redhat`) or another
    /// party (`CISA-ADP`).
    pub by: String,
}

/// cve.org's API URL for a CVE's record.
pub fn record_url(cve_id: &str) -> String {
    format!("https://cveawg.mitre.org/api/cve/{cve_id}")
}

/// The record in cve.org's answer for a CVE. `None` when the answer is
/// not a record: the error cve.org gives for an id it has published
/// nothing under, or anything that does not parse.
pub fn parse(json: &str) -> Option<Record> {
    let v: Value = serde_json::from_str(json).ok()?;
    let meta = &v["cveMetadata"];
    let id = text(&meta["cveId"])?;
    let state = text(&meta["state"])?;
    let cna = &v["containers"]["cna"];
    let adps: &[Value] = v["containers"]["adp"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    let assigner = text(&meta["assignerShortName"])
        .or_else(|| text(&cna["providerMetadata"]["shortName"]))
        .unwrap_or_default();

    let mut references: Vec<String> = Vec::new();
    let mut scores = Vec::new();
    scores.extend(score(cna, &assigner));
    for container in std::iter::once(cna).chain(adps) {
        for url in container["references"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| text(&r["url"]))
        {
            if !references.contains(&url) {
                references.push(url);
            }
        }
    }
    for adp in adps {
        let by = text(&adp["providerMetadata"]["shortName"]).unwrap_or_else(|| "ADP".to_string());
        scores.extend(score(adp, &by));
    }

    Some(Record {
        id,
        state,
        assigner,
        assigner_org_id: text(&meta["assignerOrgId"]).unwrap_or_default(),
        title: text(&cna["title"]),
        description: english(&cna["descriptions"]).or_else(|| english(&cna["rejectedReasons"])),
        references,
        scores,
    })
}

/// A JSON string, trimmed; `None` when it is absent or empty.
fn text(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The English entry of a record's list of texts in several languages
/// (`en`, `en-US`).
fn english(texts: &Value) -> Option<String> {
    texts.as_array()?.iter().find_map(|t| {
        t["lang"]
            .as_str()?
            .to_ascii_lowercase()
            .starts_with("en")
            .then(|| text(&t["value"]))?
    })
}

/// The score a container gives, by `by`: in the first CVSS version of
/// [`CVSS_KEYS`] it used. One without a severity word (CVSS 2 data has
/// none) is passed over, since the word is what a rating is read by.
fn score(container: &Value, by: &str) -> Option<Score> {
    let metrics = container["metrics"].as_array()?;
    CVSS_KEYS.iter().find_map(|(key, version)| {
        metrics.iter().find_map(|m| {
            let data = m.get(key)?;
            Some(Score {
                version: version.to_string(),
                score: data["baseScore"].as_f64()?,
                severity: data["baseSeverity"].as_str()?.to_ascii_lowercase(),
                by: by.to_string(),
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from cve.org and trimmed: CVE-2024-3094 (xz), which its
    /// CNA scored and titled, with CISA's container and the CVE
    /// Program's own.
    const XZ: &str = r#"{
        "dataType": "CVE_RECORD", "dataVersion": "5.2",
        "cveMetadata": {"cveId": "CVE-2024-3094", "state": "PUBLISHED",
            "assignerOrgId": "53f830b8-0a3f-465b-8143-3b8a9948e749",
            "assignerShortName": "redhat",
            "datePublished": "2024-03-29T16:51:12.588Z"},
        "containers": {
            "cna": {
                "title": "Xz: malicious code in distributed source",
                "metrics": [
                    {"other": {"type": "Red Hat severity rating",
                        "content": {"value": "Critical"}}},
                    {"cvssV3_1": {"baseScore": 10, "baseSeverity": "CRITICAL",
                        "vectorString": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:C/C:H/I:H/A:H",
                        "version": "3.1"}, "format": "CVSS"}],
                "descriptions": [
                    {"lang": "es", "value": "Se descubrió código malicioso."},
                    {"lang": "en", "value": "Malicious code was discovered in the upstream tarballs of xz."}],
                "references": [
                    {"url": "https://access.redhat.com/security/cve/CVE-2024-3094",
                     "tags": ["vdb-entry"]},
                    {"url": "https://github.com/tukaani-project/xz/issues/92"}],
                "providerMetadata": {"shortName": "redhat"}},
            "adp": [
                {"title": "CISA ADP Vulnrichment",
                 "metrics": [{"other": {"type": "ssvc", "content": {}}}],
                 "providerMetadata": {"shortName": "CISA-ADP"}},
                {"title": "CVE Program Container",
                 "references": [
                    {"url": "https://github.com/tukaani-project/xz/issues/92"},
                    {"url": "https://www.openwall.com/lists/oss-security/2024/03/29/4"}],
                 "providerMetadata": {"shortName": "CVE"}}]}}"#;

    #[test]
    fn a_record_gives_the_cna_s_title_description_references_and_score() {
        let r = parse(XZ).expect("a record");
        assert_eq!(r.id, "CVE-2024-3094");
        assert_eq!(r.state, "PUBLISHED");
        assert!(!r.is_rejected());
        assert_eq!(r.assigner, "redhat");
        assert_eq!(r.assigner_org_id, "53f830b8-0a3f-465b-8143-3b8a9948e749");
        assert_eq!(
            r.title.as_deref(),
            Some("Xz: malicious code in distributed source")
        );
        // The English description, not the first.
        assert_eq!(
            r.description.as_deref(),
            Some("Malicious code was discovered in the upstream tarballs of xz.")
        );
        // The CNA's references first, then the others', each once.
        assert_eq!(
            r.references,
            [
                "https://access.redhat.com/security/cve/CVE-2024-3094",
                "https://github.com/tukaani-project/xz/issues/92",
                "https://www.openwall.com/lists/oss-security/2024/03/29/4",
            ]
        );
        // The CVSS score; Red Hat's own rating and CISA's SSVC decision
        // are not scores.
        assert_eq!(
            r.scores,
            [Score {
                version: "3.1".to_string(),
                score: 10.0,
                severity: "critical".to_string(),
                by: "redhat".to_string(),
            }]
        );
    }

    /// CVE-2023-44487 (HTTP/2 rapid reset): MITRE's own record has no
    /// score, CISA's container has one.
    #[test]
    fn a_score_only_another_party_gave_is_read_from_its_container() {
        let r = parse(
            r#"{"cveMetadata": {"cveId": "CVE-2023-44487", "state": "PUBLISHED",
                    "assignerShortName": "mitre"},
                "containers": {
                    "cna": {"descriptions": [{"lang": "en", "value": "The HTTP/2 protocol allows a denial of service."}],
                        "providerMetadata": {"shortName": "mitre"}},
                    "adp": [{"metrics": [
                            {"cvssV3_1": {"baseScore": 7.5, "baseSeverity": "HIGH"}},
                            {"other": {"type": "kev", "content": {}}}],
                        "providerMetadata": {"shortName": "CISA-ADP"}}]}}"#,
        )
        .expect("a record");
        assert_eq!(r.title, None);
        assert_eq!(r.scores.len(), 1);
        assert_eq!(
            (r.scores[0].by.as_str(), r.scores[0].score),
            ("CISA-ADP", 7.5)
        );
        assert_eq!(r.scores[0].severity, "high");
    }

    #[test]
    fn each_party_s_score_is_the_first_version_it_used_and_has_a_word() {
        let r = parse(
            r#"{"cveMetadata": {"cveId": "CVE-2026-1", "state": "PUBLISHED",
                    "assignerShortName": "cna"},
                "containers": {
                    "cna": {"metrics": [
                        {"cvssV4_0": {"baseScore": 6.9, "baseSeverity": "MEDIUM"}},
                        {"cvssV3_1": {"baseScore": 8.1, "baseSeverity": "HIGH"}}]},
                    "adp": [
                        {"metrics": [{"cvssV2_0": {"baseScore": 5.0}}],
                         "providerMetadata": {"shortName": "old"}},
                        {"metrics": [{"cvssV3_0": {"baseScore": 4.2, "baseSeverity": "MEDIUM"}}]}]}}"#,
        )
        .expect("a record");
        let read: Vec<(&str, &str, f64)> = r
            .scores
            .iter()
            .map(|s| (s.by.as_str(), s.version.as_str(), s.score))
            .collect();
        // 3.1 before 4.0 for the CNA; CVSS 2 has no severity word; a
        // container that does not name its provider is still read.
        assert_eq!(read, [("cna", "3.1", 8.1), ("ADP", "3.0", 4.2)]);
    }

    /// CVE-2024-35918: withdrawn by its CNA.
    #[test]
    fn a_rejected_record_says_so_and_why() {
        let r = parse(
            r#"{"cveMetadata": {"cveId": "CVE-2024-35918", "state": "REJECTED",
                    "assignerShortName": "Linux",
                    "dateRejected": "2024-07-30T04:55:53.692Z"},
                "containers": {"cna": {
                    "providerMetadata": {"shortName": "Linux"},
                    "rejectedReasons": [{"lang": "en", "value":
                        "This CVE ID has been rejected or withdrawn by its CVE Numbering Authority."}]}}}"#,
        )
        .expect("a record");
        assert!(r.is_rejected());
        assert_eq!(
            r.description.as_deref(),
            Some("This CVE ID has been rejected or withdrawn by its CVE Numbering Authority.")
        );
        assert!(r.references.is_empty() && r.scores.is_empty());
    }

    #[test]
    fn an_answer_that_is_not_a_record_is_none() {
        // What cve.org answers, with a 404, for an id it has published
        // nothing under.
        assert_eq!(
            parse(
                r#"{"error": "CVE_RECORD_DNE",
                    "message": "The cve record for the cve id does not exist."}"#
            ),
            None
        );
        assert_eq!(parse("<html>busy</html>"), None);
        assert_eq!(parse(r#"{"cveMetadata": {"cveId": "CVE-2026-1"}}"#), None);
    }

    #[test]
    fn a_record_reads_as_an_nvd_answer_with_no_analysis() {
        let record = parse(XZ).unwrap();
        let nvd = record.as_nvd().expect("NVD's shape");
        assert_eq!(nvd.vulnerabilities.len(), 1);
        assert_eq!(nvd.vulnerabilities[0].cve.id, "CVE-2024-3094");
        assert_eq!(
            nvd.english_description(),
            Some("Malicious code was discovered in the upstream tarballs of xz.")
        );
        assert_eq!(nvd.reference_urls().len(), 3);
        assert_eq!(
            nvd.github_repos(),
            [("tukaani-project".to_string(), "xz".to_string())]
        );
        assert_eq!(nvd.vuln_status(), STAND_IN_STATUS);
        // Nothing NVD's analysis would add.
        assert!(nvd.fixed_versions().is_empty());
        assert!(nvd.affected_upstream().is_empty());
        assert_eq!(nvd.cvss(), None);
    }

    #[test]
    fn a_stand_in_keeps_the_cna_and_is_never_rejected() {
        // The OpenJS Foundation's id, which NVD's JavaScript test knows.
        let mut record = Record {
            id: "CVE-2026-2".to_string(),
            state: "REJECTED".to_string(),
            assigner_org_id: "ce714d77-add3-4f53-aff5-83d477b104bb".to_string(),
            ..Record::default()
        };
        let nvd = record.as_nvd().expect("NVD's shape");
        assert!(nvd.targets_js());
        assert_eq!(nvd.english_description(), None);
        assert_ne!(nvd.vuln_status().to_ascii_lowercase(), "rejected");
        record.assigner_org_id.clear();
        assert!(!record.as_nvd().unwrap().targets_js());
    }

    #[test]
    fn the_record_is_asked_of_cve_org_s_api() {
        assert_eq!(
            record_url("CVE-2024-3094"),
            "https://cveawg.mitre.org/api/cve/CVE-2024-3094"
        );
    }
}
