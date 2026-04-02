// SPDX-License-Identifier: MPL-2.0

use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Serialize;

/// Build the CBS web URL for a given build ID.
pub fn build_url(build_id: i64) -> String {
    format!("https://cbs.centos.org/koji/buildinfo?buildID={build_id}")
}

/// The highest promotion stage a build has reached in Hyperscale tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagStage {
    /// Only in -candidate tags.
    Candidate,
    /// In -testing tags (but not -release).
    Testing,
    /// In -release tags.
    Release,
}

impl std::fmt::Display for TagStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TagStage::Candidate => write!(f, "candidate"),
            TagStage::Testing => write!(f, "testing"),
            TagStage::Release => write!(f, "release"),
        }
    }
}

/// A completed build from the CBS Koji instance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Build {
    pub build_id: i64,
    pub name: String,
    pub version: String,
    pub release: String,
    pub nvr: String,
}

impl Build {
    /// Whether this is a Hyperscale build (release contains ".hs.").
    pub fn is_hyperscale(&self) -> bool {
        self.release.contains(".hs.")
    }

    /// Return the EL version this build targets (e.g. 9, 10), if detectable.
    ///
    /// Matches `.elN` or `.elN_Z` at the end of the release string.
    pub fn el_version(&self) -> Option<u32> {
        let s = self.release.rsplit_once(".el")?;
        // s.1 is e.g. "9", "10", "9_3"
        let num = s.1.split('_').next()?;
        num.parse().ok()
    }
}

/// Client for the CentOS Build System (CBS) Koji XML-RPC API.
pub struct Client {
    http: reqwest::blocking::Client,
    hub_url: String,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self::with_hub_url("https://cbs.centos.org/kojihub")
    }

    pub fn with_hub_url(hub_url: &str) -> Self {
        let http = reqwest::blocking::Client::builder()
            .user_agent("hs-relmon/0.1.0")
            .build()
            .expect("failed to build HTTP client");
        Self {
            http,
            hub_url: hub_url.trim_end_matches('/').to_string(),
        }
    }

    /// Look up the numeric package ID for a package name.
    pub fn get_package_id(&self, name: &str) -> Result<Option<i64>, Box<dyn std::error::Error>> {
        let body = format!(
            r#"<?xml version="1.0"?>
<methodCall>
  <methodName>getPackageID</methodName>
  <params>
    <param><value><string>{name}</string></value></param>
  </params>
</methodCall>"#
        );
        let resp = self.call(&body)?;
        // Response is a single <int> or <nil/>
        let value = parse_single_value(&resp)?;
        match value {
            XmlRpcValue::Int(id) => Ok(Some(id)),
            XmlRpcValue::Nil => Ok(None),
            other => Err(format!("unexpected response type: {other:?}").into()),
        }
    }

    /// List completed builds for a package, newest first.
    pub fn list_builds(&self, package_id: i64) -> Result<Vec<Build>, Box<dyn std::error::Error>> {
        // listBuilds(packageID, userID, taskID, prefix, state, ..., queryOpts)
        // state=1 means COMPLETE
        // 14 positional params total, last one is queryOpts
        let body = format!(
            r#"<?xml version="1.0"?>
<methodCall>
  <methodName>listBuilds</methodName>
  <params>
    <param><value><int>{package_id}</int></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><int>1</int></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><nil/></value></param>
    <param><value><struct>
      <member><name>order</name><value><string>-build_id</string></value></member>
    </struct></value></param>
  </params>
</methodCall>"#
        );
        let resp = self.call(&body)?;
        parse_builds(&resp)
    }

    /// List tag names for a given build ID.
    pub fn list_tags(&self, build_id: i64) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let body = format!(
            r#"<?xml version="1.0"?>
<methodCall>
  <methodName>listTags</methodName>
  <params>
    <param><value><int>{build_id}</int></value></param>
  </params>
</methodCall>"#
        );
        let resp = self.call(&body)?;
        parse_tag_names(&resp)
    }

    /// Find the latest Hyperscale release and testing builds for an EL version.
    ///
    /// Walks builds newest-first, checking tags for each. Returns the latest
    /// build in release, and the latest build in testing if it's newer than
    /// the release build.
    pub fn hyperscale_summary(
        &self,
        builds: &[Build],
        el_version: u32,
    ) -> Result<HyperscaleSummary, Box<dyn std::error::Error>> {
        resolve_summary(builds, el_version, |build_id| self.list_tags(build_id))
    }

    fn call(&self, body: &str) -> Result<String, Box<dyn std::error::Error>> {
        let resp = self
            .http
            .post(&self.hub_url)
            .header("Content-Type", "text/xml")
            .body(body.to_string())
            .send()?
            .text()?;
        Ok(resp)
    }
}

