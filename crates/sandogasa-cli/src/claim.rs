// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Whether a tool should claim the work it is closing.
//!
//! Project rule: any tool that closes bugs or tracking issues must also
//! offer to reassign them to the person running the command — triaging is
//! a benefit in itself, and whoever cleans up stale work may want the
//! credit. The decision lives here, in one matrix, so a Bugzilla tool and
//! a GitLab tool cannot drift apart on it:
//!
//! - an explicit claim flag (`--claim`) claims without prompting (the
//!   caller should reject the flag up front when there is no identity to
//!   claim as);
//! - `-y`/`--yes` without the flag does **not** claim — a non-interactive
//!   run must not reassign work nobody asked it to;
//! - no identity skips silently (nothing to assign to);
//! - otherwise the user is asked, via the caller's `confirm` closure, so
//!   terminal I/O stays out of the library crates.
//!
//! What differs per tracker is only *how* the claim is applied: Bugzilla
//! takes an email in `assigned_to`, GitLab takes numeric ids in
//! `assignee_ids`. Those live with their own clients.

/// Decide whether to claim, per the matrix above. Returns the identity to
/// assign to when claiming, `None` otherwise. `confirm` is invoked only
/// when an interactive prompt is actually needed.
pub fn resolve_claim<F>(
    claim_flag: bool,
    yes: bool,
    identity: Option<&str>,
    prompt: &str,
    confirm: F,
) -> Result<Option<String>, String>
where
    F: FnOnce(&str) -> Result<bool, String>,
{
    let identity = match identity {
        Some(i) if !i.is_empty() => i,
        _ => return Ok(None),
    };
    let want = if claim_flag {
        true
    } else if yes {
        false
    } else {
        confirm(prompt)?
    };
    Ok(want.then(|| identity.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_prompt(_: &str) -> Result<bool, String> {
        panic!("confirm must not be called");
    }

    #[test]
    fn the_flag_claims_without_asking() {
        let got = resolve_claim(true, false, Some("me"), "?", no_prompt).unwrap();
        assert_eq!(got.as_deref(), Some("me"));
    }

    #[test]
    fn yes_alone_does_not_claim() {
        // A non-interactive run must not reassign work nobody asked it to.
        let got = resolve_claim(false, true, Some("me"), "?", no_prompt).unwrap();
        assert_eq!(got, None);
    }

    #[test]
    fn no_identity_skips_silently() {
        let got = resolve_claim(false, false, None, "?", no_prompt).unwrap();
        assert_eq!(got, None);
        let empty = resolve_claim(true, false, Some(""), "?", no_prompt).unwrap();
        assert_eq!(empty, None, "an empty identity is no identity");
    }

    #[test]
    fn otherwise_the_user_is_asked() {
        let yes = resolve_claim(false, false, Some("me"), "claim?", |p| {
            assert_eq!(p, "claim?");
            Ok(true)
        })
        .unwrap();
        assert_eq!(yes.as_deref(), Some("me"));
        let no = resolve_claim(false, false, Some("me"), "claim?", |_| Ok(false)).unwrap();
        assert_eq!(no, None);
    }
}
