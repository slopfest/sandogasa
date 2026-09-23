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

/// The version a GitHub release-tag URL names — `…/releases/tag/v1.23.1`
/// gives `1.23.1` — when a CVE's references point at the release that
/// carried the fix. A candidate like any read from prose: the caller
/// confirms it. `None` for any other URL, or a tag that is not a
/// dotted version.
pub fn release_tag_version(url: &str) -> Option<String> {
    let (_, tag) = url.split_once("/releases/tag/")?;
    let tag = tag.split(['?', '#', '/']).next()?.trim_start_matches('v');
    (tag.contains('.')
        && tag.starts_with(|c: char| c.is_ascii_digit())
        && tag.ends_with(|c: char| c.is_ascii_digit()))
    .then(|| tag.to_string())
}

/// The repository advisories a CVE's references name —
/// `github.com/<owner>/<repo>/security/advisories/GHSA-…` — as
/// `(owner, repo, ghsa_id)`, each once. Those are the advisories a
/// project publishes itself; the global database only carries them
/// once GitHub has reviewed them, so a fresh CVE is often findable
/// here and nowhere else.
pub fn repo_advisory_refs(urls: &[String]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for url in urls {
        let Some(rest) = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("http://github.com/"))
        else {
            continue;
        };
        let parts: Vec<&str> = rest.split('/').collect();
        if let [owner, repo, "security", "advisories", ghsa, ..] = parts.as_slice()
            && ghsa.starts_with("GHSA-")
        {
            let key = (owner.to_string(), repo.to_string(), ghsa.to_string());
            if !out.contains(&key) {
                out.push(key);
            }
        }
    }
    out
}

/// GitHub's API endpoint for one repository advisory.
pub fn repo_advisory_url(owner: &str, repo: &str, ghsa_id: &str) -> String {
    format!("https://api.github.com/repos/{owner}/{repo}/security-advisories/{ghsa_id}")
}

/// A repository advisory's affected packages, in the shape
/// [`ghsa_records`] gives for the global database. The repository
/// form spells the fix as `patched_versions`, a string that may list
/// several (`1.23.3, 1.24.0`) and may carry a `v`; each becomes one
/// record with the same range, and an advisory that names no patched
/// version yields one record with none.
pub fn repo_advisory_records(json: &str) -> Vec<(String, Vec<GhsaVuln>)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(id) = v.get("ghsa_id").and_then(|i| i.as_str()) else {
        return Vec::new();
    };
    let vulns: Vec<GhsaVuln> = v
        .get("vulnerabilities")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .flat_map(|entry| {
            let text = |key: &str| entry.get(key).and_then(|v| v.as_str()).map(str::to_string);
            let package = entry
                .pointer("/package/name")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            let ecosystem = entry
                .pointer("/package/ecosystem")
                .and_then(|p| p.as_str())
                .unwrap_or_default()
                .to_string();
            let range = text("vulnerable_version_range");
            // Projects write the field by hand: `v1.23.3`, `>= v1.23.0`,
            // `1.23.3, 1.24.0`. Keep the version alone.
            let patched: Vec<Option<String>> = match text("patched_versions") {
                Some(p) if !p.trim().is_empty() => p
                    .split(',')
                    .map(|s| {
                        Some(
                            s.trim()
                                .trim_start_matches(['>', '=', ' '])
                                .trim_start_matches('v')
                                .to_string(),
                        )
                    })
                    .collect(),
                _ => vec![None],
            };
            patched
                .into_iter()
                .map(move |patched| GhsaVuln {
                    ecosystem: ecosystem.clone(),
                    package: package.clone(),
                    range: range.clone(),
                    patched,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    vec![(id.to_string(), vulns)]
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

    #[test]
    fn release_tag_urls_name_a_candidate_version() {
        assert_eq!(
            release_tag_version("https://github.com/strukturag/libheif/releases/tag/v1.23.1")
                .as_deref(),
            Some("1.23.1")
        );
        assert_eq!(
            release_tag_version("https://github.com/x/y/releases/tag/2.0#notes").as_deref(),
            Some("2.0")
        );
        assert_eq!(
            release_tag_version("https://github.com/x/y/releases/tag/nightly"),
            None
        );
        assert_eq!(
            release_tag_version("https://github.com/x/y/commit/abc"),
            None
        );
    }

    #[test]
    fn repository_advisories_are_read_from_references_and_their_api_shape() {
        let refs = vec![
            "https://github.com/strukturag/libheif/commit/089a809".to_string(),
            "https://github.com/strukturag/libheif/security/advisories/GHSA-73p7-m7gg-w2jv"
                .to_string(),
            "https://github.com/strukturag/libheif/security/advisories/GHSA-73p7-m7gg-w2jv"
                .to_string(),
        ];
        assert_eq!(
            repo_advisory_refs(&refs),
            vec![(
                "strukturag".to_string(),
                "libheif".to_string(),
                "GHSA-73p7-m7gg-w2jv".to_string()
            )]
        );
        assert_eq!(
            repo_advisory_url("strukturag", "libheif", "GHSA-73p7-m7gg-w2jv"),
            "https://api.github.com/repos/strukturag/libheif/security-advisories/GHSA-73p7-m7gg-w2jv"
        );
        // What GitHub answered for that advisory on 2026-09-23.
        let json = r#"{"ghsa_id": "GHSA-73p7-m7gg-w2jv", "cve_id": "CVE-2026-62292",
            "vulnerabilities": [{"package": {"ecosystem": "", "name": "libheif"},
            "vulnerable_version_range": ">= 1.19.0", "patched_versions": "v1.23.3"}]}"#;
        let records = repo_advisory_records(json);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].0, "GHSA-73p7-m7gg-w2jv");
        assert_eq!(
            records[0].1,
            vec![GhsaVuln {
                ecosystem: String::new(),
                package: "libheif".into(),
                range: Some(">= 1.19.0".into()),
                patched: Some("1.23.3".into()),
            }]
        );
        // An operator in the field is the project's phrasing, not a range.
        let op = r#"{"ghsa_id": "GHSA-op", "vulnerabilities": [{"package": {"name": "libheif"},
            "vulnerable_version_range": ">= 1.19.0, <= 1.22.2", "patched_versions": ">= v1.23.0"}]}"#;
        assert_eq!(
            repo_advisory_records(op)[0].1[0].patched.as_deref(),
            Some("1.23.0")
        );
        // Several patched versions become one record each; none becomes one without.
        let two = r#"{"ghsa_id": "GHSA-x", "vulnerabilities": [{"package": {"name": "p"},
            "vulnerable_version_range": "< 2", "patched_versions": "1.9.1, 2.0.1"}]}"#;
        let p: Vec<Option<String>> = repo_advisory_records(two)[0]
            .1
            .iter()
            .map(|v| v.patched.clone())
            .collect();
        assert_eq!(p, vec![Some("1.9.1".into()), Some("2.0.1".into())]);
        let none = r#"{"ghsa_id": "GHSA-y", "vulnerabilities": [{"package": {"name": "p"},
            "vulnerable_version_range": ">= 1.0", "patched_versions": ""}]}"#;
        assert_eq!(repo_advisory_records(none)[0].1[0].patched, None);
    }
}
