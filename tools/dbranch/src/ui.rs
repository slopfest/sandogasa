// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Narration + command execution. `--explain` runs commands and
//! narrates them; `--dry-run` narrates without running. Both print
//! through `anstream`, which strips color when the stream isn't a
//! terminal or `NO_COLOR` is set, so piped output stays clean.

use std::path::Path;
use std::process::Command;

use anstyle::{AnsiColor, Style};

/// Controls how steps are narrated and whether they execute.
pub struct Ui {
    /// Narrate each step and run it (follow-along / sanity-check).
    pub explain: bool,
    /// Narrate without running anything.
    pub dry_run: bool,
}

impl Ui {
    /// Whether steps are narrated (either teaching mode is on).
    fn narrating(&self) -> bool {
        self.explain || self.dry_run
    }

    /// Print a step heading (only when narrating).
    pub fn step(&self, describe: &str) {
        if !self.narrating() {
            return;
        }
        let s = Style::new().bold();
        anstream::println!("\n{}» {describe}{}", s.render(), s.render_reset());
    }

    /// Print a command exactly as it would be typed (only when
    /// narrating), so it can be copy-pasted.
    pub fn show_command(&self, argv: &[String]) {
        if !self.narrating() {
            return;
        }
        let prompt = Style::new().fg_color(Some(AnsiColor::BrightBlack.into()));
        let cmd = Style::new().fg_color(Some(AnsiColor::Green.into())).bold();
        let line = argv
            .iter()
            .map(|a| shell_quote(a))
            .collect::<Vec<_>>()
            .join(" ");
        anstream::println!(
            "    {}${} {}{line}{}",
            prompt.render(),
            prompt.render_reset(),
            cmd.render(),
            cmd.render_reset()
        );
    }

    /// In `--explain`, wait for the user to press Enter before
    /// running the command just shown (a step-through walkthrough).
    /// A non-interactive stdin (EOF) continues without blocking.
    fn pause(&self) {
        use std::io::Write;
        let mut err = std::io::stderr();
        let _ = write!(err, "    [Enter to run, Ctrl-C to abort] ");
        let _ = err.flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
    }

    /// Run a command in `cwd`, narrating it first. Returns whether it
    /// exited successfully. In `--dry-run` nothing runs and this
    /// returns `Ok(true)`; in `--explain` it pauses for Enter first.
    pub fn run(&self, argv: &[String], cwd: &Path) -> std::io::Result<bool> {
        self.show_command(argv);
        if self.dry_run {
            return Ok(true);
        }
        if self.explain {
            self.pause();
        }
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .status()?;
        Ok(status.success())
    }

    /// Run a command that must succeed; error otherwise.
    pub fn run_required(
        &self,
        argv: &[String],
        cwd: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.run(argv, cwd)? {
            Ok(())
        } else {
            Err(format!("command failed: {}", argv.join(" ")).into())
        }
    }
}

/// Quote an argument for display so the printed line is copy-paste
/// safe. Leaves shell-safe tokens (including Debian version chars
/// `~+:`) unquoted; single-quotes anything else.
fn shell_quote(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }
    let safe = arg
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-/=~+:,".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_leaves_simple_and_version_tokens_bare() {
        assert_eq!(shell_quote("git"), "git");
        assert_eq!(
            shell_quote("../damo_3.2.8-1~questing+1.dsc"),
            "../damo_3.2.8-1~questing+1.dsc"
        );
        assert_eq!(shell_quote("3.2.8-1~questing+1"), "3.2.8-1~questing+1");
    }

    #[test]
    fn shell_quote_quotes_spaces_and_specials() {
        assert_eq!(
            shell_quote("Update changelog for 3.2.8-1~questing+1 release"),
            "'Update changelog for 3.2.8-1~questing+1 release'"
        );
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
        assert_eq!(shell_quote(""), "''");
    }
}
