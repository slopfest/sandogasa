// SPDX-License-Identifier: MPL-2.0

use serde::Deserialize;

/// FASJSON API response wrapper.
#[derive(Debug, Deserialize)]
pub struct FasjsonResponse<T> {
    pub result: T,
}

/// A Fedora Account System user profile.
#[derive(Debug, Deserialize)]
pub struct FasUser {
    pub username: String,
    #[serde(default)]
    pub human_name: Option<String>,
    #[serde(default)]
    pub emails: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_fas_user() {
        let json = r#"{
            "result": {
                "username": "salimma",
                "human_name": "Michel Lind",
                "emails": ["salimma@fedoraproject.org", "michel@michel-slm.name"]
            }
        }"#;

        let resp: FasjsonResponse<FasUser> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.username, "salimma");
        assert_eq!(resp.result.human_name.as_deref(), Some("Michel Lind"));
        assert_eq!(resp.result.emails.len(), 2);
    }

    #[test]
    fn deserialize_fas_user_minimal() {
        let json = r#"{
            "result": {
                "username": "newuser"
            }
        }"#;

        let resp: FasjsonResponse<FasUser> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.username, "newuser");
        assert!(resp.result.emails.is_empty());
        assert!(resp.result.human_name.is_none());
    }
}
