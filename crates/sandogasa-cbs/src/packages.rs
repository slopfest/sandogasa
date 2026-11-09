// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Which packages a collection's tags list. CBS builds a package into
//! a tag only once it is listed there, with an owner: a first build of
//! a package needs `cbs add-pkg --owner=<user> <tag> <package>` for each
//! of the collection's `-candidate`, `-testing` and `-release` tags
//! first.

use std::process::{Command, Stdio};

use crate::{Collection, PROFILE, Stage};

/// The tags a collection's packages have to be listed in, in the order
/// a build moves through them.
pub fn stage_tags(collection: &Collection) -> [String; 3] {
    [Stage::Candidate, Stage::Testing, Stage::Release].map(|s| collection.tag(s))
}

/// The command that lists `package` in `tag`, as a person would type
/// it.
pub fn add_pkg_argv(owner: &str, tag: &str, package: &str) -> Vec<String> {
    vec![
        "cbs".to_string(),
        "add-pkg".to_string(),
        format!("--owner={owner}"),
        tag.to_string(),
        package.to_string(),
    ]
}

/// The CBS user koji's `moshimoshi` greets: `salimma` from
/// `what's up, salimma!` (the greeting word varies, the name follows
/// the last comma).
pub fn greeted_user(moshimoshi: &str) -> Option<String> {
    let first = moshimoshi.lines().map(str::trim).find(|l| !l.is_empty())?;
    let (_, name) = first.rsplit_once(", ")?;
    let name = name.trim_end_matches('!').trim();
    (!name.is_empty() && !name.contains(' ')).then(|| name.to_string())
}

/// Whether `koji list-pkgs --quiet` output, with its exit status,
/// lists `package`: `None` when it is an error rather than an answer.
pub fn listed_in(output: &str, success: bool, package: &str) -> Option<bool> {
    if success {
        return Some(
            output
                .lines()
                .any(|l| l.split_whitespace().next() == Some(package)),
        );
    }
    output.contains("(no matching packages)").then_some(false)
}

/// The CBS user the login belongs to, as koji's `moshimoshi` greets it.
pub fn whoami() -> Result<String, String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "moshimoshi"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    greeted_user(&text).ok_or_else(|| {
        format!(
            "could not tell the CBS user from `koji --profile {PROFILE} moshimoshi`: {}",
            text.lines().next().unwrap_or("no output")
        )
    })
}

/// Whether `tag` lists `package` (inherited listings count).
pub fn listed(tag: &str, package: &str) -> Result<bool, String> {
    let out = Command::new("koji")
        .args([
            "--profile",
            PROFILE,
            "list-pkgs",
            "--quiet",
            "--tag",
            tag,
            "--package",
            package,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    listed_in(&text, out.status.success(), package).ok_or_else(|| {
        format!(
            "koji list-pkgs --tag {tag} --package {package}: {}",
            text.trim()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cbs_user_is_read_from_the_greeting() {
        let out =
            "what's up, salimma!\n\nYou are using the hub at https://cbs.centos.org/kojihub/\n";
        assert_eq!(greeted_user(out).as_deref(), Some("salimma"));
        assert_eq!(greeted_user("olá, someone!\n").as_deref(), Some("someone"));
        assert_eq!(greeted_user("Hello!\n"), None);
        assert_eq!(greeted_user(""), None);
    }

    #[test]
    fn a_listing_is_told_from_an_absence_and_an_error() {
        let xz = "xz    proposed_updates10s-packages-main-testing    salimma\n";
        assert_eq!(listed_in(xz, true, "xz"), Some(true));
        assert_eq!(listed_in(xz, true, "x"), Some(false));
        assert_eq!(
            listed_in("(no matching packages)\n", false, "rsync"),
            Some(false)
        );
        assert_eq!(
            listed_in("GenericError: no such tag\n", false, "rsync"),
            None
        );
    }

    #[test]
    fn a_collections_stage_tags_and_the_command_that_lists_a_package() {
        let c = crate::Sig::ProposedUpdates
            .collection_for_branch("c10s-proposed-RHEL-303721")
            .unwrap();
        assert_eq!(
            stage_tags(&c),
            [
                "proposed_updates10s-packages-main-candidate",
                "proposed_updates10s-packages-main-testing",
                "proposed_updates10s-packages-main-release",
            ]
        );
        assert_eq!(
            add_pkg_argv(
                "salimma",
                "proposed_updates10s-packages-main-candidate",
                "rsync"
            )
            .join(" "),
            "cbs add-pkg --owner=salimma proposed_updates10s-packages-main-candidate rsync"
        );
    }
}
