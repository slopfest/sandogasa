// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal ANSI styling for terminal output, shared by the tools.
//!
//! Auto mode follows the `grep`/`ls` convention: colorize only when
//! stdout is a TTY and `NO_COLOR` is unset (<https://no-color.org/>).
//! A tool takes the choice as a `--color` flag (`clap::ValueEnum`),
//! resolves it once with [`use_color`], and paints with [`paint`].

use std::io::IsTerminal;

pub const GREEN: &str = "\x1b[32m";
pub const YELLOW: &str = "\x1b[33m";
pub const RED: &str = "\x1b[31m";
pub const MAGENTA: &str = "\x1b[35m";
pub const BOLD: &str = "\x1b[1m";
/// SGR 2 ("faint") alone is unreliable — gnome-terminal and a few
/// others render it identically to normal. SGR 90 ("bright black",
/// a gray foreground) is rendered as a distinct gray everywhere.
pub const DIM: &str = "\x1b[90m";
pub const RESET: &str = "\x1b[0m";

/// How a tool should decide about color, from its `--color` flag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    /// Auto-detect: enable on a TTY when `NO_COLOR` is unset.
    #[default]
    Auto,
    /// Force colored output even when piped.
    Always,
    /// Disable colored output entirely.
    Never,
}

/// Resolve the user's color preference into a concrete bool.
pub fn use_color(choice: ColorChoice) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => auto(
            std::env::var_os("NO_COLOR").as_deref(),
            std::io::stdout().is_terminal(),
        ),
    }
}

/// `text` wrapped in `sgr` and a reset when `color` is on, bare
/// otherwise — so callers write one format string for both.
pub fn paint(text: &str, sgr: &str, color: bool) -> String {
    if color {
        format!("{sgr}{text}{RESET}")
    } else {
        text.to_string()
    }
}

/// Whether to color what goes to stderr (prompts, headings, warnings):
/// on a terminal when `NO_COLOR` is unset, whatever stdout is, so output
/// meant for a pipe stays clean while the prompts beside it are colored.
pub fn stderr_color() -> bool {
    auto(
        std::env::var_os("NO_COLOR").as_deref(),
        std::io::stderr().is_terminal(),
    )
}

/// The automatic choice: color on a terminal (`tty`) unless `NO_COLOR`
/// is set to something non-empty.
fn auto(no_color: Option<&std::ffi::OsStr>, tty: bool) -> bool {
    tty && no_color.is_none_or(|v| v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_wraps_only_when_color_is_on() {
        assert_eq!(paint("ok", GREEN, true), "\x1b[32mok\x1b[0m");
        assert_eq!(paint("ok", GREEN, false), "ok");
    }

    #[test]
    fn explicit_choices_ignore_the_terminal() {
        assert!(use_color(ColorChoice::Always));
        assert!(!use_color(ColorChoice::Never));
    }

    #[test]
    fn auto_colors_a_terminal_unless_no_color_is_set() {
        use std::ffi::OsStr;
        assert!(auto(None, true));
        assert!(auto(Some(OsStr::new("")), true));
        assert!(!auto(Some(OsStr::new("1")), true));
        assert!(!auto(None, false));
        // Whatever this test runs under, the answers agree with auto().
        let tty = std::io::stderr().is_terminal();
        assert_eq!(
            stderr_color(),
            auto(std::env::var_os("NO_COLOR").as_deref(), tty)
        );
        let _ = use_color(ColorChoice::Auto);
    }
}
