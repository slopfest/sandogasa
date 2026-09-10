// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Reading a fixed version out of an advisory's prose.
//!
//! NVD's structured `configurations` are the authoritative source of
//! fixed versions, but a CVE sitting in `Awaiting Analysis` has none —
//! and often the version *is* public, written in a referenced
//! advisory. ZDI, for instance, ends every advisory with a line like
//! "Fixed in 7-Zip 26.02", and the oss-security posts that quote them
//! inherit it.
//!
//! What comes out of here is a *candidate*, never a decision: the
//! caller confirms it before anything is closed on the strength of
//! it, because a wrong version closes a live security bug.

/// One affected package in a GitHub Security Advisory: its name in
/// the advisory's ecosystem spelling (`GitPython` for pip), the
/// range GitHub marks vulnerable (`>= 6.30.0rc1, <= 6.33.4`) and the
/// first patched version, absent while upstream has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhsaVuln {
    /// GitHub's ecosystem name: `pip`, `rust`, `npm`, `go`, …
    pub ecosystem: String,
    pub package: String,
    pub range: Option<String>,
    pub patched: Option<String>,
}

/// GitHub's advisory database, asked for the advisories that carry a
/// CVE — NVD does not always list the GHSA among a CVE's references.
pub fn ghsa_search_url(cve_id: &str) -> String {
    format!("https://api.github.com/advisories?cve_id={cve_id}")
}

/// Fetch a GitHub API resource as JSON text. `token` is optional:
/// without one GitHub allows sixty requests an hour per address, which
/// a run with a few unresolved CVEs fits; `GITHUB_TOKEN` (the `gh`
/// CLI's variable) lifts that.
pub async fn fetch_github(
    http: &reqwest::Client,
    url: &str,
    token: Option<&str>,
) -> Option<String> {
    let mut req = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json");
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// The advisories in a search answer (or the one advisory in a single
/// record), each as its GHSA identifier and affected packages.
pub fn ghsa_records(json: &str) -> Vec<(String, Vec<GhsaVuln>)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let records: Vec<&serde_json::Value> = match v.as_array() {
        Some(a) => a.iter().collect(),
        None => vec![&v],
    };
    records
        .into_iter()
        .filter_map(|a| {
            let id = a.get("ghsa_id")?.as_str()?.to_string();
            let vulns = a
                .get("vulnerabilities")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|entry| {
                    let text =
                        |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
                    Some(GhsaVuln {
                        ecosystem: entry.pointer("/package/ecosystem")?.as_str()?.to_string(),
                        package: entry.pointer("/package/name")?.as_str()?.to_string(),
                        range: text("vulnerable_version_range"),
                        patched: text("first_patched_version"),
                    })
                })
                .collect();
            Some((id, vulns))
        })
        .collect()
}

/// A GHSA vulnerable range as NVD would state it: `>= 6.30.0rc1, <= 6.33.4`
/// bounds both ends, `< 5.29.6` names the fix, `<= 0.8.0` an unfixed
/// series, `= 1.2.3` a single version. `None` for a range that names
/// no bound this can read.
pub fn ghsa_range(product: &str, range: &str) -> Option<sandogasa_nvd::VulnerableRange> {
    let mut out = sandogasa_nvd::VulnerableRange {
        product: product.to_string(),
        start_including: None,
        end_excluding: None,
        end_including: None,
    };
    let mut any = false;
    for clause in range.split(',') {
        let clause = clause.trim();
        let (op, version) = clause.split_at(clause.find(|c: char| c.is_ascii_digit())?);
        let version = version.to_string();
        // A `>` lower bound is read as inclusive: the difference is one
        // version that no build is likely to carry.
        match op.trim() {
            ">=" | ">" => out.start_including = Some(version),
            "<" => out.end_excluding = Some(version),
            "<=" => out.end_including = Some(version),
            "=" | "" => {
                out.start_including = Some(version.clone());
                out.end_including = Some(version);
            }
            _ => continue,
        }
        any = true;
    }
    any.then_some(out)
}

/// Fetch an advisory page as text, or `None` if it can't be read.
/// Best-effort by design — these are third-party pages and a fetch
/// failure only costs the suggestion.
pub async fn fetch(http: &reqwest::Client, url: &str) -> Option<String> {
    let resp = http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.text().await.ok()
}

/// Distinct fixed-version candidates found in `text`, in the order
/// they appear.
///
/// Deliberately narrow: only "fixed in …" phrasing counts, and the
/// version must contain a dot, so prose like "fixed in 2026" or
/// "fixed in commit abc123" yields nothing. Several distinct
/// candidates mean the text is ambiguous and the caller should say so
/// rather than pick one.
pub fn fixed_version_candidates(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut out: Vec<String> = Vec::new();
    for (i, _) in lower.match_indices("fixed in ") {
        let rest = &text[i + "fixed in ".len()..];
        // Allow a product name between "fixed in" and the version
        // ("Fixed in 7-Zip 26.02"), so scan the next few words and
        // take the first that looks like a dotted version.
        for word in rest.split_whitespace().take(4) {
            // Trim to digit boundaries, so brackets, commas, a
            // trailing full stop and a leading "v" fall away while
            // internal dots survive. A product name like "7-Zip"
            // reduces to "7", which has no dot and is skipped.
            let candidate = word.trim_matches(|c: char| !c.is_ascii_digit());
            if is_version(candidate) {
                if !out.iter().any(|v| v == candidate) {
                    out.push(candidate.to_string());
                }
                break;
            }
        }
    }
    out
}

