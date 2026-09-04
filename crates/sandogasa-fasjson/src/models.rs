// SPDX-License-Identifier: Apache-2.0 OR MIT

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
    /// Red Hat Bugzilla email, if different from FAS email.
    #[serde(default)]
    pub rhbzemail: Option<String>,
    /// IANA timezone the user has set on their FAS profile,
    /// e.g. `Europe/Dublin`. Empty / null for users who haven't
    /// filled it in.
    #[serde(default)]
    pub timezone: Option<String>,
    /// Chat handles as FAS stores them: `matrix:/nick` (fedora.im),
    /// `matrix://homeserver/nick`, `irc:/nick`.
    #[serde(default)]
    pub ircnicks: Vec<String>,
}

impl FasUser {
    /// The user's Matrix IDs (`@nick:homeserver`), from `ircnicks`;
    /// IRC handles are skipped.
    pub fn matrix_ids(&self) -> Vec<String> {
        self.ircnicks
            .iter()
            .filter_map(|n| matrix_id_from_ircnick(n))
            .collect()
    }
}

/// `matrix:/nick` → `@nick:fedora.im`; `matrix://homeserver/nick` →
/// `@nick:homeserver`; anything else (`irc:/nick`) → `None`.
pub fn matrix_id_from_ircnick(nick: &str) -> Option<String> {
    let rest = nick.strip_prefix("matrix:")?;
    if let Some(hosted) = rest.strip_prefix("//") {
        let (server, local) = hosted.split_once('/')?;
        (!server.is_empty() && !local.is_empty()).then(|| format!("@{local}:{server}"))
    } else {
        let local = rest.strip_prefix('/')?;
        (!local.is_empty()).then(|| format!("@{local}:fedora.im"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_ids_come_from_ircnicks_in_both_shapes() {
        for (nick, want) in [
            ("matrix:/salimma", Some("@salimma:fedora.im")),
            (
                "matrix://matrix.org/michel-slm",
                Some("@michel-slm:matrix.org"),
            ),
            (
                "matrix://matrix.scrye.com/nirik",
                Some("@nirik:matrix.scrye.com"),
            ),
            ("irc:/michel-slm", None),
            ("matrix://", None),
            ("matrix:/", None),
        ] {
            assert_eq!(matrix_id_from_ircnick(nick).as_deref(), want, "{nick}");
        }
        let user: FasUser = serde_json::from_str(
            r#"{"username":"salimma","ircnicks":["matrix://matrix.org/michel-slm","matrix:/salimma","irc:/michel-slm"]}"#,
        )
        .unwrap();
        assert_eq!(
            user.matrix_ids(),
            ["@michel-slm:matrix.org", "@salimma:fedora.im"]
        );
    }

    #[test]
    fn deserialize_fas_user() {
        let json = r#"{
            "result": {
                "username": "salimma",
                "human_name": "Michel Lind",
                "emails": ["salimma@fedoraproject.org", "michel@michel-slm.name"],
                "timezone": "Europe/Dublin"
            }
        }"#;

        let resp: FasjsonResponse<FasUser> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.result.username, "salimma");
        assert_eq!(resp.result.human_name.as_deref(), Some("Michel Lind"));
        assert_eq!(resp.result.emails.len(), 2);
        assert_eq!(resp.result.timezone.as_deref(), Some("Europe/Dublin"));
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
