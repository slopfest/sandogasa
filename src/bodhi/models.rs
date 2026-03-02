// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct UpdatesResponse {
    pub updates: Vec<Update>,
    pub total: u64,
    pub page: u64,
    pub pages: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Update {
    pub alias: String,
    pub status: String,
    #[serde(default)]
    pub builds: Vec<Build>,
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

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct BodhiBug {
    pub bug_id: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Release {
    pub name: String,
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
}
