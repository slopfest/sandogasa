// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Parse Koji build/task references from URLs or prefixed IDs.

use std::fmt;

/// A reference to a Koji build or task.
#[derive(Debug)]
pub struct KojiRef {
    pub instance: String,
    pub ref_type: RefType,
    pub id: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefType {
    Build,
    Task,
}

impl fmt::Display for RefType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RefType::Build => write!(f, "build"),
            RefType::Task => write!(f, "task"),
        }
    }
}

#[derive(Debug)]
pub struct ParseError(pub String);

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for ParseError {}

/// Parse a reference string into a [`KojiRef`].
///
/// Accepted formats:
/// - Full URL: `https://koji.fedoraproject.org/koji/buildinfo?buildID=2970379`
/// - Full URL: `https://koji.fedoraproject.org/koji/taskinfo?taskID=143927217`
/// - Prefixed: `build:2970379` (requires `default_instance`)
/// - Prefixed: `task:143927217` (requires `default_instance`)
pub fn parse_ref(input: &str, default_instance: Option<&str>) -> Result<KojiRef, ParseError> {
    if input.starts_with("http://") || input.starts_with("https://") {
        return parse_url(input);
    }

    if let Some(id_str) = input.strip_prefix("build:") {
        let instance = default_instance
            .ok_or_else(|| ParseError("--instance required for bare IDs".into()))?;
        let id = id_str
            .parse::<i64>()
            .map_err(|_| ParseError(format!("invalid build ID: {id_str}")))?;
        return Ok(KojiRef {
            instance: instance.to_string(),
            ref_type: RefType::Build,
            id,
        });
    }

    if let Some(id_str) = input.strip_prefix("task:") {
        let instance = default_instance
            .ok_or_else(|| ParseError("--instance required for bare IDs".into()))?;
        let id = id_str
            .parse::<i64>()
            .map_err(|_| ParseError(format!("invalid task ID: {id_str}")))?;
        return Ok(KojiRef {
            instance: instance.to_string(),
            ref_type: RefType::Task,
            id,
        });
    }

    Err(ParseError(format!(
        "cannot parse reference: {input}\n\
         Expected: Koji URL, build:<ID>, or task:<ID>"
    )))
}

fn parse_url(url: &str) -> Result<KojiRef, ParseError> {
    let without_scheme = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);

    let (host, path_and_query) = without_scheme
        .split_once('/')
        .ok_or_else(|| ParseError(format!("cannot parse URL: {url}")))?;

    let instance = host.to_string();

    if (path_and_query.contains("buildinfo") || path_and_query.contains("buildID"))
        && let Some(id) = extract_param(url, "buildID")
    {
        return Ok(KojiRef {
            instance,
            ref_type: RefType::Build,
            id,
        });
    }

    if (path_and_query.contains("taskinfo") || path_and_query.contains("taskID"))
        && let Some(id) = extract_param(url, "taskID")
    {
        return Ok(KojiRef {
            instance,
            ref_type: RefType::Task,
            id,
        });
    }

    Err(ParseError(format!(
        "cannot extract build/task ID from URL: {url}"
    )))
}

fn extract_param(url: &str, param: &str) -> Option<i64> {
    let query = url.split('?').nth(1)?;
    for part in query.split('&') {
        if let Some(value) = part.strip_prefix(&format!("{param}=")) {
            return value.parse().ok();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_build_url() {
        let r = parse_ref(
            "https://koji.fedoraproject.org/koji/buildinfo?buildID=2970379",
            None,
        )
        .unwrap();
        assert_eq!(r.instance, "koji.fedoraproject.org");
        assert_eq!(r.ref_type, RefType::Build);
        assert_eq!(r.id, 2970379);
    }

    #[test]
    fn test_parse_task_url() {
        let r = parse_ref(
            "https://koji.fedoraproject.org/koji/taskinfo?taskID=143927217",
            None,
        )
        .unwrap();
        assert_eq!(r.instance, "koji.fedoraproject.org");
        assert_eq!(r.ref_type, RefType::Task);
        assert_eq!(r.id, 143927217);
    }

    #[test]
    fn test_parse_cbs_url() {
        let r = parse_ref("https://cbs.centos.org/koji/buildinfo?buildID=12345", None).unwrap();
        assert_eq!(r.instance, "cbs.centos.org");
        assert_eq!(r.ref_type, RefType::Build);
        assert_eq!(r.id, 12345);
    }

    #[test]
    fn test_parse_build_prefix() {
        let r = parse_ref("build:2970379", Some("koji.fedoraproject.org")).unwrap();
        assert_eq!(r.instance, "koji.fedoraproject.org");
        assert_eq!(r.ref_type, RefType::Build);
        assert_eq!(r.id, 2970379);
    }

    #[test]
    fn test_parse_task_prefix() {
        let r = parse_ref("task:143927217", Some("koji.fedoraproject.org")).unwrap();
        assert_eq!(r.instance, "koji.fedoraproject.org");
        assert_eq!(r.ref_type, RefType::Task);
        assert_eq!(r.id, 143927217);
    }

    #[test]
    fn test_parse_prefix_without_instance_fails() {
        let err = parse_ref("build:123", None).unwrap_err();
        assert!(err.0.contains("--instance"));
    }

    #[test]
    fn test_parse_invalid_ref() {
        let err = parse_ref("foobar", None).unwrap_err();
        assert!(err.0.contains("cannot parse"));
    }

    #[test]
    fn test_parse_url_with_multiple_params() {
        let r = parse_ref(
            "https://koji.fedoraproject.org/koji/taskinfo?foo=bar&taskID=999&baz=1",
            None,
        )
        .unwrap();
        assert_eq!(r.ref_type, RefType::Task);
        assert_eq!(r.id, 999);
    }

    #[test]
    fn test_parse_http_url() {
        let r = parse_ref(
            "http://koji.fedoraproject.org/koji/buildinfo?buildID=100",
            None,
        )
        .unwrap();
        assert_eq!(r.instance, "koji.fedoraproject.org");
        assert_eq!(r.id, 100);
    }
}