/// Whether `s` looks like a release version: digits and dots only,
/// with at least one dot, starting and ending with a digit. The dot
/// requirement is what keeps years and bare counts out.
fn is_version(s: &str) -> bool {
    s.contains('.')
        && s.starts_with(|c: char| c.is_ascii_digit())
        && s.ends_with(|c: char| c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_digit() || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghsa_records_read_each_advisory_and_package() {
        let json = r#"[{"ghsa_id":"GHSA-7gcm-g887-7qv7","cve_id":"CVE-2026-0994",
          "vulnerabilities":[
            {"package":{"ecosystem":"pip","name":"protobuf"},
             "vulnerable_version_range":">= 6.30.0rc1, <= 6.33.4","first_patched_version":"6.33.5"},
            {"package":{"ecosystem":"pip","name":"protobuf"},
             "vulnerable_version_range":"< 5.29.6","first_patched_version":"5.29.6"}]},
          {"ghsa_id":"GHSA-x4mc-mqm7-gg39","vulnerabilities":[
            {"package":{"ecosystem":"rust","name":"coreutils"},
             "vulnerable_version_range":"<= 0.8.0","first_patched_version":null}]}]"#;
        let records = ghsa_records(json);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].0, "GHSA-7gcm-g887-7qv7");
        assert_eq!(
            records[0].1[1],
            GhsaVuln {
                ecosystem: "pip".into(),
                package: "protobuf".into(),
                range: Some("< 5.29.6".into()),
                patched: Some("5.29.6".into()),
            }
        );
        assert_eq!(records[1].1[0].patched, None);
        // A single record (the per-GHSA endpoint) reads the same way.
        assert_eq!(
            ghsa_records(r#"{"ghsa_id":"GHSA-a","vulnerabilities":[]}"#).len(),
            1
        );
        assert!(ghsa_records("not json").is_empty());
        assert!(ghsa_records("[]").is_empty());
    }

    #[test]
    fn ghsa_range_reads_each_bound_shape() {
        let r = ghsa_range("protobuf", ">= 6.30.0rc1, <= 6.33.4").unwrap();
        assert_eq!(r.start_including.as_deref(), Some("6.30.0rc1"));
        assert_eq!(r.end_including.as_deref(), Some("6.33.4"));
        assert_eq!(r.end_excluding, None);
        let r = ghsa_range("protobuf", "< 5.29.6").unwrap();
        assert_eq!(r.end_excluding.as_deref(), Some("5.29.6"));
        let r = ghsa_range("x", "= 1.2.3").unwrap();
        assert_eq!(r.start_including.as_deref(), Some("1.2.3"));
        assert_eq!(r.end_including.as_deref(), Some("1.2.3"));
        assert!(ghsa_range("x", "unknown").is_none());
    }

    #[test]
    fn finds_the_zdi_phrasing() {
        // The real ZDI-26-444 line, as quoted into oss-security.
        let text = "> Additional Details\n>\n> Fixed in 7-Zip 26.02\n>\n> Disclosure Timeline";
        assert_eq!(fixed_version_candidates(text), vec!["26.02"]);
    }

    #[test]
    fn finds_version_without_a_product_name() {
        assert_eq!(
            fixed_version_candidates("This was fixed in 1.2.3, please update."),
            vec!["1.2.3"]
        );
        // Case-insensitive on the phrase.
        assert_eq!(fixed_version_candidates("FIXED IN 4.5"), vec!["4.5"]);
    }

    #[test]
    fn ignores_prose_without_a_dotted_version() {
        for text in [
            "This was fixed in 2026",
            "fixed in commit abc123",
            "fixed in the next release",
            "fixed in version v-next",
            "nothing relevant here",
        ] {
            assert!(
                fixed_version_candidates(text).is_empty(),
                "matched: {text:?}"
            );
        }
    }

    #[test]
    fn reports_every_distinct_candidate_and_dedupes() {
        let text = "Fixed in foo 1.2 and backported; also fixed in bar 3.4. Fixed in foo 1.2.";
        assert_eq!(fixed_version_candidates(text), vec!["1.2", "3.4"]);
    }

    #[test]
    fn stops_scanning_before_an_unrelated_later_number() {
        // The version has to be near the phrase, not anywhere after
        // it: "fixed in the upcoming release" must not reach 26.02.
        let text = "fixed in the upcoming major release, unlike 26.02";
        assert!(fixed_version_candidates(text).is_empty(), "{text}");
    }

    #[test]
    fn strips_surrounding_punctuation() {
        assert_eq!(fixed_version_candidates("fixed in (1.2.3)."), vec!["1.2.3"]);
        assert_eq!(
            fixed_version_candidates("fixed in 7-Zip 26.02."),
            vec!["26.02"]
        );
    }
}
