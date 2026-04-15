// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde::{Deserialize, Serialize};

/// Access level tiers, ordered from lowest to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessLevel {
    Ticket,
    Collaborator,
    Commit,
    Admin,
    Owner,
}

impl std::str::FromStr for AccessLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ticket" => Ok(AccessLevel::Ticket),
            "collaborator" => Ok(AccessLevel::Collaborator),
            "commit" => Ok(AccessLevel::Commit),
            "admin" => Ok(AccessLevel::Admin),
            "owner" => Ok(AccessLevel::Owner),
            _ => Err(format!("invalid access level: {s}")),
        }
    }
}

impl std::fmt::Display for AccessLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AccessLevel::Ticket => write!(f, "ticket"),
            AccessLevel::Collaborator => write!(f, "collaborator"),
            AccessLevel::Commit => write!(f, "commit"),
            AccessLevel::Admin => write!(f, "admin"),
            AccessLevel::Owner => write!(f, "owner"),
        }
    }
}

/// Result of an access level check.
#[derive(Debug, Clone)]
pub enum AccessResult {
    /// User has sufficient access directly.
    Direct(AccessLevel),
    /// User has sufficient access via group membership.
    ViaGroup { level: AccessLevel, group: String },
    /// User does not have sufficient access.
    Insufficient { level: Option<AccessLevel> },
}

impl AccessResult {
    /// Returns true if the access check passed.
    pub fn is_sufficient(&self) -> bool {
        !matches!(self, AccessResult::Insufficient { .. })
    }
}

/// ACL levels for a dist-git project.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectAcls {
    pub access_users: AccessUsers,
    pub access_groups: AccessGroups,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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

impl ProjectAcls {
    /// Return the highest direct access level for a user.
    pub fn user_level(&self, username: &str) -> Option<AccessLevel> {
        if self.access_users.owner.iter().any(|u| u == username) {
            return Some(AccessLevel::Owner);
        }
        if self.access_users.admin.iter().any(|u| u == username) {
            return Some(AccessLevel::Admin);
        }
        if self.access_users.commit.iter().any(|u| u == username) {
            return Some(AccessLevel::Commit);
        }
        if self.access_users.collaborator.iter().any(|u| u == username) {
            return Some(AccessLevel::Collaborator);
        }
        if self.access_users.ticket.iter().any(|u| u == username) {
            return Some(AccessLevel::Ticket);
        }
        None
    }

    /// Return the highest access level for a group.
    pub fn group_level(&self, group_name: &str) -> Option<AccessLevel> {
        if self.access_groups.admin.iter().any(|g| g == group_name) {
            return Some(AccessLevel::Admin);
        }
        if self.access_groups.commit.iter().any(|g| g == group_name) {
            return Some(AccessLevel::Commit);
        }
        if self
            .access_groups
            .collaborator
            .iter()
            .any(|g| g == group_name)
        {
            return Some(AccessLevel::Collaborator);
        }
        if self.access_groups.ticket.iter().any(|g| g == group_name) {
            return Some(AccessLevel::Ticket);
        }
        None
    }

    /// Return groups that have access at or above `min_level`.
    pub fn groups_with_level(&self, min_level: AccessLevel) -> Vec<(&str, AccessLevel)> {
        let mut result = Vec::new();
        if min_level <= AccessLevel::Admin {
            for g in &self.access_groups.admin {
                result.push((g.as_str(), AccessLevel::Admin));
            }
        }
        if min_level <= AccessLevel::Commit {
            for g in &self.access_groups.commit {
                result.push((g.as_str(), AccessLevel::Commit));
            }
        }
        if min_level <= AccessLevel::Collaborator {
            for g in &self.access_groups.collaborator {
                result.push((g.as_str(), AccessLevel::Collaborator));
            }
        }
        if min_level <= AccessLevel::Ticket {
            for g in &self.access_groups.ticket {
                result.push((g.as_str(), AccessLevel::Ticket));
            }
        }
        result
    }
}

impl AccessGroups {
    /// Check whether a group has any level of access.
    pub fn contains_group(&self, group: &str) -> bool {
        self.admin.iter().any(|g| g == group)
            || self.commit.iter().any(|g| g == group)
            || self.collaborator.iter().any(|g| g == group)
            || self.ticket.iter().any(|g| g == group)
    }
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

