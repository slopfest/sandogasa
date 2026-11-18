// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The plan a confirmation asks about (`Start build?`, `Promote?`,
//! `Untag?`): what will run and what will be posted, styled by the
//! workspace's color chart. Headings are bold, commands bold green as
//! `--explain` shows them just before running, and the tracking-issue
//! comments as `sandogasa_cbs::notify::describe_styled` renders them.
//! The plan goes to standard error with its question, colored on a
//! terminal only.

use sandogasa_cbs::Stage;
use sandogasa_cbs::check::{Check, is_soname};
use sandogasa_cbs::notify::{Notice, describe_styled};
use sandogasa_cbs::tags::TagChange;
use sandogasa_cli::explain::command_line;
use sandogasa_cli::style::{BOLD, GREEN, RED, YELLOW, paint};

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

/// What `found` says, per build: what it drops, red for a soname and
/// yellow otherwise, with the packages elsewhere requiring it in bold
/// red, and what could not be checked in yellow. The rest (nothing
/// dropped, what another package still provides, the version changes)
/// is the result too, so it keeps the normal text color. `unchecked` is why
/// nothing could be checked at all, when that is so.
pub fn checks(found: &[Check], unchecked: Option<&str>, color: bool) -> String {
    let mut out = String::new();
    if let Some(why) = unchecked {
        out.push_str(&format!(
            "{}\n",
            paint(&format!("Not checked for breakage: {why}"), YELLOW, color)
        ));
    }
    for c in found {
        let Some(against) = &c.against else {
            out.push_str(&format!(
                "{}: nothing in -release to compare it with\n",
                c.nvr
            ));
            continue;
        };
        out.push_str(&heading(&format!("{} against {against}:", c.nvr), color));
        let dropped = c.dropped();
        if dropped.is_empty() {
            out.push_str("  drops no Provides\n");
        }
        for d in &dropped {
            let style = if is_soname(d) { RED } else { YELLOW };
            out.push_str(&format!(
                "  {}\n",
                paint(&format!("drops {d}"), style, color)
            ));
        }
        for (place, cap, by) in &c.still_provided {
            out.push_str(&format!(
                "  {cap} is still provided in {place} by {}\n",
                by.join(", ")
            ));
        }
        for (place, cap, users) in c.required() {
            let what = if cap.is_empty() {
                "something it drops".to_string()
            } else {
                cap.to_string()
            };
            out.push_str(&format!(
                "  {}\n",
                paint(
                    &format!("{what} is required in {place} by {}", users.join(", ")),
                    &format!("{BOLD}{RED}"),
                    color
                )
            ));
        }
        if !dropped.is_empty() && !c.breaks() && c.unchecked.is_empty() {
            out.push_str("  nothing in CentOS Stream or EPEL requires what it drops\n");
        }
        let changed = c.changed();
        if let Some((old, new)) = changed.first() {
            let more = match changed.len() {
                1 => String::new(),
                n => format!(" and {} more", n - 1),
            };
            out.push_str(&format!("  changes {old} to {new}{more}\n"));
        }
        for u in &c.unchecked {
            out.push_str(&format!(
                "  {}\n",
                paint(&format!("not looked in: {u}"), YELLOW, color)
            ));
        }
    }
    out
}

/// What `cbspkg promote` will do: what the checks of builds going to
/// `-testing` found, each build's tag changes under a heading naming
/// where it goes, the comments, then the older builds it offers to
/// untag, asked separately.
pub fn promote(
    steps: &[(String, Stage, Vec<TagChange>)],
    found: &[Check],
    unchecked: Option<&str>,
    notices: &[Notice],
    replaced: &[TagChange],
    postable: bool,
    color: bool,
) -> String {
    let mut out = checks(found, unchecked, color);
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
            promote(&steps, &[], None, &[], &replaced, true, false),
            "r-1-1.hs.el10 to -testing:\n  \
             cbs tag-build hyperscale10s-packages-main-testing r-1-1.hs.el10\n\
             Then, asked separately, the older builds they replace:\n  \
             cbs untag-build hyperscale10s-packages-main-testing r-0-1.hs.el10\n"
        );
        let colored = promote(&steps, &[], None, &[], &[], true, true);
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

    #[test]
    fn a_check_shows_what_breaks_what_does_not_and_what_was_not_looked_at() {
        use sandogasa_cbs::check::compare;
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        let old = set(&[
            "libr.so.1()(64bit)",
            "r-tools = 1-1",
            "r = 1-1",
            "r-data = 1-1",
        ]);
        let new = set(&["r = 2-1", "r-data = 2-1"]);
        let mut c = compare("r-2-1", Some(("r-1-1", &old)), &new);
        c.still_provided = vec![(
            "CentOS Stream 10".into(),
            "r-tools".into(),
            vec!["other".into()],
        )];
        c.users = vec![(
            "CentOS Stream 10".into(),
            vec![("libr.so.1()(64bit)".into(), vec!["user".into()])],
        )];
        c.unchecked = vec!["EPEL 10: no such release".into()];
        let shown = checks(std::slice::from_ref(&c), None, false);
        assert_eq!(
            shown,
            "r-2-1 against r-1-1:\n  \
             drops libr.so.1()(64bit)\n  \
             drops r-tools = 1-1\n  \
             r-tools is still provided in CentOS Stream 10 by other\n  \
             libr.so.1()(64bit) is required in CentOS Stream 10 by user\n  \
             changes r = 1-1 to r = 2-1 and 1 more\n  \
             not looked in: EPEL 10: no such release\n"
        );
        let colored = checks(std::slice::from_ref(&c), None, true);
        assert!(
            colored.contains(&format!("{RED}drops libr.so.1")),
            "{colored}"
        );
        assert!(
            colored.contains(&format!("{YELLOW}drops r-tools")),
            "{colored}"
        );
        assert!(
            colored.contains(&format!("{BOLD}{RED}libr.so.1()(64bit) is required in")),
            "{colored}"
        );
        assert!(
            colored.contains(&format!("{YELLOW}not looked in")),
            "{colored}"
        );
        // Results that are not findings keep the normal text color.
        assert!(
            colored.contains("\n  r-tools is still provided in CentOS Stream 10 by other\n"),
            "{colored}"
        );
        assert!(
            colored.contains("\n  changes r = 1-1 to r = 2-1 and 1 more\n"),
            "{colored}"
        );
        // Dropped, looked up everywhere, used nowhere.
        c.users = vec![("CentOS Stream 10".into(), vec![])];
        c.unchecked.clear();
        assert!(checks(&[c], None, false).contains("nothing in CentOS Stream or EPEL requires"));
        // Nothing dropped, one version change.
        let only = compare(
            "r-2-1",
            Some(("r-1-1", &set(&["r = 1-1"]))),
            &set(&["r = 2-1"]),
        );
        assert_eq!(
            checks(&[only], None, false),
            "r-2-1 against r-1-1:\n  drops no Provides\n  changes r = 1-1 to r = 2-1\n"
        );
    }
}
