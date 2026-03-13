// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

/// ACL levels for a dist-git project.
#[derive(Debug, Clone, Deserialize)]
pub struct ProjectAcls {
    pub access_users: AccessUsers,
    pub access_groups: AccessGroups,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccessUsers {
    #[serde(default)]
    pub owner: Vec<String>,
    #[serde(default)]
    pub admin: Vec<String>,
    #[serde(default)]
    pub commit: Vec<String>,
    #[serde(default)]
    pub collaborator: Vec<String>,
    #[serde(default)]
    pub ticket: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccessGroups {
    #[serde(default)]
    pub admin: Vec<String>,
    #[serde(default)]
    pub commit: Vec<String>,
    #[serde(default)]
    pub collaborator: Vec<String>,
    #[serde(default)]
    pub ticket: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_project_acls_full() {
        let json = r#"{
            "access_users": {
                "owner": ["ngompa"],
                "admin": ["salimma"],
                "commit": ["dcavalca"],
                "collaborator": [],
                "ticket": ["testuser"]
            },
            "access_groups": {
                "admin": [],
                "commit": ["kde-sig"],
                "collaborator": [],
                "ticket": []
            }
        }"#;
        let acls: ProjectAcls = serde_json::from_str(json).unwrap();
        assert_eq!(acls.access_users.owner, vec!["ngompa"]);
        assert_eq!(acls.access_users.admin, vec!["salimma"]);
        assert_eq!(acls.access_users.commit, vec!["dcavalca"]);
        assert!(acls.access_users.collaborator.is_empty());
        assert_eq!(acls.access_users.ticket, vec!["testuser"]);
        assert!(acls.access_groups.admin.is_empty());
        assert_eq!(acls.access_groups.commit, vec!["kde-sig"]);
    }

    #[test]
    fn deserialize_project_acls_missing_fields_default_to_empty() {
        let json = r#"{
            "access_users": {
                "owner": ["ngompa"]
            },
            "access_groups": {}
        }"#;
        let acls: ProjectAcls = serde_json::from_str(json).unwrap();
        assert_eq!(acls.access_users.owner, vec!["ngompa"]);
        assert!(acls.access_users.admin.is_empty());
        assert!(acls.access_users.commit.is_empty());
        assert!(acls.access_users.collaborator.is_empty());
        assert!(acls.access_users.ticket.is_empty());
        assert!(acls.access_groups.admin.is_empty());
        assert!(acls.access_groups.commit.is_empty());
    }

    #[test]
    fn deserialize_project_acls_from_full_api_response() {
        // The real API returns many more fields; we only deserialize what we need.
        let json = r#"{
            "access_groups": {
                "admin": [],
                "collaborator": [],
                "commit": ["kde-sig"],
                "ticket": []
            },
            "access_users": {
                "admin": [],
                "collaborator": [],
                "commit": [],
                "owner": ["ngompa"],
                "ticket": []
            },
            "close_status": [],
            "full_url": "https://src.fedoraproject.org/rpms/freerdp",
            "id": 12345,
            "name": "freerdp",
            "namespace": "rpms"
        }"#;
        let acls: ProjectAcls = serde_json::from_str(json).unwrap();
        assert_eq!(acls.access_users.owner, vec!["ngompa"]);
        assert_eq!(acls.access_groups.commit, vec!["kde-sig"]);
    }

    #[test]
    fn deserialize_project_acls_multiple_users() {
        let json = r#"{
            "access_users": {
                "owner": ["ngompa"],
                "admin": ["salimma", "dcavalca"],
                "commit": ["user1", "user2", "user3"],
                "collaborator": [],
                "ticket": []
            },
            "access_groups": {
                "admin": [],
                "commit": [],
                "collaborator": [],
                "ticket": []
            }
        }"#;
        let acls: ProjectAcls = serde_json::from_str(json).unwrap();
        assert_eq!(acls.access_users.admin, vec!["salimma", "dcavalca"]);
        assert_eq!(acls.access_users.commit, vec!["user1", "user2", "user3"]);
    }
}
