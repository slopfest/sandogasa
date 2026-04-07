// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct UpdatesResponse {
    pub updates: Vec<Update>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

#[derive(Debug, Deserialize)]
pub struct Update {
    pub alias: String,
    pub status: String,
    #[serde(default)]
    pub builds: Vec<Build>,
    #[serde(default)]
    pub from_side_tag: Option<String>,
    #[serde(default)]
    pub bugs: Vec<BodhiBug>,
    #[serde(default)]
    pub release: Option<Release>,
    #[serde(default)]
    pub date_submitted: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Build {
    pub nvr: String,
}

#[derive(Debug, Deserialize)]
pub struct BodhiBug {
    pub bug_id: u64,
}

#[derive(Debug, Deserialize)]
pub struct Release {
    pub name: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SingleUpdateResponse {
    pub update: Update,
}

#[derive(Debug, Deserialize)]
pub struct CommentsResponse {
    pub comments: Vec<Comment>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

#[derive(Debug, Deserialize)]
pub struct Comment {
    pub id: u64,
    #[serde(default)]
    pub text: String,
    pub karma: i32,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub update_alias: Option<String>,
}

/// A Bodhi release entry from the releases API.
#[derive(Debug, Deserialize)]
pub struct BodhiRelease {
    pub name: String,
    pub branch: String,
    pub id_prefix: String,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct ReleasesResponse {
    pub releases: Vec<BodhiRelease>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_updates_response() {
        let json = r#"{
            "updates": [
                {
                    "alias": "FEDORA-2026-abc123",
                    "status": "stable",
                    "builds": [
                        {"nvr": "freerdp-3.23.0-1.fc42"}
                    ],
                    "bugs": [
                        {"bug_id": 2442801}
                    ],
                    "release": {"name": "F42"},
                    "date_submitted": "2026-02-25 11:55:26"
                }
            ],
            "total": 1,
            "page": 1,
            "pages": 1
        }"#;

        let resp: UpdatesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.updates.len(), 1);
        assert_eq!(resp.updates[0].alias, "FEDORA-2026-abc123");
        assert_eq!(resp.updates[0].status, "stable");
        assert_eq!(resp.updates[0].builds.len(), 1);
        assert_eq!(resp.updates[0].builds[0].nvr, "freerdp-3.23.0-1.fc42");
        assert_eq!(resp.updates[0].bugs.len(), 1);
        assert_eq!(resp.updates[0].bugs[0].bug_id, 2442801);
        assert_eq!(resp.updates[0].release.as_ref().unwrap().name, "F42");
        assert_eq!(
            resp.updates[0].date_submitted.as_deref(),
            Some("2026-02-25 11:55:26")
        );
        assert_eq!(resp.total, 1);
        assert_eq!(resp.page, 1);
        assert_eq!(resp.pages, 1);
    }

    #[test]
    fn deserialize_update_with_side_tag() {
        let json = r#"{
            "alias": "FEDORA-EPEL-2026-abc123",
            "status": "testing",
            "from_side_tag": "epel9-build-side-133287",
            "builds": [
                {"nvr": "rust-uucore-0.0.28-2.el9"}
            ]
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(
            update.from_side_tag.as_deref(),
            Some("epel9-build-side-133287")
        );
    }

    #[test]
    fn deserialize_update_without_side_tag() {
        let json = r#"{
            "alias": "FEDORA-2026-xyz",
            "status": "testing",
            "builds": [{"nvr": "foo-1.0-1.fc42"}]
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert!(update.from_side_tag.is_none());
    }

    #[test]
    fn deserialize_update_minimal() {
        let json = r#"{
            "alias": "FEDORA-2026-xyz",
            "status": "testing"
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.alias, "FEDORA-2026-xyz");
        assert_eq!(update.status, "testing");
        assert!(update.builds.is_empty());
        assert!(update.bugs.is_empty());
        assert!(update.release.is_none());
        assert!(update.date_submitted.is_none());
    }

    #[test]
    fn deserialize_multiple_builds() {
        let json = r#"{
            "alias": "FEDORA-2026-multi",
            "status": "stable",
            "builds": [
                {"nvr": "freerdp-3.23.0-1.fc42"},
                {"nvr": "freerdp-libs-3.23.0-1.fc42"}
            ],
            "bugs": [],
            "release": {"name": "F42"}
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.builds.len(), 2);
        assert_eq!(update.builds[0].nvr, "freerdp-3.23.0-1.fc42");
        assert_eq!(update.builds[1].nvr, "freerdp-libs-3.23.0-1.fc42");
    }

