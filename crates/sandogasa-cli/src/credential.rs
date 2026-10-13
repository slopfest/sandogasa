// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Checking a credential, and offering to fix one the service refuses.
//!
//! Every tool that stores an API key validates it at configure time,
//! and each had grown its own flow. Two went wrong in the same way:
//! the check asked something the service would answer anonymously, so
//! it passed for a key that could never write — `ebranch` reported a
//! key pasted three times into a hidden prompt as valid, and only a
//! real attempt to file revealed it. The other half was the remedy: a
//! refused key was saved with a warning and an instruction to rerun.
//!
//! What a check *proves* is each caller's problem, since only it knows
//! what its service gates. What happens next is the same everywhere,
//! and lives here.

use std::future::Future;
use std::io::IsTerminal;
use std::pin::Pin;

/// What a credential check concluded.
pub enum KeyVerdict {
    /// The service took it. The string is what to print.
    Accepted(String),
    /// The service refused it: worth offering to replace.
    Refused(String),
    /// The check could not be made — a network error, a rate limit, a
    /// service that is down. That says nothing about the key, so
    /// nothing is offered and what is there is kept.
    Unknown(String),
}

/// A credential after checking: what to save, and whether the
/// service took it.
///
/// Callers that only store the value read `key`; one that must decide
/// *whether* to store it — Jira's credentials are an email and a
/// token together, and a refused pair means asking for both again —
/// reads `accepted`.
pub struct Checked {
    /// The value to keep, which is the original unless it was
    /// replaced.
    pub key: String,
    /// Whether the last check accepted it. False for a refusal kept
    /// deliberately, and for a check that could not be made.
    pub accepted: bool,
}

/// What to do once a verdict is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Keep the key as it is and stop asking.
    Keep,
    /// Offer to replace it.
    Offer,
}

/// The rule, separated from the terminal it runs on.
///
/// Only a refusal is worth offering to fix, and only where somebody
/// can answer: a prompt with no terminal is not a question, and
/// `confirm` returns its default on an empty line, so asking anyway
/// would turn "nobody is here" into whichever answer the default
/// happens to be.
pub fn next_step(verdict: &KeyVerdict, interactive: bool) -> Next {
    match verdict {
        KeyVerdict::Refused(_) if interactive => Next::Offer,
        _ => Next::Keep,
    }
}

/// Say what is being checked, and flush so the verdict lands on the
/// same line.
fn announce(label: &str, noun: &str) {
    print!("Validating {label} {noun}... ");
    let _ = std::io::Write::flush(&mut std::io::stdout());
}

/// The note a verdict carries.
fn note(verdict: &KeyVerdict) -> &str {
    match verdict {
        KeyVerdict::Accepted(n) | KeyVerdict::Refused(n) | KeyVerdict::Unknown(n) => n,
    }
}

/// Offer to replace a refused credential. `None` keeps what is there.
///
/// Declining is the point of the offer rather than an afterthought: a
/// value edited by hand to watch the check fail must not be replaced
/// by something that assumes a typo, and an empty answer keeps what
/// is there instead of aborting the command.
fn ask_replacement(label: &str, noun: &str) -> Option<String> {
    match crate::confirm(&format!("enter a new {label} {noun}?"), true) {
        Ok(true) => {}
        _ => {
            eprintln!("warning: keeping the {label} {noun} as it is; it will not work.");
            return None;
        }
    }
    match sandogasa_config::prompt_field(label, noun, true, None) {
        Ok(new) if !new.trim().is_empty() => Some(new.trim().to_string()),
        _ => {
            eprintln!("warning: nothing entered; keeping the {label} {noun} as it is.");
            None
        }
    }
}

/// What to print and whether to keep going, given a verdict.
///
/// Shared by the blocking and async loops so they cannot drift: the
/// only thing that differs between them is how the check is called.
fn settle(label: &str, noun: &str, verdict: &KeyVerdict) -> Next {
    println!("{}", note(verdict));
    let step = next_step(verdict, std::io::stdin().is_terminal());
    if warn_kept(verdict, step) {
        eprintln!("warning: the {label} {noun} was kept but will not work.");
    }
    step
}

