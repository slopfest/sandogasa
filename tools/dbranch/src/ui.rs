// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Narration + command execution. `--explain` runs commands and
//! narrates them; `--dry-run` narrates without running. Both print
//! through `anstream`, which strips color when the stream isn't a
//! terminal or `NO_COLOR` is set, so piped output stays clean.

use std::path::Path;
use std::process::{Command, Stdio};

use anstyle::{AnsiColor, Style};

/// A stage command exited non-zero. Carries the exit code so the
/// process can propagate the real status instead of a generic `1`.
#[derive(Debug)]
pub struct StageFailure {
    pub command: String,
    pub code: i32,
}

impl std::fmt::Display for StageFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` exited with status {}", self.command, self.code)
    }
}

impl std::error::Error for StageFailure {}

/// Controls how steps are narrated and whether they execute.
pub struct Ui {
    /// Narrate each step and run it (follow-along / sanity-check).
    pub explain: bool,
    /// Narrate without running anything.
    pub dry_run: bool,
    /// Suppress shelled-out tool output, surfacing it only on failure.
    pub quiet: bool,
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

    /// In `--explain`, pause for Enter before proceeding; a no-op
    /// otherwise. For steps that aren't a single `run_*` call (e.g.
    /// the `glab ci list` poll loop) the caller invokes this after
    /// `show_command` to keep the step-through walkthrough contract.
    pub fn pause_if_explain(&self) {
        if self.explain {
            self.pause();
        }
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

    /// Run a command in `cwd`, narrating it first, and return its exit
    /// code (`0` on success). In `--dry-run` nothing runs and this
    /// returns `Ok(0)`; in `--explain` it pauses for Enter first. A
    /// signal-terminated child reports `1`. In `--quiet` the command's
    /// output is captured and replayed only if it fails, so a normal
    /// run shows just dbranch's own narration.
    pub fn run_status(&self, argv: &[String], cwd: &Path) -> std::io::Result<i32> {
        self.show_command(argv);
        if self.dry_run {
            return Ok(0);
        }
        if self.explain {
            self.pause();
        }
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]).current_dir(cwd);
        if !self.quiet {
            return Ok(cmd.status()?.code().unwrap_or(1));
        }
        // Quiet: swallow the tool's chatter, but replay it on failure
        // so a problem is still diagnosable.
        let out = cmd.output()?;
        let code = out.status.code().unwrap_or(1);
        if code != 0 {
            use std::io::Write;
            let _ = std::io::stdout().write_all(&out.stdout);
            let _ = std::io::stderr().write_all(&out.stderr);
        }
        Ok(code)
    }

    /// Whether the command exited successfully.
    pub fn run(&self, argv: &[String], cwd: &Path) -> std::io::Result<bool> {
        Ok(self.run_status(argv, cwd)? == 0)
    }

    /// Like [`Ui::run_status`], but captures the command's combined
    /// stdout+stderr alongside the exit code. Used where the tool is
    /// quiet on success (e.g. lintian) and we want to echo + summarize
    /// its output. In `--dry-run` returns `(0, "")` without running.
    pub fn run_capture(&self, argv: &[String], cwd: &Path) -> std::io::Result<(i32, String)> {
        self.show_command(argv);
        if self.dry_run {
            return Ok((0, String::new()));
        }
        if self.explain {
            self.pause();
        }
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .output()?;
        let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code().unwrap_or(1), combined))
    }

    /// Run a query command with stdin on `/dev/null` and capture its
    /// stdout and stderr separately, returning the exit code too. The
    /// null stdin keeps an interactive tool from blocking on a prompt
    /// (glab's `ci status` action menu only appears with a TTY on
    /// stdin). Unlike [`Ui::run_status`] this neither narrates nor
    /// pauses — it's a silent probe meant to be called repeatedly in a
    /// poll loop. The caller is responsible for the `--dry-run` guard.
    pub fn run_query(&self, argv: &[String], cwd: &Path) -> std::io::Result<(i32, String, String)> {
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::null())
            .output()?;
        Ok((
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ))
    }

    /// Run a command that must succeed; otherwise return a
    /// [`StageFailure`] carrying its exit code so the process can exit
    /// with the same status.
    pub fn run_required(
        &self,
        argv: &[String],
        cwd: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let code = self.run_status(argv, cwd)?;
        if code == 0 {
            Ok(())
        } else {
            Err(Box::new(StageFailure {
                command: argv.join(" "),
                code,
            }))
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
    fn run_required_propagates_exit_code() {
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: false,
        };
        let err = ui
            .run_required(
                &["sh".to_string(), "-c".to_string(), "exit 7".to_string()],
                Path::new("."),
            )
            .unwrap_err();
        let failure = err
            .downcast_ref::<StageFailure>()
            .expect("a StageFailure with the child's code");
        assert_eq!(failure.code, 7);
    }

    #[test]
    fn run_query_captures_stdout_with_null_stdin() {
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: false,
        };
        // Reads stdin; null stdin means it gets EOF and prints nothing
        // extra, proving the probe can't hang on input.
        let (code, out, _err) = ui
            .run_query(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo 'Pipeline state: success'; cat".to_string(),
                ],
                Path::new("."),
            )
            .unwrap();
        assert_eq!(code, 0);
        assert!(out.contains("Pipeline state: success"));
    }

    #[test]
    fn quiet_run_status_still_reports_exit_code() {
        // Quiet swallows output but the exit code is unaffected, so a
        // failing command still propagates through run_required.
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        };
        assert_eq!(
            ui.run_status(
                &[
                    "sh".to_string(),
                    "-c".to_string(),
                    "echo hi; exit 3".to_string()
                ],
                Path::new("."),
            )
            .unwrap(),
            3
        );
        assert!(ui.run(&["true".to_string()], Path::new(".")).unwrap());
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
