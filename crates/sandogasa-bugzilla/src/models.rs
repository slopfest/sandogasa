// SPDX-License-Identifier: MPL-2.0

use chrono::{DateTime, Utc};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct BugSearchResponse {
    pub bugs: Vec<Bug>,
    pub total_matches: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct Bug {
    pub id: u64,
    pub summary: String,
    pub status: String,
    pub resolution: String,
    pub product: String,
    pub component: Vec<String>,
    pub severity: String,
    pub priority: String,
    pub assigned_to: String,
    pub creator: String,
    pub creation_time: DateTime<Utc>,
    pub last_change_time: DateTime<Utc>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub alias: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<u64>,
    #[serde(default)]
    pub blocks: Vec<u64>,
    #[serde(default)]
    pub see_also: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub flags: Vec<Flag>,
    #[serde(default)]
    pub version: Vec<String>,
    #[serde(default)]
    pub cf_fixed_in: String,
}

#[derive(Debug, Deserialize)]
pub struct Flag {
    pub name: String,
    pub status: String,
    #[serde(default)]
    pub setter: String,
    #[serde(default)]
    pub requestee: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CommentResponse {
    pub bugs: std::collections::HashMap<String, CommentBucket>,
}

#[derive(Debug, Deserialize)]
pub struct CommentBucket {
    pub comments: Vec<Comment>,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub id: u64,
    pub text: String,
    pub creator: String,
    pub creation_time: DateTime<Utc>,
    pub is_private: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_flag_with_all_fields() {
        let json = serde_json::json!({
            "name": "needinfo",
            "status": "?",
            "setter": "dev@example.com",
            "requestee": "reviewer@example.com"
        });
        let flag: Flag = serde_json::from_value(json).unwrap();
        assert_eq!(flag.name, "needinfo");
        assert_eq!(flag.status, "?");
        assert_eq!(flag.setter, "dev@example.com");
        assert_eq!(flag.requestee.as_deref(), Some("reviewer@example.com"));
    }

    #[test]
    fn deserialize_flag_without_optional_fields() {
        let json = serde_json::json!({
            "name": "fedora-review",
            "status": "+"
        });
        let flag: Flag = serde_json::from_value(json).unwrap();
        assert_eq!(flag.name, "fedora-review");
        assert_eq!(flag.status, "+");
        assert_eq!(flag.setter, "");
        assert!(flag.requestee.is_none());
    }

    #[test]
    fn deserialize_comment() {
        let json = serde_json::json!({
            "id": 42,
            "text": "Fixed in rawhide",
            "creator": "dev@example.com",
            "creation_time": "2025-06-01T14:30:00Z",
            "is_private": false
        });
        let comment: Comment = serde_json::from_value(json).unwrap();
        assert_eq!(comment.id, 42);
        assert_eq!(comment.text, "Fixed in rawhide");
        assert_eq!(comment.creator, "dev@example.com");
        assert!(!comment.is_private);
    }

    #[test]
    fn deserialize_comment_private() {
        let json = serde_json::json!({
            "id": 99,
            "text": "Internal note",
            "creator": "security@redhat.com",
            "creation_time": "2025-07-01T08:00:00Z",
            "is_private": true
        });
        let comment: Comment = serde_json::from_value(json).unwrap();
        assert!(comment.is_private);
    }

    #[test]
    fn deserialize_bug_with_defaults() {
        let json = serde_json::json!({
            "id": 100,
            "summary": "Test bug",
            "status": "NEW",
            "resolution": "",
            "product": "Fedora",
            "component": ["kernel"],
            "severity": "medium",
            "priority": "unspecified",
            "assigned_to": "nobody@fedoraproject.org",
            "creator": "reporter@example.com",
            "creation_time": "2025-01-01T00:00:00Z",
            "last_change_time": "2025-01-01T00:00:00Z"
        });
        let bug: Bug = serde_json::from_value(json).unwrap();
        assert_eq!(bug.id, 100);
        assert!(bug.keywords.is_empty());
        assert!(bug.alias.is_empty());
        assert!(bug.depends_on.is_empty());
        assert!(bug.blocks.is_empty());
        assert!(bug.see_also.is_empty());
        assert!(bug.cc.is_empty());
        assert!(bug.flags.is_empty());
        assert!(bug.version.is_empty());
        assert_eq!(bug.cf_fixed_in, "");
    }

    #[test]
    fn deserialize_bug_with_flags_and_aliases() {
        let json = serde_json::json!({
            "id": 200,
            "summary": "CVE-2025-9999 kernel: overflow",
            "status": "ASSIGNED",
            "resolution": "",
            "product": "Fedora",
            "component": ["kernel"],
            "severity": "high",
            "priority": "urgent",
            "assigned_to": "dev@example.com",
            "creator": "secalert@redhat.com",
            "creation_time": "2025-03-01T00:00:00Z",
            "last_change_time": "2025-03-02T00:00:00Z",
            "alias": ["CVE-2025-9999"],
            "flags": [
                {"name": "needinfo", "status": "?", "setter": "a@b.com", "requestee": "c@d.com"},
                {"name": "fedora-review", "status": "+"}
            ],
            "depends_on": [100, 101],
            "blocks": [300],
            "cc": ["watcher@example.com"],
            "cf_fixed_in": "6.12.5"
        });
        let bug: Bug = serde_json::from_value(json).unwrap();
        assert_eq!(bug.alias, vec!["CVE-2025-9999"]);
        assert_eq!(bug.flags.len(), 2);
        assert_eq!(bug.flags[0].name, "needinfo");
        assert_eq!(bug.flags[1].name, "fedora-review");
        assert_eq!(bug.depends_on, vec![100, 101]);
        assert_eq!(bug.blocks, vec![300]);
        assert_eq!(bug.cc, vec!["watcher@example.com"]);
        assert_eq!(bug.cf_fixed_in, "6.12.5");
    }

    #[test]
    fn deserialize_bug_search_response() {
        let json = serde_json::json!({
            "bugs": [{
                "id": 1,
                "summary": "Bug",
                "status": "NEW",
                "resolution": "",
                "product": "Fedora",
                "component": ["test"],
                "severity": "low",
                "priority": "low",
                "assigned_to": "nobody@fedoraproject.org",
                "creator": "r@e.com",
                "creation_time": "2025-01-01T00:00:00Z",
                "last_change_time": "2025-01-01T00:00:00Z"
            }],
            "total_matches": 42
        });
        let resp: BugSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.bugs.len(), 1);
        assert_eq!(resp.total_matches, Some(42));
    }

    #[test]
    fn deserialize_bug_search_response_without_total() {
        let json = serde_json::json!({
            "bugs": []
        });
        let resp: BugSearchResponse = serde_json::from_value(json).unwrap();
        assert!(resp.bugs.is_empty());
        assert!(resp.total_matches.is_none());
    }

    #[test]
    fn deserialize_comment_response() {
        let json = serde_json::json!({
            "bugs": {
                "12345": {
                    "comments": [{
                        "id": 1,
                        "text": "Hello",
                        "creator": "a@b.com",
                        "creation_time": "2025-01-01T00:00:00Z",
                        "is_private": false
                    }]
                }
            }
        });
        let resp: CommentResponse = serde_json::from_value(json).unwrap();
        let bucket = resp.bugs.get("12345").unwrap();
        assert_eq!(bucket.comments.len(), 1);
        assert_eq!(bucket.comments[0].text, "Hello");
    }
}