/// Summary of the latest Hyperscale builds for an EL version.
#[derive(Debug, Clone, Serialize)]
pub struct HyperscaleSummary {
    /// Latest build tagged for release.
    pub release: Option<Build>,
    /// Latest build tagged for testing, only if newer than the release build.
    pub testing: Option<Build>,
}

/// Filter builds to Hyperscale builds for a given EL version.
///
/// Preserves ordering (assumed newest-first by descending build_id).
pub fn hyperscale_builds(builds: &[Build], el_version: u32) -> Vec<&Build> {
    builds
        .iter()
        .filter(|b| b.is_hyperscale() && b.el_version() == Some(el_version))
        .collect()
}

/// Walk Hyperscale builds for an EL version and resolve the summary.
///
/// Uses the provided `lookup_tags` function to get tags for each build,
/// allowing the caller to supply either a real API call or a test stub.
pub fn resolve_summary<F>(
    builds: &[Build],
    el_version: u32,
    lookup_tags: F,
) -> Result<HyperscaleSummary, Box<dyn std::error::Error>>
where
    F: Fn(i64) -> Result<Vec<String>, Box<dyn std::error::Error>>,
{
    let mut summary = HyperscaleSummary {
        release: None,
        testing: None,
    };

    for build in hyperscale_builds(builds, el_version) {
        let tags = lookup_tags(build.build_id)?;
        let stage = tag_stage(&tags);

        match stage {
            Some(TagStage::Release) => {
                summary.release = Some(build.clone());
                break;
            }
            Some(TagStage::Testing) => {
                if summary.testing.is_none() {
                    summary.testing = Some(build.clone());
                }
            }
            _ => {}
        }
    }

    Ok(summary)
}

/// Determine the highest promotion stage from a list of tag names.
///
/// Looks for Hyperscale tags ending in `-release`, `-testing`, or `-candidate`.
pub fn tag_stage(tags: &[String]) -> Option<TagStage> {
    let mut stage: Option<TagStage> = None;
    for tag in tags {
        if !tag.starts_with("hyperscale") {
            continue;
        }
        let new = if tag.ends_with("-release") {
            TagStage::Release
        } else if tag.ends_with("-testing") {
            TagStage::Testing
        } else if tag.ends_with("-candidate") {
            TagStage::Candidate
        } else {
            continue;
        };
        stage = Some(match stage {
            None => new,
            Some(TagStage::Release) => TagStage::Release,
            Some(TagStage::Testing) if new == TagStage::Release => TagStage::Release,
            Some(TagStage::Testing) => TagStage::Testing,
            Some(TagStage::Candidate) => new,
        });
    }
    stage
}

// --- XML-RPC response parsing ---

#[derive(Debug, Clone, PartialEq)]
enum XmlRpcValue {
    Int(i64),
    Str(String),
    Nil,
    Array(Vec<XmlRpcValue>),
    Struct(Vec<(String, XmlRpcValue)>),
}

/// Parse a methodResponse containing a single return value.
fn parse_single_value(xml: &str) -> Result<XmlRpcValue, Box<dyn std::error::Error>> {
    let values = parse_response_values(xml)?;
    values
        .into_iter()
        .next()
        .ok_or_else(|| "empty response".into())
}

