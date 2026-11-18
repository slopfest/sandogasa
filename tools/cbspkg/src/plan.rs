// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The plan a confirmation asks about (`Start build?`, `Promote?`,
//! `Untag?`): what will run and what will be posted, styled by the
//! workspace's color chart. Headings are bold, commands bold green as
//! `--explain` shows them just before running, and the tracking-issue
//! comments as `sandogasa_cbs::notify::describe_styled` renders them.
//! The plan goes to standard error with its question, colored on a
//! terminal only.

use sandogasa_cbs::Stage;
use sandogasa_cbs::notify::{Notice, describe_styled};
use sandogasa_cbs::tags::TagChange;
use sandogasa_cli::explain::command_line;
use sandogasa_cli::style::{BOLD, GREEN, paint};

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

/// What `cbspkg promote` will do: each build's tag changes under a
/// heading naming where it goes, the comments, then the older builds
/// it offers to untag, asked separately.
pub fn promote(
    steps: &[(String, Stage, Vec<TagChange>)],
    notices: &[Notice],
    replaced: &[TagChange],
    postable: bool,
    color: bool,
) -> String {
    let mut out = String::new();
    for (nvr, to, changes) in steps {
        out.push_str(&heading(&format!("{nvr} to -{}:", to.as_str()), color));
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
            promote(&steps, &[], &replaced, true, false),
            "r-1-1.hs.el10 to -testing:\n  \
             cbs tag-build hyperscale10s-packages-main-testing r-1-1.hs.el10\n\
             Then, asked separately, the older builds they replace:\n  \
             cbs untag-build hyperscale10s-packages-main-testing r-0-1.hs.el10\n"
        );
        let colored = promote(&steps, &[], &[], true, true);
        assert!(colored.starts_with(&format!("{BOLD}r-1-1.hs.el10 to -testing:{RESET}\n")));
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
}
