// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The plan a confirmation asks about (`Start build?`, `Promote?`,
//! `Untag?`): what will run and what will be posted, styled by the
//! workspace's color chart. Headings are bold, commands bold green as
//! `--explain` shows them just before running, and the tracking-issue
//! comments as `sandogasa_cbs::notify::describe_styled` renders them.
//! The plan goes to standard error with its question, colored on a
//! terminal only.

use crate::depcheck::BuildCheck;
use sandogasa_cbs::Stage;
use sandogasa_cbs::notify::{Notice, describe_styled};
use sandogasa_cbs::tags::TagChange;
use sandogasa_cli::explain::command_line;
use sandogasa_cli::style::{BOLD, GREEN, YELLOW, paint};

/// The style of a command to run: bold green.
pub fn command_style() -> String {
    format!("{BOLD}{GREEN}")
}

/// A heading of a plan: bold.
pub fn heading(text: &str, color: bool) -> String {
    format!("{}\n", paint(text, BOLD, color))
}

/// A command the plan runs once confirmed, indented.
pub fn command(argv: &[String], color: bool) -> String {
    format!(
        "  {}\n",
        paint(&command_line(argv), &command_style(), color)
    )
}

/// The tracking-issue comments `notices` will post (or, without a token
/// to post with, will not).
fn comments(notices: &[Notice], postable: bool, color: bool) -> String {
    notices
        .iter()
        .map(|n| describe_styled(n, postable, color))
        .collect()
}

/// What `cbspkg build` will start: under `title`, each build's command
/// (and the watch, when there is one), then the comments.
pub fn build(
    title: &str,
    commands: &[Vec<String>],
    notices: &[Notice],
    postable: bool,
    color: bool,
) -> String {
    let mut out = heading(title, color);
    for argv in commands {
        out.push_str(&command(argv, color));
    }
    out.push_str(&comments(notices, postable, color));
    out
}

/// What `found` says, per build, as the shared report shows it on a
/// terminal ([`sandogasa_depcheck::render::to_terminal`]): what the
/// build was compared with and where, what it drops and what that
/// breaks, and its version changes. `unchecked` is why nothing could be
/// checked at all, when that is so.
pub fn checks(found: &[BuildCheck], unchecked: Option<&str>, color: bool) -> String {
    let mut out = String::new();
    if let Some(why) = unchecked {
        out.push_str(&format!(
            "{}\n",
            paint(&format!("Not checked for breakage: {why}"), YELLOW, color)
        ));
    }
    // A blank line between checks; the plan spaces what follows.
    for (i, c) in found.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(&sandogasa_depcheck::render::to_terminal(
            &c.markdown(),
            color,
        ));
    }
    out
}

/// What `cbspkg promote` will do: what the checks of builds going to
/// `-testing` found, each build's tag changes under a heading naming
/// where it goes, the comments, then the older builds it offers to
/// untag, asked separately.
pub fn promote(
    steps: &[(String, Stage, Vec<TagChange>)],
    found: &[BuildCheck],
    unchecked: Option<&str>,
    notices: &[Notice],
    replaced: &[TagChange],
    postable: bool,
    color: bool,
) -> String {
    let mut out = checks(found, unchecked, color);
    for (nvr, to, changes) in steps {
        out.push_str(&heading(
            &format!("Promote {nvr} to -{}:", to.as_str()),
            color,
        ));
        for change in changes {
            out.push_str(&command(&change.argv(), color));
        }
    }
    out.push_str(&comments(notices, postable, color));
    if !replaced.is_empty() {
        out.push_str(&heading(
            "Then, asked separately, the older builds they replace:",
            color,
        ));
        for change in replaced {
            out.push_str(&command(&change.argv(), color));
        }
    }
    out
}