/// Parse the top-level values from a methodResponse.
fn parse_response_values(xml: &str) -> Result<Vec<XmlRpcValue>, Box<dyn std::error::Error>> {
    // Find the <params> section and parse each <param><value>...</value></param>
    let mut reader = Reader::from_str(xml);
    let mut values = Vec::new();
    let mut depth = Vec::<String>::new();

    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth.push(tag);
                if depth == ["methodResponse", "params", "param", "value"] {
                    let val = parse_value(&mut reader, &mut depth)?;
                    values.push(val);
                }
            }
            Event::End(_) => {
                depth.pop();
            }
            Event::Empty(e) => {
                // Handle <fault/> or similar
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "fault" {
                    return Err("XML-RPC fault".into());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(values)
}

/// Parse a single <value>...</value>. Assumes we just entered <value>.
fn parse_value(
    reader: &mut Reader<&[u8]>,
    depth: &mut Vec<String>,
) -> Result<XmlRpcValue, Box<dyn std::error::Error>> {
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth.push(tag.clone());
                match tag.as_str() {
                    "int" | "i4" | "i8" => {
                        let text = reader.read_text(e.name())?;
                        depth.pop();
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Int(text.trim().parse()?));
                    }
                    "string" => {
                        let text = reader.read_text(e.name())?;
                        depth.pop();
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Str(text.to_string()));
                    }
                    "array" => {
                        let arr = parse_array(reader, depth)?;
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Array(arr));
                    }
                    "struct" => {
                        let members = parse_struct(reader, depth)?;
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Struct(members));
                    }
                    "nil" => {
                        let _ = reader.read_text(e.name())?;
                        depth.pop();
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Nil);
                    }
                    _ => {
                        // Unknown type, read as string
                        let text = reader.read_text(e.name())?;
                        depth.pop();
                        consume_end_value(reader, depth)?;
                        return Ok(XmlRpcValue::Str(text.to_string()));
                    }
                }
            }
            Event::Empty(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "nil" {
                    consume_end_value(reader, depth)?;
                    return Ok(XmlRpcValue::Nil);
                }
            }
            Event::Text(e) => {
                // Bare text inside <value> without type tag = string
                let text = e.unescape()?.to_string();
                if !text.trim().is_empty() {
                    consume_end_value(reader, depth)?;
                    return Ok(XmlRpcValue::Str(text));
                }
            }
            Event::End(_) => {
                // </value> with no content
                depth.pop();
                return Ok(XmlRpcValue::Nil);
            }
            Event::Eof => return Err("unexpected EOF in value".into()),
            _ => {}
        }
    }
}

fn consume_end_value(
    reader: &mut Reader<&[u8]>,
    depth: &mut Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Read until we hit </value>
    loop {
        match reader.read_event()? {
            Event::End(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth.pop();
                if tag == "value" {
                    return Ok(());
                }
            }
            Event::Eof => return Err("unexpected EOF waiting for </value>".into()),
            _ => {}
        }
    }
}

fn parse_array(
    reader: &mut Reader<&[u8]>,
    depth: &mut Vec<String>,
) -> Result<Vec<XmlRpcValue>, Box<dyn std::error::Error>> {
    let mut items = Vec::new();
    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth.push(tag.clone());
                if tag == "value" {
                    items.push(parse_value(reader, depth)?);
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "array" {
                    depth.pop();
                    return Ok(items);
                }
                depth.pop();
            }
            Event::Eof => return Err("unexpected EOF in array".into()),
            _ => {}
        }
    }
}

fn parse_struct(
    reader: &mut Reader<&[u8]>,
    depth: &mut Vec<String>,
) -> Result<Vec<(String, XmlRpcValue)>, Box<dyn std::error::Error>> {
    let mut members = Vec::new();
    let mut current_name: Option<String> = None;

    loop {
        match reader.read_event()? {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                depth.push(tag.clone());
                match tag.as_str() {
                    "name" => {
                        let text = reader.read_text(e.name())?;
                        depth.pop();
                        current_name = Some(text.to_string());
                    }
                    "value" => {
                        let val = parse_value(reader, depth)?;
                        if let Some(name) = current_name.take() {
                            members.push((name, val));
                        }
                    }
                    _ => {}
                }
            }
            Event::End(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if tag == "struct" {
                    depth.pop();
                    return Ok(members);
                }
                depth.pop();
            }
            Event::Eof => return Err("unexpected EOF in struct".into()),
            _ => {}
        }
    }
}