    #[test]
    fn deserialize_empty_response() {
        let json = r#"{
            "updates": [],
            "total": 0,
            "page": 1,
            "pages": 0
        }"#;

        let resp: UpdatesResponse = serde_json::from_str(json).unwrap();
        assert!(resp.updates.is_empty());
        assert_eq!(resp.total, 0);
    }

    #[test]
    fn deserialize_multiple_bugs() {
        let json = r#"{
            "alias": "FEDORA-2026-bugs",
            "status": "stable",
            "bugs": [
                {"bug_id": 100},
                {"bug_id": 200},
                {"bug_id": 300}
            ]
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.bugs.len(), 3);
        assert_eq!(update.bugs[0].bug_id, 100);
        assert_eq!(update.bugs[2].bug_id, 300);
    }

    // ---- BodhiRelease / ReleasesResponse ----

    #[test]
    fn deserialize_releases_response() {
        let json = r#"{
            "releases": [
                {
                    "name": "F43",
                    "branch": "f43",
                    "id_prefix": "FEDORA",
                    "state": "current"
                },
                {
                    "name": "EPEL-9",
                    "branch": "epel9",
                    "id_prefix": "FEDORA-EPEL",
                    "state": "current"
                }
            ],
            "total": 2,
            "page": 1,
            "pages": 1
        }"#;

        let resp: ReleasesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.releases.len(), 2);
        assert_eq!(resp.releases[0].name, "F43");
        assert_eq!(resp.releases[0].branch, "f43");
        assert_eq!(resp.releases[0].id_prefix, "FEDORA");
        assert_eq!(resp.releases[0].state, "current");
        assert_eq!(resp.releases[1].name, "EPEL-9");
        assert_eq!(resp.releases[1].branch, "epel9");
    }

    #[test]
    fn deserialize_releases_empty() {
        let json = r#"{
            "releases": [],
            "total": 0,
            "page": 1,
            "pages": 0
        }"#;

        let resp: ReleasesResponse = serde_json::from_str(json).unwrap();
        assert!(resp.releases.is_empty());
    }

    // ---- Comment / CommentsResponse ----

    #[test]
    fn deserialize_comments_response() {
        let json = r#"{
            "comments": [
                {
                    "id": 4559905,
                    "text": "Checking interaction with packages",
                    "karma": 0,
                    "timestamp": "2026-02-24 11:17:59",
                    "author": "salimma",
                    "update_alias": "FEDORA-EPEL-2026-8e235e20a2"
                }
            ],
            "total": 1,
            "page": 1,
            "pages": 1
        }"#;

        let resp: CommentsResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.comments.len(), 1);
        assert_eq!(resp.comments[0].id, 4559905);
        assert_eq!(resp.comments[0].karma, 0);
        assert_eq!(resp.comments[0].author.as_deref(), Some("salimma"));
        assert_eq!(
            resp.comments[0].update_alias.as_deref(),
            Some("FEDORA-EPEL-2026-8e235e20a2")
        );
    }

    #[test]
    fn deserialize_comment_with_karma() {
        let json = r#"{
            "id": 123,
            "text": "Works for me",
            "karma": 1,
            "timestamp": "2026-03-01 10:00:00",
            "author": "reviewer",
            "update_alias": "FEDORA-2026-abc"
        }"#;

        let comment: Comment = serde_json::from_str(json).unwrap();
        assert_eq!(comment.karma, 1);
        assert_eq!(comment.text, "Works for me");
    }

    #[test]
    fn deserialize_comment_negative_karma() {
        let json = r#"{
            "id": 456,
            "text": "Broken on aarch64",
            "karma": -1
        }"#;

        let comment: Comment = serde_json::from_str(json).unwrap();
        assert_eq!(comment.karma, -1);
        assert!(comment.author.is_none());
        assert!(comment.update_alias.is_none());
    }

    #[test]
    fn deserialize_comments_empty() {
        let json = r#"{
            "comments": [],
            "total": 0,
            "page": 1,
            "pages": 0
        }"#;

        let resp: CommentsResponse = serde_json::from_str(json).unwrap();
        assert!(resp.comments.is_empty());
    }
}
