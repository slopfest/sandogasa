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

#[cfg(test)]
mod tests {
    use super::*;

    // ---- helpers ----

    fn cpe_match(criteria: &str) -> CpeMatch {
        CpeMatch {
            criteria: criteria.to_string(),
        }
    }

    fn cve_with_cpe(criteria: &str) -> CveResponse {
        CveResponse {
            vulnerabilities: vec![Vulnerability {
                cve: CveItem {
                    id: "CVE-2026-0001".to_string(),
                    source_identifier: String::new(),
                    descriptions: vec![],
                    configurations: vec![Configuration {
                        nodes: vec![Node {
                            cpe_match: vec![cpe_match(criteria)],
                        }],
                    }],
                },
            }],
        }
    }

    fn cve_with_source(source_id: &str) -> CveResponse {
        CveResponse {
            vulnerabilities: vec![Vulnerability {
                cve: CveItem {
                    id: "CVE-2026-0002".to_string(),
                    source_identifier: source_id.to_string(),
                    descriptions: vec![],
                    configurations: vec![],
                },
            }],
        }
    }

    fn cve_with_description(lang: &str, text: &str) -> CveResponse {
        CveResponse {
            vulnerabilities: vec![Vulnerability {
                cve: CveItem {
                    id: "CVE-2026-0003".to_string(),
                    source_identifier: String::new(),
                    descriptions: vec![CveDescription {
                        lang: lang.to_string(),
                        value: text.to_string(),
                    }],
                    configurations: vec![],
                },
            }],
        }
    }

    fn empty_cve() -> CveResponse {
        CveResponse {
            vulnerabilities: vec![Vulnerability {
                cve: CveItem {
                    id: "CVE-2026-0000".to_string(),
                    source_identifier: String::new(),
                    descriptions: vec![],
                    configurations: vec![],
                },
            }],
        }
    }

    // ---- CpeMatch::targets_js ----

    #[test]
    fn cpe_targets_js_with_node_target_sw() {
        let m = cpe_match(
            "cpe:2.3:a:axios:axios:*:*:*:*:*:node.js:*:*",
        );
        assert!(m.targets_js());
    }

    #[test]
    fn cpe_targets_js_case_insensitive() {
        let m = cpe_match(
            "cpe:2.3:a:axios:axios:*:*:*:*:*:Node.JS:*:*",
        );
        assert!(m.targets_js());
    }

    #[test]
    fn cpe_does_not_target_js_wildcard() {
        let m = cpe_match(
            "cpe:2.3:a:vendor:product:1.0:*:*:*:*:*:*:*",
        );
        assert!(!m.targets_js());
    }

    #[test]
    fn cpe_does_not_target_js_python() {
        let m = cpe_match(
            "cpe:2.3:a:vendor:product:1.0:*:*:*:*:python:*:*",
        );
        assert!(!m.targets_js());
    }

    #[test]
    fn cpe_short_string_does_not_panic() {
        let m = cpe_match("cpe:2.3:a:vendor");
        assert!(!m.targets_js());
    }

    #[test]
    fn cpe_empty_string_does_not_panic() {
        let m = cpe_match("");
        assert!(!m.targets_js());
    }

    // ---- CveResponse::targets_js — strategy 1: CPE ----

    #[test]
    fn response_targets_js_via_cpe() {
        let resp = cve_with_cpe("cpe:2.3:a:axios:axios:*:*:*:*:*:node.js:*:*");
        assert!(resp.targets_js());
    }

    #[test]
    fn response_not_js_via_cpe() {
        let resp = cve_with_cpe("cpe:2.3:a:vendor:product:*:*:*:*:*:*:*:*");
        assert!(!resp.targets_js());
    }

    #[test]
    fn cpe_is_authoritative_over_description() {
        // CPE says not-JS, description says JS — CPE should win
        let mut resp = cve_with_cpe("cpe:2.3:a:vendor:product:*:*:*:*:*:*:*:*");
        resp.vulnerabilities[0].cve.descriptions.push(CveDescription {
            lang: "en".to_string(),
            value: "A vulnerability in a Node.js package".to_string(),
        });
        assert!(!resp.targets_js());
    }

    // ---- CveResponse::targets_js — strategy 2: CNA source ----

    #[test]
    fn response_targets_js_via_openjs_cna() {
        let resp = cve_with_source("ce714d77-add3-4f53-aff5-83d477b104bb");
        assert!(resp.targets_js());
    }

    #[test]
    fn response_not_js_via_unknown_cna() {
        let resp = cve_with_source("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
        assert!(!resp.targets_js());
    }

    // ---- CveResponse::targets_js — strategy 3: description keywords ----

    #[test]
    fn response_targets_js_via_keyword_nodejs() {
        let resp = cve_with_description("en", "A vulnerability in a Node.js HTTP library");
        assert!(resp.targets_js());
    }

    #[test]
    fn response_targets_js_via_keyword_npm_package() {
        let resp = cve_with_description("en", "Cross-site scripting in npm package foo-bar");
        assert!(resp.targets_js());
    }

    #[test]
    fn response_targets_js_via_keyword_javascript() {
        let resp = cve_with_description("en", "Prototype pollution in a JavaScript library");
        assert!(resp.targets_js());
    }

    #[test]
    fn response_not_js_via_description_unrelated() {
        let resp = cve_with_description("en", "Buffer overflow in libpng 1.6.40");
        assert!(!resp.targets_js());
    }

    #[test]
    fn response_ignores_non_english_description() {
        let resp = cve_with_description("es", "Vulnerabilidad en un paquete npm module foo");
        assert!(!resp.targets_js());
    }

    #[test]
    fn response_keyword_matching_is_case_insensitive() {
        let resp = cve_with_description("en", "Flaw in NODEJS allows remote code execution");
        assert!(resp.targets_js());
    }

    // ---- CveResponse::targets_js — edge cases ----

    #[test]
    fn response_no_vulnerabilities() {
        let resp = CveResponse {
            vulnerabilities: vec![],
        };
        assert!(!resp.targets_js());
    }

    #[test]
    fn response_empty_cve_not_js() {
        let resp = empty_cve();
        assert!(!resp.targets_js());
    }

    #[test]
    fn response_multiple_cpe_one_js() {
        let resp = CveResponse {
            vulnerabilities: vec![Vulnerability {
                cve: CveItem {
                    id: "CVE-2026-1234".to_string(),
                    source_identifier: String::new(),
                    descriptions: vec![],
                    configurations: vec![Configuration {
                        nodes: vec![Node {
                            cpe_match: vec![
                                cpe_match("cpe:2.3:a:vendor:product:*:*:*:*:*:*:*:*"),
                                cpe_match("cpe:2.3:a:axios:axios:*:*:*:*:*:node.js:*:*"),
                            ],
                        }],
                    }],
                },
            }],
        };
        assert!(resp.targets_js());
    }
}