/// What `cbspkg untag` will do: the untags under a heading naming the
/// build, then the comments.
pub fn untag(
    nvr: &str,
    changes: &[TagChange],
    notices: &[Notice],
    postable: bool,
    color: bool,
) -> String {
    let mut out = heading(&format!("Untag {nvr}:"), color);
    for change in changes {
        out.push_str(&command(&change.argv(), color));
    }
    out.push_str(&comments(notices, postable, color));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    use sandogasa_cli::style::RESET;

    #[test]
    fn a_plan_has_bold_headings_and_green_commands() {
        let nvr = "r-1-1.hs.el10";
        let steps = [(
            nvr.to_string(),
            Stage::Testing,
            vec![TagChange::tag("hyperscale10s-packages-main-testing", nvr)],
        )];
        let replaced = [TagChange::untag(
            "hyperscale10s-packages-main-testing",
            "r-0-1.hs.el10",
        )];
        assert_eq!(
            promote(&steps, &[], None, &[], &replaced, true, false),
            "Promote r-1-1.hs.el10 to -testing:\n  \
             cbs tag-build hyperscale10s-packages-main-testing r-1-1.hs.el10\n\
             Then, asked separately, the older builds they replace:\n  \
             cbs untag-build hyperscale10s-packages-main-testing r-0-1.hs.el10\n"
        );
        let colored = promote(&steps, &[], None, &[], &[], true, true);
        assert!(colored.starts_with(&format!(
            "{BOLD}Promote r-1-1.hs.el10 to -testing:{RESET}\n"
        )));
        assert!(
            colored.contains(&format!("  {BOLD}{GREEN}cbs tag-build")),
            "{colored}"
        );
        assert_eq!(
            untag(nvr, &[TagChange::untag("t-testing", nvr)], &[], true, false),
            "Untag r-1-1.hs.el10:\n  cbs untag-build t-testing r-1-1.hs.el10\n"
        );
        assert_eq!(
            build(
                "r main at 0123:",
                &[vec![
                    "cbs".into(),
                    "build".into(),
                    "t".into(),
                    "git+x".into()
                ]],
                &[],
                true,
                false
            ),
            "r main at 0123:\n  cbs build t git+x\n"
        );
    }

    #[test]
    fn a_check_shows_as_the_shared_report_in_color() {
        use crate::depcheck::BuildCheck;
        use sandogasa_cli::style::RED;
        use sandogasa_depcheck::analysis::Analysis;
        use sandogasa_depcheck::report::{BreakKind, BrokenRequires, ChangedProvide, RevDepResult};
        let c = BuildCheck {
            nvr: "r-2-1".into(),
            package: "r".into(),
            against: Some("r-1-1".into()),
            looked_in: "CentOS Stream 10, EPEL 10 and r-release".into(),
            changed: vec![ChangedProvide {
                old: "libr.so.1()(64bit)".into(),
                new: None,
            }],
            result: Ok(Analysis {
                installability_issues: vec![],
                reverse_deps: std::collections::BTreeMap::from([(
                    "user".to_string(),
                    RevDepResult {
                        status: "broken".into(),
                        issues: vec![BrokenRequires {
                            dep: "libr.so.1()(64bit)".into(),
                            changed_provide: "libr.so.1()(64bit)".into(),
                            kind: BreakKind::Fti,
                        }],
                        arrival: None,
                    },
                )]),
            }),
        };
        // Without color, the Markdown a comment carries.
        let plain = checks(std::slice::from_ref(&c), None, false);
        assert_eq!(plain, c.markdown());
        // Two checks: one blank line between them.
        let two = checks(&[c.clone(), c.clone()], None, false);
        assert_eq!(two, format!("{}\n{}", c.markdown(), c.markdown()));
        let colored = checks(std::slice::from_ref(&c), None, true);
        assert!(
            colored.contains(&format!("{RED}- libr.so.1()(64bit)")),
            "{colored}"
        );
        assert!(colored.contains(&format!("{BOLD}{RED}- user")), "{colored}");
        // Nothing could be checked: said first.
        assert!(
            checks(&[], Some("no CBS hub"), false)
                .starts_with("Not checked for breakage: no CBS hub")
        );
    }
}