/// Whether keeping this verdict deserves a warning.
///
/// Only a refusal that is being kept: an accepted credential needs no
/// caveat, and a check that could not be made has not found anything
/// wrong to warn about.
fn warn_kept(verdict: &KeyVerdict, step: Next) -> bool {
    step == Next::Keep && matches!(verdict, KeyVerdict::Refused(_))
}

/// Validate a credential, offering to replace one that is refused.
///
/// `noun` names what it is — "API key", "token" — and appears in
/// every line this prints, because telling somebody their API key is
/// refused when they configured a token is its own small confusion.
pub async fn validated_key<F>(label: &str, noun: &str, key: String, check: F) -> Checked
where
    F: Fn(String) -> Pin<Box<dyn Future<Output = KeyVerdict>>>,
{
    let mut key = key;
    loop {
        announce(label, noun);
        let verdict = check(key.clone()).await;
        if settle(label, noun, &verdict) == Next::Keep {
            return Checked {
                key,
                accepted: matches!(verdict, KeyVerdict::Accepted(_)),
            };
        }
        match ask_replacement(label, noun) {
            Some(new) => key = new,
            None => {
                return Checked {
                    key,
                    accepted: false,
                };
            }
        }
    }
}

/// [`validated_key`] for a check that does not need awaiting.
///
/// Several tools validate a token with a blocking call, and wrapping
/// those in a runtime only to await them would mean each caller
/// carrying one — and panicking where a runtime already exists.
pub fn validated_key_blocking<F>(label: &str, noun: &str, key: String, check: F) -> Checked
where
    F: Fn(&str) -> KeyVerdict,
{
    let mut key = key;
    loop {
        announce(label, noun);
        let verdict = check(&key);
        if settle(label, noun, &verdict) == Next::Keep {
            return Checked {
                key,
                accepted: matches!(verdict, KeyVerdict::Accepted(_)),
            };
        }
        match ask_replacement(label, noun) {
            Some(new) => key = new,
            None => {
                return Checked {
                    key,
                    accepted: false,
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_refusal_is_worth_offering_to_fix() {
        let note = || "x".to_string();
        // A rate limit or an unreachable service says nothing about
        // the key, so replacing it would be guesswork.
        assert_eq!(next_step(&KeyVerdict::Unknown(note()), true), Next::Keep);
        assert_eq!(next_step(&KeyVerdict::Accepted(note()), true), Next::Keep);
        assert_eq!(next_step(&KeyVerdict::Refused(note()), true), Next::Offer);
    }

    #[test]
    fn only_a_kept_refusal_is_warned_about() {
        let note = || "x".to_string();
        // Kept because nobody could be asked: say so, since the thing
        // was saved and will not work.
        assert!(warn_kept(&KeyVerdict::Refused(note()), Next::Keep));
        // About to be offered a replacement: the warning would be
        // premature, and wrong if they enter a good one.
        assert!(!warn_kept(&KeyVerdict::Refused(note()), Next::Offer));
        // Nothing is known to be wrong with these.
        assert!(!warn_kept(&KeyVerdict::Unknown(note()), Next::Keep));
        assert!(!warn_kept(&KeyVerdict::Accepted(note()), Next::Keep));
    }

    #[test]
    fn the_note_is_whatever_the_verdict_carries() {
        // Each verdict's string is what the caller wrote for it, so a
        // service can explain itself in its own words.
        assert_eq!(note(&KeyVerdict::Accepted("valid.".into())), "valid.");
        assert_eq!(note(&KeyVerdict::Refused("rejected.".into())), "rejected.");
        assert_eq!(note(&KeyVerdict::Unknown("offline.".into())), "offline.");
    }

    #[test]
    fn nothing_is_offered_where_nobody_can_answer() {
        // `confirm` returns its default on an empty line, so asking
        // without a terminal turns "nobody is here" into whichever
        // answer the default happens to be.
        assert_eq!(
            next_step(&KeyVerdict::Refused("x".to_string()), false),
            Next::Keep
        );
    }
}