    fn make_acls(
        owners: Vec<&str>,
        admins: Vec<&str>,
        commits: Vec<&str>,
        collabs: Vec<&str>,
        tickets: Vec<&str>,
    ) -> ProjectAcls {
        ProjectAcls {
            access_users: AccessUsers {
                owner: owners.into_iter().map(String::from).collect(),
                admin: admins.into_iter().map(String::from).collect(),
                commit: commits.into_iter().map(String::from).collect(),
                collaborator: collabs.into_iter().map(String::from).collect(),
                ticket: tickets.into_iter().map(String::from).collect(),
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
        }
    }

    // ---- AccessLevel ----

    #[test]
    fn access_level_ordering() {
        assert!(AccessLevel::Ticket < AccessLevel::Collaborator);
        assert!(AccessLevel::Collaborator < AccessLevel::Commit);
        assert!(AccessLevel::Commit < AccessLevel::Admin);
        assert!(AccessLevel::Admin < AccessLevel::Owner);
    }

    #[test]
    fn access_level_display() {
        assert_eq!(AccessLevel::Ticket.to_string(), "ticket");
        assert_eq!(AccessLevel::Collaborator.to_string(), "collaborator");
        assert_eq!(AccessLevel::Commit.to_string(), "commit");
        assert_eq!(AccessLevel::Admin.to_string(), "admin");
        assert_eq!(AccessLevel::Owner.to_string(), "owner");
    }

    #[test]
    fn access_level_serde_round_trip() {
        let json = serde_json::to_string(&AccessLevel::Admin).unwrap();
        assert_eq!(json, "\"admin\"");
        let parsed: AccessLevel = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AccessLevel::Admin);
    }

    // ---- ProjectAcls::user_level ----

    #[test]
    fn user_level_returns_owner() {
        let acls = make_acls(vec!["ngompa"], vec![], vec![], vec![], vec![]);
        assert_eq!(acls.user_level("ngompa"), Some(AccessLevel::Owner));
    }

    #[test]
    fn user_level_returns_admin() {
        let acls = make_acls(vec![], vec!["salimma"], vec![], vec![], vec![]);
        assert_eq!(acls.user_level("salimma"), Some(AccessLevel::Admin));
    }

    #[test]
    fn user_level_returns_commit() {
        let acls = make_acls(vec![], vec![], vec!["dcavalca"], vec![], vec![]);
        assert_eq!(acls.user_level("dcavalca"), Some(AccessLevel::Commit));
    }

    #[test]
    fn user_level_returns_collaborator() {
        let acls = make_acls(vec![], vec![], vec![], vec!["testuser"], vec![]);
        assert_eq!(acls.user_level("testuser"), Some(AccessLevel::Collaborator));
    }

    #[test]
    fn user_level_returns_ticket() {
        let acls = make_acls(vec![], vec![], vec![], vec![], vec!["viewer"]);
        assert_eq!(acls.user_level("viewer"), Some(AccessLevel::Ticket));
    }

    #[test]
    fn user_level_returns_none_for_unknown() {
        let acls = make_acls(vec!["ngompa"], vec![], vec![], vec![], vec![]);
        assert_eq!(acls.user_level("unknown"), None);
    }

    #[test]
    fn user_level_returns_highest_level() {
        let acls = make_acls(vec![], vec!["user1"], vec!["user1"], vec![], vec![]);
        assert_eq!(acls.user_level("user1"), Some(AccessLevel::Admin));
    }

    // ---- ProjectAcls::groups_with_level ----

