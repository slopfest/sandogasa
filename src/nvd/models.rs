// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CveResponse {
    pub vulnerabilities: Vec<Vulnerability>,
}

#[derive(Debug, Deserialize)]
pub struct Vulnerability {
    pub cve: CveItem,
}

#[derive(Debug, Deserialize)]
pub struct CveItem {
    #[allow(dead_code)]
    pub id: String,
    #[serde(default, rename = "sourceIdentifier")]
    pub source_identifier: String,
    #[serde(default)]
    pub descriptions: Vec<CveDescription>,
    #[serde(default)]
    pub configurations: Vec<Configuration>,
}

#[derive(Debug, Deserialize)]
pub struct CveDescription {
    pub lang: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct Configuration {
    #[serde(default)]
    pub nodes: Vec<Node>,
}

#[derive(Debug, Deserialize)]
pub struct Node {
    #[serde(default, rename = "cpeMatch")]
    pub cpe_match: Vec<CpeMatch>,
}

#[derive(Debug, Deserialize)]
pub struct CpeMatch {
    pub criteria: String,
}

impl CpeMatch {
    /// Check if this CPE match targets node.js as its target software.
    /// CPE 2.3 format: cpe:2.3:part:vendor:product:version:update:edition:language:sw_edition:target_sw:target_hw:other
    /// target_sw is at index 10 (0-based).
    pub fn targets_js(&self) -> bool {
        self.criteria
            .split(':')
            .nth(10)
            .is_some_and(|sw| sw.eq_ignore_ascii_case("node.js"))
    }
}

/// CNAs (CVE Numbering Authorities) known to exclusively handle JavaScript projects.
/// Maps source identifier (UUID or email) to a human-readable name.
const JS_CNAS: &[(&str, &str)] = &[
    ("ce714d77-add3-4f53-aff5-83d477b104bb", "OpenJS Foundation"),
];

/// Keywords that indicate a JavaScript-related CVE when found in the description.
const JS_KEYWORDS: &[&str] = &[
    "javascript",
    "node.js",
    "nodejs",
    "npm package",
    "npm module",
];

impl CveResponse {
    /// Check if this CVE targets JavaScript/NodeJS using three strategies:
    /// 1. CPE data (authoritative, if available)
    /// 2. CNA source (e.g. OpenJS Foundation)
    /// 3. Description keyword matching (fallback)
    pub fn targets_js(&self) -> bool {
        let cpe_matches: Vec<_> = self
            .vulnerabilities
            .iter()
            .flat_map(|v| &v.cve.configurations)
            .flat_map(|c| &c.nodes)
            .flat_map(|n| &n.cpe_match)
            .collect();

        // If CPE data exists, use it authoritatively
        if !cpe_matches.is_empty() {
            return cpe_matches.iter().any(|m| m.targets_js());
        }

        // Check if the CNA is a known JavaScript-only authority
        let is_js_cna = self.vulnerabilities.iter().any(|v| {
            let src = &v.cve.source_identifier;
            JS_CNAS.iter().any(|(id, _)| src == id)
        });
        if is_js_cna {
            return true;
        }

        // Fallback: check English description for JS keywords
        self.vulnerabilities.iter().any(|v| {
            v.cve
                .descriptions
                .iter()
                .filter(|d| d.lang == "en")
                .any(|d| {
                    let lower = d.value.to_lowercase();
                    JS_KEYWORDS.iter().any(|kw| lower.contains(kw))
                })
        })
    }
}
