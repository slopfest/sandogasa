// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Narration and command execution for a tool whose steps are commands
//! a person could run by hand (`git`, `gbp`, `centpkg`, `koji`, …), so
//! that someone following along learns the workflow, or checks the
//! tool, and could do it without it.
//!
//! `--explain` runs each command after narrating it and showing it
//! exactly as it would be typed, pausing for Enter first; `--dry-run`
//! narrates without running anything; the two together are a tutorial.
//! A plain run just does the work. Output goes through `anstream`,
//! which strips color when the stream isn't a terminal or `NO_COLOR`
//! is set, so piped output stays clean.
//!
//! Extracted from dbranch, whose `--explain` the workspace rule names
//! as the model; see `tools/dbranch/CLAUDE.md` for the contract.

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
        write!(f, "{} exited with status {}", self.command, self.code)
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
        print_command(argv);
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

    /// In `--explain` (real runs only), show `git diff` for the paths
    /// dbranch just edited by hand — resolving the changelog conflict,
    /// normalizing the entry, the gbp.conf / salsa-ci.yml tweaks — and
    /// pause, so the user sees the change before it's committed. `git
    /// diff` is itself a real command they can run to see the same
    /// thing. No-op outside `--explain` and under `--dry-run` (nothing
    /// was edited).
    pub fn explain_diff(&self, repo: &Path, paths: &[&str]) {
        self.explain_diff_with(repo, paths, false);
    }

    /// Like [`explain_diff`](Self::explain_diff) but shows *staged*
    /// changes (`git diff --cached`) — for a freshly created file, which
    /// must be `git add`ed first and so isn't visible in the unstaged
    /// diff.
    pub fn explain_diff_cached(&self, repo: &Path, paths: &[&str]) {
        self.explain_diff_with(repo, paths, true);
    }

    /// Shared core of [`explain_diff`](Self::explain_diff) /
    /// [`explain_diff_cached`](Self::explain_diff_cached): run
    /// `git diff [--cached] -- <paths>` and pause.
    fn explain_diff_with(&self, repo: &Path, paths: &[&str], cached: bool) {
        if !self.explain || self.dry_run {
            return;
        }
        let mut cmd = Command::new("git");
        cmd.args(["--no-pager", "diff"]);
        if cached {
            cmd.arg("--cached");
        }
        let _ = cmd.arg("--").args(paths).current_dir(repo).status();
        self.pause_with("[Enter to continue]");
    }

    /// Ask a yes/no question (default **yes**) and return the answer.
    /// Empty input / EOF → yes; only `n`/`no` is a no. The caller is
    /// responsible for only prompting when interactive.
    pub fn confirm(&self, question: &str) -> bool {
        crate::confirm(question, true).unwrap_or(true)
    }

    /// Ask a yes/no question defaulting to **no** (`[y/N]`) — for a
    /// safety prompt where the cautious choice is *not* to proceed (e.g.
    /// "the package isn't in this PPA — upload anyway?"). Only `y`/`yes`
    /// is a yes; empty input / EOF → no. The caller is responsible for
    /// only prompting when interactive.
    pub fn confirm_default_no(&self, question: &str) -> bool {
        crate::confirm(question, false).unwrap_or(false)
    }

    /// Offer a numbered choice on stderr and read the pick from stdin;
    /// `None` on EOF or a non-answer. The caller is responsible for
    /// only prompting when interactive.
    pub fn choose(&self, question: &str, options: &[String]) -> Option<usize> {
        use std::io::Write;
        let mut err = std::io::stderr();
        for (i, option) in options.iter().enumerate() {
            let _ = writeln!(err, "  {}) {option}", i + 1);
        }
        let _ = write!(err, "{question} [1-{}]: ", options.len());
        let _ = err.flush();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok()?;
        let n: usize = line.trim().parse().ok()?;
        (1..=options.len()).contains(&n).then(|| n - 1)
    }

    /// In `--explain`, wait for the user to press Enter before
    /// running the command just shown (a step-through walkthrough).
    /// A non-interactive stdin (EOF) continues without blocking.
    fn pause(&self) {
        self.pause_with("[Enter to run, Ctrl-C to abort]");
    }

    /// Print `prompt` to stderr and wait for a line on stdin (EOF
    /// continues without blocking).
    fn pause_with(&self, prompt: &str) {
        use std::io::Write;
        let mut err = std::io::stderr();
        let _ = write!(err, "    {prompt} ");
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

    /// Like [`Ui::run_required`], but feeding `input` to the command's
    /// stdin. Narrated as `echo '<input>' | <command>` so the printed
    /// line still reproduces the step by hand (for a tool that reads
    /// its data from stdin, like `debusine workflow start --data -`).
    pub fn run_required_with_input(
        &self,
        input: &str,
        argv: &[String],
        cwd: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut shown = vec![
            "echo".to_string(),
            input.trim_end().to_string(),
            "|".to_string(),
        ];
        shown.extend(argv.iter().cloned());
        self.show_command_raw(&shown, 2);
        if self.dry_run {
            return Ok(());
        }
        if self.explain {
            self.pause();
        }
        use std::io::Write;
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            // A command can exit without reading all of its input, which
            // closes the pipe under the write: its exit status, below,
            // says how it went, not the broken pipe.
            match stdin.write_all(input.as_bytes()) {
                Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
                _ => {}
            }
        }
        let code = child.wait()?.code().unwrap_or(1);
        if code == 0 {
            Ok(())
        } else {
            Err(Box::new(StageFailure {
                command: shown.join(" "),
                code,
            }))
        }
    }

    /// [`Ui::show_command`] with the argument at `raw_at` printed as
    /// is (a shell operator such as `|`, which must not be quoted).
    fn show_command_raw(&self, argv: &[String], raw_at: usize) {
        if !self.narrating() {
            return;
        }
        let line = argv
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if i == raw_at {
                    a.clone()
                } else {
                    shell_quote(a)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        anstream::println!("{}", styled_line(&line));
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

/// A command as a person would type it, each argument quoted for the
/// shell where it needs to be: what `--explain` shows, for a tool that
/// also prints a command outside the narration (in a confirmation, or
/// as the next step to run).
pub fn command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `argv` as [`Ui::show_command`] prints it while narrating: indented,
/// after a dim `$`, in bold green, so a command the tool tells the user
/// to run, or asks before running, stands out the same way.
fn styled_command(argv: &[String]) -> String {
    styled_line(&command_line(argv))
}

/// A command line already quoted, styled as [`styled_command`] says.
fn styled_line(line: &str) -> String {
    let prompt = Style::new().fg_color(Some(AnsiColor::BrightBlack.into()));
    let cmd = Style::new().fg_color(Some(AnsiColor::Green.into())).bold();
    format!(
        "    {}${} {}{line}{}",
        prompt.render(),
        prompt.render_reset(),
        cmd.render(),
        cmd.render_reset()
    )
}

/// Print `argv` to stdout as `--explain` shows a command, whether or
/// not the run is narrating: for a command in a confirmation, or one
/// the user is told to run next. The color is stripped where stdout is
/// not a terminal.
pub fn print_command(argv: &[String]) {
    anstream::println!("{}", styled_command(argv));
}

/// [`print_command`] to stderr, for a command in a note or a warning.
pub fn eprint_command(argv: &[String]) {
    anstream::eprintln!("{}", styled_command(argv));
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
    fn input_is_fed_to_the_command_and_narrated_as_a_pipe() {
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: false,
        };
        let sh = |script: &str| ["sh", "-c", script].map(String::from).to_vec();
        ui.run_required_with_input(
            "hello\n",
            &sh("read x; test \"$x\" = hello"),
            Path::new("."),
        )
        .unwrap();
        let err = ui
            .run_required_with_input("hello\n", &sh("exit 3"), Path::new("."))
            .unwrap_err();
        let failure = err.downcast_ref::<StageFailure>().unwrap();
        assert_eq!(failure.code, 3);
        assert_eq!(failure.command, "echo hello | sh -c exit 3");
        // More input than a pipe holds, to a command that reads none of
        // it: still its exit status, not a broken pipe.
        let big = "x".repeat(1 << 20);
        let err = ui
            .run_required_with_input(&big, &sh("exit 4"), Path::new("."))
            .unwrap_err();
        assert_eq!(
            err.downcast_ref::<StageFailure>().map(|f| f.code),
            Some(4),
            "{err}"
        );
        // A dry run narrates the pipe and runs nothing.
        let dry = Ui {
            dry_run: true,
            ..ui
        };
        dry.run_required_with_input("hello\n", &sh("exit 3"), Path::new("."))
            .unwrap();
    }

    #[test]
    fn a_diff_is_shown_only_while_explaining_a_real_run() {
        // Neither runs git here: one is not explaining, the other a dry
        // run, where nothing was edited.
        let plain = Ui {
            explain: false,
            dry_run: false,
            quiet: false,
        };
        plain.explain_diff(Path::new("/nonexistent"), &["x"]);
        let dry = Ui {
            explain: true,
            dry_run: true,
            quiet: false,
        };
        dry.explain_diff_cached(Path::new("/nonexistent"), &["x"]);
    }

    #[test]
    fn a_command_is_printed_to_either_stream() {
        let argv = ["cbspkg", "upload-sources"].map(String::from);
        print_command(&argv);
        eprint_command(&argv);
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
    fn a_command_line_quotes_only_what_needs_it() {
        let argv: Vec<String> = ["cbs", "build", "t", "git+https://x/y.git#abc"]
            .map(String::from)
            .to_vec();
        assert_eq!(command_line(&argv), "cbs build t 'git+https://x/y.git#abc'");
        // As --explain shows it: after a `$`, the same quoting, in color.
        let styled = styled_command(&argv);
        assert!(styled.starts_with("    "), "{styled:?}");
        assert!(styled.contains("$\u{1b}"), "{styled:?}");
        assert!(styled.contains(&command_line(&argv)), "{styled:?}");
        assert!(
            styled.contains("\u{1b}[32m") || styled.contains("\u{1b}[1m"),
            "{styled:?}"
        );
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