    #[test]
    fn groups_with_level_admin_returns_only_admin_groups() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["sig-a".to_string()],
                commit: vec!["sig-b".to_string()],
                collaborator: vec![],
                ticket: vec!["sig-c".to_string()],
            },
        };
        let groups = acls.groups_with_level(AccessLevel::Admin);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0], ("sig-a", AccessLevel::Admin));
    }

    #[test]
    fn groups_with_level_commit_returns_admin_and_commit() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["sig-a".to_string()],
                commit: vec!["sig-b".to_string()],
                collaborator: vec![],
                ticket: vec!["sig-c".to_string()],
            },
        };
        let groups = acls.groups_with_level(AccessLevel::Commit);
        assert_eq!(groups.len(), 2);
        assert!(groups.contains(&("sig-a", AccessLevel::Admin)));
        assert!(groups.contains(&("sig-b", AccessLevel::Commit)));
    }

    #[test]
    fn groups_with_level_ticket_returns_all_groups() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["sig-a".to_string()],
                commit: vec!["sig-b".to_string()],
                collaborator: vec!["sig-d".to_string()],
                ticket: vec!["sig-c".to_string()],
            },
        };
        let groups = acls.groups_with_level(AccessLevel::Ticket);
        assert_eq!(groups.len(), 4);
    }

    #[test]
    fn groups_with_level_empty_when_no_groups() {
        let acls = make_acls(vec!["ngompa"], vec![], vec![], vec![], vec![]);
        let groups = acls.groups_with_level(AccessLevel::Ticket);
        assert!(groups.is_empty());
    }

    // ---- AccessLevel::from_str ----

    #[test]
    fn access_level_from_str_valid() {
        assert_eq!(
            "ticket".parse::<AccessLevel>().unwrap(),
            AccessLevel::Ticket
        );
        assert_eq!(
            "collaborator".parse::<AccessLevel>().unwrap(),
            AccessLevel::Collaborator
        );
        assert_eq!(
            "commit".parse::<AccessLevel>().unwrap(),
            AccessLevel::Commit
        );
        assert_eq!("admin".parse::<AccessLevel>().unwrap(), AccessLevel::Admin);
        assert_eq!("owner".parse::<AccessLevel>().unwrap(), AccessLevel::Owner);
    }

    #[test]
    fn access_level_from_str_invalid() {
        assert!("superadmin".parse::<AccessLevel>().is_err());
        assert!("".parse::<AccessLevel>().is_err());
    }

    // ---- ProjectAcls::group_level ----

    #[test]
    fn group_level_returns_admin() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec!["kde-sig".to_string()],
                commit: vec!["python-sig".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
        };
        assert_eq!(acls.group_level("kde-sig"), Some(AccessLevel::Admin));
    }

    #[test]
    fn group_level_returns_commit() {
        let acls = ProjectAcls {
            access_users: AccessUsers {
                owner: vec![],
                admin: vec![],
                commit: vec![],
                collaborator: vec![],
                ticket: vec![],
            },
            access_groups: AccessGroups {
                admin: vec![],
                commit: vec!["python-sig".to_string()],
                collaborator: vec![],
                ticket: vec![],
            },
        };
        assert_eq!(acls.group_level("python-sig"), Some(AccessLevel::Commit));
    }

    #[test]
    fn group_level_returns_none_for_unknown() {
        let acls = make_acls(vec![], vec![], vec![], vec![], vec![]);
        assert_eq!(acls.group_level("nonexistent"), None);
    }

    // ---- AccessResult ----

    #[test]
    fn access_result_direct_is_sufficient() {
        let result = AccessResult::Direct(AccessLevel::Admin);
        assert!(result.is_sufficient());
    }

    #[test]
    fn access_result_via_group_is_sufficient() {
        let result = AccessResult::ViaGroup {
            level: AccessLevel::Admin,
            group: "kde-sig".to_string(),
        };
        assert!(result.is_sufficient());
    }

    #[test]
    fn access_result_insufficient_is_not_sufficient() {
        let result = AccessResult::Insufficient {
            level: Some(AccessLevel::Commit),
        };
        assert!(!result.is_sufficient());
    }

    #[test]
    fn access_result_insufficient_none_is_not_sufficient() {
        let result = AccessResult::Insufficient { level: None };
        assert!(!result.is_sufficient());
    }

    // ---- Deserialization ----

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

    // ---- AccessGroups::contains_group ----

    fn make_groups(
        admin: Vec<&str>,
        commit: Vec<&str>,
        collaborator: Vec<&str>,
        ticket: Vec<&str>,
    ) -> AccessGroups {
        AccessGroups {
            admin: admin.into_iter().map(String::from).collect(),
            commit: commit.into_iter().map(String::from).collect(),
            collaborator: collaborator.into_iter().map(String::from).collect(),
            ticket: ticket.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn contains_group_in_admin() {
        let groups = make_groups(vec!["rust-sig"], vec![], vec![], vec![]);
        assert!(groups.contains_group("rust-sig"));
    }

    #[test]
    fn contains_group_in_commit() {
        let groups = make_groups(vec![], vec!["python-sig"], vec![], vec![]);
        assert!(groups.contains_group("python-sig"));
    }

    #[test]
    fn contains_group_in_collaborator() {
        let groups = make_groups(vec![], vec![], vec!["kde-sig"], vec![]);
        assert!(groups.contains_group("kde-sig"));
    }

    #[test]
    fn contains_group_in_ticket() {
        let groups = make_groups(vec![], vec![], vec![], vec!["epel-sig"]);
        assert!(groups.contains_group("epel-sig"));
    }

    #[test]
    fn contains_group_not_present() {
        let groups = make_groups(vec!["rust-sig"], vec!["python-sig"], vec![], vec![]);
        assert!(!groups.contains_group("kde-sig"));
    }

    #[test]
    fn contains_group_empty() {
        let groups = make_groups(vec![], vec![], vec![], vec![]);
        assert!(!groups.contains_group("rust-sig"));
    }
}
