// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What to run next, after a command that leaves a step to the person:
//! the step, then the command on a line of its own in bold green (a
//! command to run, in the workspace's color chart), and the commands
//! that do it by hand in gray. Printed to standard error, colored on a
//! terminal only.

use sandogasa_cli::explain::command_line;
use sandogasa_cli::style::{DIM, paint};

/// `what` to do next, `command` that does it, and `by_hand`, the
/// commands that do the same without cbspkg (none when there are
/// none).
pub fn next_step(what: &str, command: &[String], by_hand: &[Vec<String>], color: bool) -> String {
    let mut out = format!(
        "Next, {what}:\n  {}\n",
        paint(&command_line(command), &crate::plan::command_style(), color)
    );
    for argv in by_hand {
        let line = format!("(or by hand: {})", command_line(argv));
        out.push_str(&format!("  {}\n", paint(&line, DIM, color)));
    }
    out
}

/// [`next_step`] on standard error, colored when it is a terminal.
pub fn print_next_step(what: &str, command: &[String], by_hand: &[Vec<String>]) {
    eprint!(
        "{}",
        next_step(what, command, by_hand, sandogasa_cli::style::stderr_color())
    );
}

/// `nvrs` as a list in a sentence: `a`, `a and b`, `a, b and c`.
pub fn listed(nvrs: &[&str]) -> String {
    match nvrs {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The `cbspkg promote` moving `nvrs` to their next stage: one build by
/// its NVR, whichever branch or variant it is run from; several in one
/// command with the selection they were made with (`-C`, `--branch`,
/// `--variant`, `--rhel`), since without an NVR promote takes each
/// collection's newest build in the stage before, and a package's
/// configured variants apply to it as they did before.
pub fn promote_command(
    nvrs: &[&str],
    repo: Option<&std::path::Path>,
    branch: Option<&str>,
    variants: &crate::add_pkg::VariantArgs,
) -> Vec<String> {
    let mut command = vec!["cbspkg".to_string(), "promote".to_string()];
    if let [nvr] = nvrs {
        command.push(nvr.to_string());
        return command;
    }
    if let Some(repo) = repo {
        command.extend(["-C".to_string(), repo.display().to_string()]);
    }
    if let Some(branch) = branch {
        command.extend(["--branch".to_string(), branch.to_string()]);
    }
    for v in &variants.variant {
        command.extend(["--variant".to_string(), v.clone()]);
    }
    for v in &variants.rhel {
        command.extend(["--rhel".to_string(), v.clone()]);
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    use sandogasa_cli::style::{BOLD, GREEN};

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn the_command_has_a_line_of_its_own() {
        let nvr = "rasdaemon-1.0.0-1.hs+fb.el10";
        let shown = next_step(
            &format!("to offer {nvr} for testing"),
            &argv(&["cbspkg", "promote", nvr]),
            &[argv(&[
                "cbs",
                "tag-build",
                "hyperscale10s-packages-facebook-testing",
                nvr,
            ])],
            false,
        );
        assert_eq!(
            shown,
            "Next, to offer rasdaemon-1.0.0-1.hs+fb.el10 for testing:\n  \
             cbspkg promote rasdaemon-1.0.0-1.hs+fb.el10\n  \
             (or by hand: cbs tag-build hyperscale10s-packages-facebook-testing \
             rasdaemon-1.0.0-1.hs+fb.el10)\n"
        );
        let colored = next_step("x", &argv(&["cbspkg", "promote"]), &[argv(&["cbs"])], true);
        assert!(
            colored.contains(&format!("{BOLD}{GREEN}cbspkg promote")),
            "{colored}"
        );
        assert!(
            colored.contains(&format!("{DIM}(or by hand: cbs)")),
            "{colored}"
        );
        assert_eq!(next_step("x", &argv(&["y"]), &[], false), "Next, x:\n  y\n");
    }

    #[test]
    fn builds_are_listed_and_promoted_together() {
        assert_eq!(listed(&[]), "");
        assert_eq!(listed(&["a"]), "a");
        assert_eq!(listed(&["a", "b"]), "a and b");
        assert_eq!(listed(&["a", "b", "c"]), "a, b and c");
        let mut variants = crate::add_pkg::VariantArgs::default();
        let line = |c: Vec<String>| c.join(" ");
        assert_eq!(
            line(promote_command(&["n-1-1"], None, Some("b"), &variants)),
            "cbspkg promote n-1-1"
        );
        variants.variant = vec!["main".into(), "facebook".into()];
        variants.rhel = vec!["facebook".into()];
        assert_eq!(
            line(promote_command(
                &["a", "b"],
                Some(std::path::Path::new("/w")),
                Some("c10s-hs"),
                &variants
            )),
            "cbspkg promote -C /w --branch c10s-hs --variant main --variant facebook --rhel facebook"
        );
        assert_eq!(
            line(promote_command(
                &["a", "b"],
                None,
                None,
                &Default::default()
            )),
            "cbspkg promote"
        );
    }
}
