// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Posting a review back to its Bugzilla review bug: build the update
//! body (comment + `fedora-review` flag + status) deterministically here
//! (unit-tested); `main` does the actual fetch/confirm/PUT via
//! `sandogasa_bugzilla::BzClient`.

use sandogasa_bugzilla::models::Bug;
use serde_json::{Value, json};

/// The current `fedora-review` flag status on a bug, if the flag is set.
pub fn current_review_flag(bug: &Bug) -> Option<&str> {
    bug.flags
        .iter()
        .find(|f| f.name == "fedora-review")
        .map(|f| f.status.as_str())
}

/// The Bugzilla `update` body for posting a review:
/// - **approved** → the digest comment, `fedora-review+`, and status
///   `POST` (ready for import);
/// - **not approved** → the digest comment and, unless the flag is
///   already `?`, `fedora-review?` (review in progress) — no status
///   change.
///
/// `claim` is the reviewer's login to assign the bug to (claiming it);
/// `None` leaves the assignee unchanged (it's already the reviewer).
pub fn update_body(
    digest: &str,
    approved: bool,
    current_flag: Option<&str>,
    claim: Option<&str>,
) -> Value {
    let mut body = json!({ "comment": { "body": digest } });
    let obj = body.as_object_mut().expect("object literal");
    if let Some(assignee) = claim {
        obj.insert("assigned_to".into(), json!(assignee));
    }
    if approved {
        obj.insert("status".into(), json!("POST"));
        obj.insert(
            "flags".into(),
            json!([{ "name": "fedora-review", "status": "+" }]),
        );
    } else if current_flag != Some("?") {
        obj.insert(
            "flags".into(),
            json!([{ "name": "fedora-review", "status": "?" }]),
        );
    }
    body
}

/// A one-line human summary of what the post will change, for the
/// confirmation prompt. `claim` is true when the bug will be reassigned
/// to the reviewer.
pub fn action_summary(approved: bool, current_flag: Option<&str>, claim: bool) -> String {
    let mut parts = vec![
        if approved {
            "post the review as a comment"
        } else {
            "post the current review as a comment"
        }
        .to_string(),
    ];
    if claim {
        parts.push("claim the bug".to_string());
    }
    if approved {
        parts.push("set fedora-review +".to_string());
        parts.push("set status POST".to_string());
    } else if current_flag != Some("?") {
        parts.push("set fedora-review ?".to_string());
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approved_body_sets_flag_status_and_comment() {
        let b = update_body("DIGEST", true, Some("?"), None);
        assert_eq!(b["comment"]["body"], "DIGEST");
        assert_eq!(b["status"], "POST");
        assert_eq!(b["flags"][0]["name"], "fedora-review");
        assert_eq!(b["flags"][0]["status"], "+");
        assert!(b.get("assigned_to").is_none()); // not claiming
    }

    #[test]
    fn unapproved_sets_question_flag_when_not_already_set() {
        let b = update_body("DIGEST", false, None, None);
        assert_eq!(b["comment"]["body"], "DIGEST");
        assert!(b.get("status").is_none()); // no status change
        assert_eq!(b["flags"][0]["status"], "?");
    }

    #[test]
    fn unapproved_leaves_an_existing_question_flag_alone() {
        let b = update_body("DIGEST", false, Some("?"), None);
        assert!(b.get("status").is_none());
        assert!(b.get("flags").is_none()); // already ?, don't touch it
        assert_eq!(b["comment"]["body"], "DIGEST");
    }

    #[test]
    fn claim_sets_the_assignee() {
        let b = update_body("DIGEST", true, Some("?"), Some("me@example.com"));
        assert_eq!(b["assigned_to"], "me@example.com");
        // Claiming composes with the approval changes.
        assert_eq!(b["status"], "POST");
    }

    #[test]
    fn action_summary_reflects_the_state() {
        assert!(action_summary(true, Some("?"), false).contains("status POST"));
        assert!(action_summary(false, None, false).contains("fedora-review ?"));
        assert!(!action_summary(false, Some("?"), false).contains("fedora-review"));
        // Claiming is called out when it will happen.
        assert!(action_summary(true, Some("?"), true).contains("claim the bug"));
        assert!(!action_summary(true, Some("?"), false).contains("claim"));
    }
}