/// Parse a listTags response into tag name strings.
fn parse_tag_names(xml: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let value = parse_single_value(xml)?;
    let XmlRpcValue::Array(items) = value else {
        return Err("expected array response".into());
    };

    let mut names = Vec::new();
    for item in items {
        let XmlRpcValue::Struct(members) = item else {
            continue;
        };
        for (key, val) in &members {
            if key == "name"
                && let XmlRpcValue::Str(v) = val
            {
                names.push(v.clone());
            }
        }
    }
    Ok(names)
}

/// Parse a listBuilds response into Build objects.
fn parse_builds(xml: &str) -> Result<Vec<Build>, Box<dyn std::error::Error>> {
    let value = parse_single_value(xml)?;
    let XmlRpcValue::Array(items) = value else {
        return Err("expected array response".into());
    };

    let mut builds = Vec::new();
    for item in items {
        let XmlRpcValue::Struct(members) = item else {
            continue;
        };
        let mut build_id = 0i64;
        let mut name = String::new();
        let mut version = String::new();
        let mut release = String::new();
        let mut nvr = String::new();

        for (key, val) in &members {
            match key.as_str() {
                "build_id" => {
                    if let XmlRpcValue::Int(v) = val {
                        build_id = *v;
                    }
                }
                "name" | "package_name" => {
                    if let XmlRpcValue::Str(v) = val
                        && name.is_empty()
                    {
                        name = v.clone();
                    }
                }
                "version" => {
                    if let XmlRpcValue::Str(v) = val {
                        version = v.clone();
                    }
                }
                "release" => {
                    if let XmlRpcValue::Str(v) = val {
                        release = v.clone();
                    }
                }
                "nvr" => {
                    if let XmlRpcValue::Str(v) = val {
                        nvr = v.clone();
                    }
                }
                _ => {}
            }
        }

        if !nvr.is_empty() {
            builds.push(Build {
                build_id,
                name,
                version,
                release,
                nvr,
            });
        }
    }

    Ok(builds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_is_hyperscale() {
        let hs = Build {
            build_id: 1,
            name: "ethtool".into(),
            version: "6.15".into(),
            release: "3.hs.el10".into(),
            nvr: "ethtool-6.15-3.hs.el10".into(),
        };
        assert!(hs.is_hyperscale());

        let non_hs = Build {
            build_id: 2,
            name: "ethtool".into(),
            version: "6.2".into(),
            release: "1.el9sbase_901".into(),
            nvr: "ethtool-6.2-1.el9sbase_901".into(),
        };
        assert!(!non_hs.is_hyperscale());
    }

    #[test]
    fn test_el_version() {
        let cases = [
            ("3.hs.el9", Some(9)),
            ("3.hs.el10", Some(10)),
            ("1.hs.el9_3", Some(9)),
            ("1.hs.el10_2", Some(10)),
            ("1.el9sbase_901", None),
            ("2.el10s~1", None),
        ];
        for (release, expected) in cases {
            let b = Build {
                build_id: 1,
                name: "test".into(),
                version: "1".into(),
                release: release.into(),
                nvr: format!("test-1-{release}"),
            };
            assert_eq!(b.el_version(), expected, "release={release}");
        }
    }

    #[test]
    fn test_hyperscale_builds_filters_by_el_version() {
        let builds = vec![
            Build {
                build_id: 3,
                name: "ethtool".into(),
                version: "6.15".into(),
                release: "2.el10s~1".into(),
                nvr: "ethtool-6.15-2.el10s~1".into(),
            },
            Build {
                build_id: 2,
                name: "ethtool".into(),
                version: "6.15".into(),
                release: "3.hs.el9".into(),
                nvr: "ethtool-6.15-3.hs.el9".into(),
            },
            Build {
                build_id: 1,
                name: "ethtool".into(),
                version: "6.14".into(),
                release: "1.hs.el10".into(),
                nvr: "ethtool-6.14-1.hs.el10".into(),
            },
        ];
        let el9 = hyperscale_builds(&builds, 9);
        assert_eq!(el9.len(), 1);
        assert_eq!(el9[0].nvr, "ethtool-6.15-3.hs.el9");

        let el10 = hyperscale_builds(&builds, 10);
        assert_eq!(el10.len(), 1);
        assert_eq!(el10[0].nvr, "ethtool-6.14-1.hs.el10");

        assert!(hyperscale_builds(&builds, 8).is_empty());
    }

    #[test]
    fn test_hyperscale_builds_empty() {
        let builds = vec![Build {
            build_id: 1,
            name: "ethtool".into(),
            version: "6.2".into(),
            release: "1.el9sbase_901".into(),
            nvr: "ethtool-6.2-1.el9sbase_901".into(),
        }];
        assert!(hyperscale_builds(&builds, 9).is_empty());
    }

    fn make_build(build_id: i64, version: &str, release: &str) -> Build {
        Build {
            build_id,
            name: "pkg".into(),
            version: version.into(),
            release: release.into(),
            nvr: format!("pkg-{version}-{release}"),
        }
    }

    fn mock_tags(
        mapping: &[(i64, &[&str])],
    ) -> impl Fn(i64) -> Result<Vec<String>, Box<dyn std::error::Error>> {
        let map: std::collections::HashMap<i64, Vec<String>> = mapping
            .iter()
            .map(|(id, tags)| (*id, tags.iter().map(|s| s.to_string()).collect()))
            .collect();
        move |build_id| Ok(map.get(&build_id).cloned().unwrap_or_default())
    }

    #[test]
    fn test_resolve_summary_release_only() {
        let builds = vec![make_build(3, "6.15", "3.hs.el9")];
        let tags = mock_tags(&[(3, &["hyperscale9s-packages-main-release"])]);
        let summary = resolve_summary(&builds, 9, tags).unwrap();
        assert_eq!(summary.release.as_ref().unwrap().build_id, 3);
        assert!(summary.testing.is_none());
    }

    #[test]
    fn test_resolve_summary_testing_then_release() {
        // Newest build (id=4) is testing-only, older build (id=3) is release
        let builds = vec![
            make_build(4, "260~rc2", "20260309.hs.el9"),
            make_build(3, "258.5", "1.1.hs.el9"),
        ];
        let tags = mock_tags(&[
            (4, &["hyperscale9s-packages-main-testing"]),
            (3, &["hyperscale9s-packages-main-release"]),
        ]);
        let summary = resolve_summary(&builds, 9, tags).unwrap();
        assert_eq!(summary.release.as_ref().unwrap().version, "258.5");
        assert_eq!(summary.testing.as_ref().unwrap().version, "260~rc2");
    }

    #[test]
    fn test_resolve_summary_testing_only() {
        let builds = vec![make_build(4, "260~rc2", "20260309.hs.el10")];
        let tags = mock_tags(&[(4, &["hyperscale10s-packages-main-testing"])]);
        let summary = resolve_summary(&builds, 10, tags).unwrap();
        assert!(summary.release.is_none());
        assert_eq!(summary.testing.as_ref().unwrap().version, "260~rc2");
    }

    #[test]
    fn test_resolve_summary_skips_candidate() {
        // Build 5 is candidate-only, build 4 is testing, build 3 is release
        let builds = vec![
            make_build(5, "261", "1.hs.el9"),
            make_build(4, "260", "1.hs.el9"),
            make_build(3, "258", "1.hs.el9"),
        ];
        let tags = mock_tags(&[
            (5, &["hyperscale9s-packages-main-candidate"]),
            (4, &["hyperscale9s-packages-main-testing"]),
            (3, &["hyperscale9s-packages-main-release"]),
        ]);
        let summary = resolve_summary(&builds, 9, tags).unwrap();
        assert_eq!(summary.release.as_ref().unwrap().version, "258");
        assert_eq!(summary.testing.as_ref().unwrap().version, "260");
    }

    #[test]
    fn test_resolve_summary_empty() {
        let builds: Vec<Build> = vec![];
        let tags = mock_tags(&[]);
        let summary = resolve_summary(&builds, 9, tags).unwrap();
        assert!(summary.release.is_none());
        assert!(summary.testing.is_none());
    }

    #[test]
    fn test_resolve_summary_no_testing_when_release_is_latest() {
        // The latest build is already in release; no testing line needed
        let builds = vec![
            make_build(3, "6.15", "3.hs.el10"),
            make_build(2, "6.14", "1.hs.el10"),
        ];
        let tags = mock_tags(&[
            (3, &["hyperscale10s-packages-main-release"]),
            // build 2 would also be release but we stop at 3
        ]);
        let summary = resolve_summary(&builds, 10, tags).unwrap();
        assert_eq!(summary.release.as_ref().unwrap().version, "6.15");
        assert!(summary.testing.is_none());
    }

    #[test]
    fn test_parse_get_package_id_response() {
        let xml = r#"<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><int>8491</int></value>
</param>
</params>
</methodResponse>"#;
        let val = parse_single_value(xml).unwrap();
        assert_eq!(val, XmlRpcValue::Int(8491));
    }

    #[test]
    fn test_parse_nil_response() {
        let xml = r#"<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><nil/></value>
</param>
</params>
</methodResponse>"#;
        let val = parse_single_value(xml).unwrap();
        assert_eq!(val, XmlRpcValue::Nil);
    }

    #[test]
    fn test_parse_builds_response() {
        let xml = include_str!("../tests/fixtures/koji_builds.xml");
        let builds = parse_builds(xml).unwrap();
        assert_eq!(builds.len(), 3);

        assert_eq!(builds[0].nvr, "ethtool-6.15-3.hs.el9");
        assert_eq!(builds[0].build_id, 61758);
        assert_eq!(builds[0].version, "6.15");
        assert_eq!(builds[0].release, "3.hs.el9");
        assert!(builds[0].is_hyperscale());

        assert_eq!(builds[1].nvr, "ethtool-6.15-3.hs.el10");
        assert!(builds[1].is_hyperscale());

        assert_eq!(builds[2].nvr, "ethtool-6.14-1.hs.el10");
    }

    #[test]
    fn test_parse_empty_array() {
        let xml = r#"<?xml version='1.0'?>
<methodResponse>
<params>
<param>
<value><array><data></data></array></value>
</param>
</params>
</methodResponse>"#;
        let builds = parse_builds(xml).unwrap();
        assert!(builds.is_empty());
    }

    #[test]
    fn test_parse_tag_names() {
        let xml = include_str!("../tests/fixtures/koji_tags.xml");
        let names = parse_tag_names(xml).unwrap();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "hyperscale9s-packages-main-candidate");
        assert_eq!(names[1], "hyperscale9s-packages-main-testing");
        assert_eq!(names[2], "hyperscale9s-packages-main-release");
    }

    #[test]
    fn test_tag_stage_release() {
        let tags = vec![
            "hyperscale9s-packages-main-candidate".into(),
            "hyperscale9s-packages-main-testing".into(),
            "hyperscale9s-packages-main-release".into(),
        ];
        assert_eq!(tag_stage(&tags), Some(TagStage::Release));
    }

    #[test]
    fn test_tag_stage_testing_only() {
        let tags = vec!["hyperscale10s-packages-main-testing".into()];
        assert_eq!(tag_stage(&tags), Some(TagStage::Testing));
    }

    #[test]
    fn test_tag_stage_candidate_only() {
        let tags = vec!["hyperscale9s-packages-main-candidate".into()];
        assert_eq!(tag_stage(&tags), Some(TagStage::Candidate));
    }

    #[test]
    fn test_tag_stage_no_hyperscale_tags() {
        let tags = vec!["some-other-tag".into()];
        assert_eq!(tag_stage(&tags), None);
    }

    #[test]
    fn test_tag_stage_display() {
        assert_eq!(TagStage::Release.to_string(), "release");
        assert_eq!(TagStage::Testing.to_string(), "testing");
        assert_eq!(TagStage::Candidate.to_string(), "candidate");
    }

    #[test]
    fn test_build_url() {
        assert_eq!(
            build_url(70550),
            "https://cbs.centos.org/koji/buildinfo?buildID=70550"
        );
    }

    #[test]
    fn test_client_new() {
        let client = Client::new();
        assert_eq!(client.hub_url, "https://cbs.centos.org/kojihub");
    }

    #[test]
    fn test_client_with_hub_url_trims_slash() {
        let client = Client::with_hub_url("https://example.com/kojihub/");
        assert_eq!(client.hub_url, "https://example.com/kojihub");
    }
}
