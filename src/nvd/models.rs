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
    pub id: String,
    #[serde(default)]
    pub configurations: Vec<Configuration>,
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
    pub fn targets_nodejs(&self) -> bool {
        self.criteria
            .split(':')
            .nth(10)
            .is_some_and(|sw| sw.eq_ignore_ascii_case("node.js"))
    }
}
