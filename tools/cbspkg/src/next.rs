// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What to run next, after a command that leaves a step to the person:
//! the step, then the command on a line of its own in bold green (a
//! command to run, in the workspace's color chart), and the commands
//! that do it by hand in gray. Printed to standard error, colored on a
//! terminal only.

use sandogasa_cli::explain::command_line;
use sandogasa_cli::style::{BOLD, DIM, GREEN, paint};

/// `what` to do next, `command` that does it, and `by_hand`, the
/// commands that do the same without cbspkg (none when there are
/// none).
pub fn next_step(what: &str, command: &[String], by_hand: &[Vec<String>], color: bool) -> String {
    let mut out = format!(
        "Next, {what}:\n  {}\n",
        paint(&command_line(command), &format!("{BOLD}{GREEN}"), color)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
