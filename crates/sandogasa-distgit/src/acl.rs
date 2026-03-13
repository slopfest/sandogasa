// SPDX-License-Identifier: MPL-2.0

use serde::{Deserialize, Serialize};

/// ACL levels for a dist-git project.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectAcls {
    pub access_users: AccessUsers,
    pub access_groups: AccessGroups,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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

/// Contributors response from `/api/0/rpms/<package>/contributors`.
///
/// Unlike the project endpoint, this includes branch patterns for
/// collaborators (e.g. `"epel*"`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Contributors {
    pub users: ContributorLevels,
    pub groups: ContributorLevels,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContributorLevels {
    #[serde(default)]
    pub admin: Vec<String>,
    #[serde(default)]
    pub commit: Vec<String>,
    #[serde(default)]
    pub collaborators: Vec<Collaborator>,
    #[serde(default)]
    pub ticket: Vec<String>,
}

/// A collaborator with optional branch restriction.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Collaborator {
    WithBranches { user: String, branches: String },
    Plain(String),
}

impl Collaborator {
    pub fn name(&self) -> &str {
        match self {
            Collaborator::WithBranches { user, .. } => user,
            Collaborator::Plain(name) => name,
        }
    }

    pub fn branches(&self) -> Option<&str> {
        match self {
            Collaborator::WithBranches { branches, .. } => Some(branches),
            Collaborator::Plain(_) => None,
        }
    }
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
    fn serialize_round_trip() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec!["ngompa".to_string()],
                admin: vec!["salimma".to_string()],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec!["kde-sig".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
        };
        let json = serde_json::to_string(&acls).unwrap();
        let parsed: ProjectAcls = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.access_users.owner, vec!["ngompa"]);
        assert_eq!(parsed.access_users.admin, vec!["salimma"]);
        assert_eq!(parsed.access_groups.commit, vec!["kde-sig"]);
    }

    #[test]
    fn deserialize_contributors_with_collaborator_branches() {
        let json = r#"{
            "groups": {
                "admin": [],
                "collaborators": [
                    {"branches": "epel*", "user": "epel-packagers-sig"}
                ],
                "commit": [],
                "ticket": []
            },
            "users": {
                "admin": ["abompard", "orion"],
                "collaborators": [],
                "commit": [],
                "ticket": []
            }
        }"#;
        let contribs: Contributors = serde_json::from_str(json).unwrap();
        assert_eq!(contribs.users.admin, vec!["abompard", "orion"]);
        assert!(contribs.users.collaborators.is_empty());
        assert_eq!(contribs.groups.collaborators.len(), 1);
        assert_eq!(
            contribs.groups.collaborators[0].name(),
            "epel-packagers-sig"
        );
        assert_eq!(contribs.groups.collaborators[0].branches(), Some("epel*"));
    }

    #[test]
    fn deserialize_contributors_no_collaborators() {
        let json = r#"{
            "groups": {
                "admin": [],
                "collaborators": [],
                "commit": ["kde-sig"],
                "ticket": []
            },
            "users": {
                "admin": [],
                "collaborators": [],
                "commit": ["dcavalca"],
                "ticket": []
            }
        }"#;
        let contribs: Contributors = serde_json::from_str(json).unwrap();
        assert_eq!(contribs.groups.commit, vec!["kde-sig"]);
        assert_eq!(contribs.users.commit, vec!["dcavalca"]);
        assert!(contribs.groups.collaborators.is_empty());
    }

    #[test]
    fn deserialize_contributors_user_collaborator_with_branches() {
        let json = r#"{
            "groups": {
                "admin": [],
                "collaborators": [],
                "commit": [],
                "ticket": []
            },
            "users": {
                "admin": [],
                "collaborators": [
                    {"branches": "f4*", "user": "testuser"}
                ],
                "commit": [],
                "ticket": []
            }
        }"#;
        let contribs: Contributors = serde_json::from_str(json).unwrap();
        assert_eq!(contribs.users.collaborators.len(), 1);
        assert_eq!(contribs.users.collaborators[0].name(), "testuser");
        assert_eq!(contribs.users.collaborators[0].branches(), Some("f4*"));
    }

    #[test]
    fn collaborator_serialize_round_trip() {
        let collab = Collaborator::WithBranches {
            user: "test".to_string(),
            branches: "epel*".to_string(),
        };
        let json = serde_json::to_string(&collab).unwrap();
        let parsed: Collaborator = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name(), "test");
        assert_eq!(parsed.branches(), Some("epel*"));
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
