// SPDX-License-Identifier: Apache-2.0 OR MIT

use serde::Deserialize;

/// Paginated response from HyperKitty.
#[derive(Debug, Deserialize)]
pub struct PaginatedResponse<T> {
    pub count: u64,
    #[serde(default)]
    pub next: Option<String>,
    #[serde(default)]
    pub previous: Option<String>,
    pub results: Vec<T>,
}

/// An email in the HyperKitty archive.
#[derive(Debug, Deserialize)]
pub struct Email {
    #[serde(default)]
    pub message_id_hash: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub sender: Option<Sender>,
    #[serde(default)]
    pub mailinglist: Option<String>,
}

/// A sender reference embedded in an email.
#[derive(Debug, Deserialize)]
pub struct Sender {
    /// Obfuscated email address, e.g. "user (a) domain.com".
    pub address: String,
    pub mailman_id: String,
}

/// Obfuscate an email address the way HyperKitty does.
///
/// Converts `user@domain.com` to `user (a) domain.com`.
pub fn obfuscate_email(email: &str) -> String {
    email.replace('@', " (a) ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscate_email_simple() {
        assert_eq!(obfuscate_email("user@example.com"), "user (a) example.com");
    }

    #[test]
    fn obfuscate_email_no_at() {
        assert_eq!(obfuscate_email("noatsign"), "noatsign");
    }

    #[test]
    fn deserialize_email() {
        let json = r#"{
            "message_id_hash": "ABC123",
            "subject": "Test subject",
            "date": "2026-03-23T12:00:00+00:00",
            "sender_name": "Alice",
            "sender": {
                "address": "alice (a) example.com",
                "mailman_id": "abc123"
            },
            "mailinglist": "https://example.com/api/list/test@example.com/"
        }"#;

        let email: Email = serde_json::from_str(json).unwrap();
        assert_eq!(email.subject, "Test subject");
        let sender = email.sender.unwrap();
        assert_eq!(sender.address, "alice (a) example.com");
        assert_eq!(sender.mailman_id, "abc123");
    }

    #[test]
    fn deserialize_paginated_response() {
        let json = r#"{
            "count": 1,
            "next": null,
            "previous": null,
            "results": [
                {
                    "message_id_hash": "ABC123",
                    "subject": "Test",
                    "sender": {
                        "address": "alice (a) example.com",
                        "mailman_id": "abc123"
                    }
                }
            ]
        }"#;

        let resp: PaginatedResponse<Email> = serde_json::from_str(json).unwrap();
        assert_eq!(resp.count, 1);
        assert_eq!(resp.results.len(), 1);
    }
}
