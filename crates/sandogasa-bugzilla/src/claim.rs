// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla's half of the "claim ownership" mechanics.
//!
//! Project rule: any tool that closes bugs must also offer to reassign
//! them to the person running the command. The decision matrix moved to
//! [`sandogasa_cli::claim`] when GitLab-side tools needed the same rule,
//! and is re-exported here; what stays is Bugzilla-specific — the
//! `assigned_to` body field, and the prompt naming a count of bugs.

/// The standard prompt for claiming bugs that are about to be
/// closed. Callers with a broader reassignment scope (e.g. also
/// touching bugs that stay open) should write their own prompt.
pub fn close_claim_prompt(count: usize, email: &str) -> String {
    format!("Also claim ownership of the {count} bug(s) being closed (assigned_to = {email})?")
}

/// Re-exported from [`sandogasa_cli::claim`], where the matrix now lives
/// so GitLab-side tools share it. Kept at this path because callers use
/// it and the rule is the same one.
pub use sandogasa_cli::claim::resolve_claim;

/// Add `assigned_to` to a Bugzilla update body when claiming.
/// A `None` email leaves the body untouched, so callers can
/// apply the `resolve_claim` result unconditionally.
pub fn apply_claim(body: &mut serde_json::Value, email: Option<&str>) {
    if let Some(email) = email {
        body["assigned_to"] = serde_json::json!(email);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_prompt(_: &str) -> Result<bool, String> {
        panic!("confirm must not be called");
    }

    #[test]
    fn claim_flag_claims_without_prompting() {
        let got = resolve_claim(true, false, Some("me@example.com"), "p", no_prompt).unwrap();
        assert_eq!(got.as_deref(), Some("me@example.com"));
        // The flag wins even under -y.
        let got = resolve_claim(true, true, Some("me@example.com"), "p", no_prompt).unwrap();
        assert_eq!(got.as_deref(), Some("me@example.com"));
    }

    #[test]
    fn yes_without_flag_declines_without_prompting() {
        let got = resolve_claim(false, true, Some("me@example.com"), "p", no_prompt).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn missing_or_empty_email_declines_silently() {
        assert_eq!(
            resolve_claim(true, false, None, "p", no_prompt).unwrap(),
            None
        );
        assert_eq!(
            resolve_claim(false, false, Some(""), "p", no_prompt).unwrap(),
            None
        );
    }

    #[test]
    fn interactive_answer_decides() {
        let got = resolve_claim(false, false, Some("me@example.com"), "p", |prompt| {
            assert_eq!(prompt, "p");
            Ok(true)
        })
        .unwrap();
        assert_eq!(got.as_deref(), Some("me@example.com"));
        let got = resolve_claim(false, false, Some("me@example.com"), "p", |_| Ok(false)).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn prompt_error_propagates() {
        let err = resolve_claim(false, false, Some("me@example.com"), "p", |_| {
            Err("stdin closed".to_string())
        })
        .unwrap_err();
        assert_eq!(err, "stdin closed");
    }

    #[test]
    fn apply_claim_sets_assigned_to_only_when_claiming() {
        let mut body = serde_json::json!({"status": "CLOSED"});
        apply_claim(&mut body, None);
        assert!(body.get("assigned_to").is_none());
        apply_claim(&mut body, Some("me@example.com"));
        assert_eq!(body["assigned_to"], "me@example.com");
    }

    #[test]
    fn close_claim_prompt_mentions_count_and_email() {
        let p = close_claim_prompt(3, "me@example.com");
        assert!(p.contains("3 bug(s)"));
        assert!(p.contains("assigned_to = me@example.com"));
    }
}
