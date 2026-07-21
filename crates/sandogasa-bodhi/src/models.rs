// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct UpdatesResponse {
    pub updates: Vec<Update>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

/// The user who submitted an update.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct BodhiUser {
    pub name: String,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct Update {
    pub alias: String,
    pub status: String,
    /// Submitter. Bodhi zeroes overall karma from the submitter
    /// on their own updates (per-bug feedback still counts).
    #[serde(default)]
    pub user: Option<BodhiUser>,
    /// Bodhi's auto-generated NVR-joined title (space-separated
    /// list of builds). Rarely what a reader wants; prefer
    /// `display_name` or the first line of `notes`.
    #[serde(default)]
    pub title: Option<String>,
    /// User-set one-line display name shown as the update's
    /// heading in the Bodhi UI. Optional — often empty.
    #[serde(default)]
    pub display_name: Option<String>,
    /// User-written markdown notes. The first non-empty line is
    /// typically the update's human-readable summary, even when
    /// `display_name` is blank.
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub builds: Vec<Build>,
    #[serde(default)]
    pub from_tag: Option<String>,
    #[serde(default)]
    pub bugs: Vec<BodhiBug>,
    #[serde(default)]
    pub release: Option<Release>,
    #[serde(default)]
    pub date_submitted: Option<String>,
    #[serde(default)]
    pub date_testing: Option<String>,
    #[serde(default)]
    pub date_stable: Option<String>,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct Build {
    pub nvr: String,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct BodhiBug {
    pub bug_id: u64,
    /// Bug summary as cached by Bodhi from Bugzilla. May be
    /// missing or stale if Bodhi couldn't fetch it.
    #[serde(default)]
    pub title: Option<String>,
}

/// Per-bug karma sent with a comment (the web UI's per-bug
/// thumbs up/down).
#[derive(Debug, Clone, serde::Serialize)]
pub struct BugFeedbackItem {
    pub bug_id: u64,
    /// -1, 0, or +1.
    pub karma: i32,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct Release {
    pub name: String,
    #[serde(default)]
    pub branch: Option<String>,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct SingleUpdateResponse {
    pub update: Update,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct CommentsResponse {
    pub comments: Vec<Comment>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
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

/// A server-side adjustment note returned alongside a write
/// response (e.g. "You may not give karma to your own updates."
/// when Bodhi zeroes the karma instead of rejecting the comment).
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct Caveat {
    #[serde(default)]
    pub name: Option<String>,
    pub description: String,
}

/// Response from posting a comment (`POST /comments/`).
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct SingleCommentResponse {
    pub comment: Comment,
    /// Server-side adjustments, e.g. karma zeroed on own update.
    #[serde(default)]
    pub caveats: Vec<Caveat>,
}

/// Fields for creating an update from a Koji side tag
/// (`POST /updates/` with `from_tag` — the API behind
/// `bodhi updates new --from-tag`).
#[derive(Debug, Clone)]
pub struct NewUpdateFromTag {
    /// The Koji side tag holding the builds.
    pub from_tag: String,
    /// Update notes/description (markdown; shown to users).
    pub notes: String,
    /// `bugfix` | `enhancement` | `security` | `newpackage`.
    pub update_type: String,
    /// `unspecified` | `low` | `medium` | `high` | `urgent`.
    /// Bodhi requires a real severity for `security` updates.
    pub severity: String,
    /// Bug IDs to associate with the update.
    pub bugs: Vec<u64>,
    /// Close the associated bugs when the update goes stable.
    pub close_bugs: bool,
    /// Push automatically at the karma thresholds.
    pub autokarma: bool,
    /// Karma needed to push stable (Bodhi default 3).
    pub stable_karma: i32,
    /// Negative karma at which the update is unpushed (default -3).
    pub unstable_karma: i32,
}

/// Response from creating an update (`POST /updates/`). Bodhi puts
/// the update's fields at the *top level* for the single-update case
/// (which `from_tag` always is) and returns an `updates` list when a
/// request created several; caveats ride along in both shapes.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct NewUpdateResponse {
    /// The new update's alias, in the single-update response shape.
    #[serde(default)]
    pub alias: Option<String>,
    /// The created updates, in the multi-update response shape.
    #[serde(default)]
    pub updates: Vec<Update>,
    /// Server-side adjustment notes.
    #[serde(default)]
    pub caveats: Vec<Caveat>,
}

impl NewUpdateResponse {
    /// Aliases of every update the request created, whichever
    /// response shape Bodhi used.
    pub fn aliases(&self) -> Vec<&str> {
        match &self.alias {
            Some(a) => vec![a.as_str()],
            None => self.updates.iter().map(|u| u.alias.as_str()).collect(),
        }
    }
}

/// A Bodhi release entry from the releases API.
#[derive(Debug, Deserialize)]
#[non_exhaustive]
pub struct BodhiRelease {
    pub name: String,
    pub branch: String,
    pub id_prefix: String,
    pub state: String,
    /// Koji tag stable updates land in (e.g. `f43-updates`,
    /// `f45` for rawhide, `epel10.3`). With tag inheritance this
    /// is the authoritative "what the release carries" tag —
    /// `f43-updates` inherits `f43`, so one inherited query
    /// covers GA and updates content. Defaulted so canned
    /// fixtures without the field keep deserializing.
    #[serde(default)]
    pub stable_tag: String,
}

#[derive(Debug, Deserialize)]
#[non_exhaustive]
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
    fn deserialize_update_with_from_tag() {
        let json = r#"{
            "alias": "FEDORA-EPEL-2026-abc123",
            "status": "testing",
            "from_tag": "epel9-build-side-133287",
            "builds": [
                {"nvr": "rust-uucore-0.0.28-2.el9"}
            ]
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert_eq!(update.from_tag.as_deref(), Some("epel9-build-side-133287"));
    }

    #[test]
    fn deserialize_update_without_from_tag() {
        let json = r#"{
            "alias": "FEDORA-2026-xyz",
            "status": "testing",
            "builds": [{"nvr": "foo-1.0-1.fc42"}]
        }"#;

        let update: Update = serde_json::from_str(json).unwrap();
        assert!(update.from_tag.is_none());
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
                    "state": "current",
                    "stable_tag": "f43-updates"
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
        assert_eq!(resp.releases[0].stable_tag, "f43-updates");
        // Missing stable_tag defaults to empty (older fixtures).
        assert_eq!(resp.releases[1].stable_tag, "");
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
