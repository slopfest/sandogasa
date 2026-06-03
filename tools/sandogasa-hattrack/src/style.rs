// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Minimal ANSI styling for terminal output.
//!
//! Auto mode follows the `grep`/`ls` convention: colorize only
//! when stdout is a TTY and `NO_COLOR` is unset
//! (<https://no-color.org/>).

use std::io::IsTerminal;

const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
// SGR 2 ("faint") alone is unreliable — gnome-terminal and a
// few others render it identically to normal. Use SGR 90
// ("bright black" / gray foreground) instead — universally
// rendered as a distinct gray without doubling the dimming.
const DIM: &str = "\x1b[90m";
const RESET: &str = "\x1b[0m";

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
        ColorChoice::Auto => no_color_unset() && std::io::stdout().is_terminal(),
    }
}

fn no_color_unset() -> bool {
    std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty())
}

/// Parse a `START-END` working-hours range as two 0–24 ints.
/// Both ends are local-clock hours; start is inclusive, end is
/// exclusive, matching "before 9 / after 6" intuition (so
/// `9-18` means 09:00–17:59 is in-hours, 18:00 onwards is out).
pub fn parse_working_hours(s: &str) -> Result<(u8, u8), String> {
    let (a, b) = s
        .split_once('-')
        .ok_or_else(|| format!("expected START-END, got `{s}`"))?;
    let start: u8 = a
        .trim()
        .parse()
        .map_err(|e| format!("invalid start `{a}`: {e}"))?;
    let end: u8 = b
        .trim()
        .parse()
        .map_err(|e| format!("invalid end `{b}`: {e}"))?;
    if start > 24 || end > 24 {
        return Err(format!("hours must be 0-24, got `{s}`"));
    }
    if start >= end {
        return Err(format!("start must be < end, got `{s}`"));
    }
    Ok((start, end))
}

fn paint(s: &str, code: &str, on: bool) -> String {
    if on {
        format!("{code}{s}{RESET}")
    } else {
        s.to_string()
    }
}

/// Style the `Local time:` value: dim the timestamp when the
/// local hour falls outside the working-hours range; colorize
/// the weekday block green for a weekday or yellow for a
/// weekend. `is_weekend = None` (country unknown) leaves the
/// weekday plain.
pub fn local_time_line(
    display_time: &str,
    hour: u32,
    weekday: chrono::Weekday,
    is_weekend: Option<bool>,
    working_hours: (u8, u8),
    color: bool,
) -> String {
    let (start, end) = working_hours;
    let in_hours = hour >= u32::from(start) && hour < u32::from(end);
    let time = if in_hours {
        display_time.to_string()
    } else {
        paint(display_time, DIM, color)
    };

    let weekday_tag = match is_weekend {
        Some(true) => paint(&format!("{weekday} — weekend"), YELLOW, color),
        Some(false) => paint(&format!("{weekday} — weekday"), GREEN, color),
        None => weekday.to_string(),
    };

    format!("{time} ({weekday_tag})")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    #[test]
    fn parse_working_hours_default() {
        assert_eq!(parse_working_hours("9-18").unwrap(), (9, 18));
    }

    #[test]
    fn parse_working_hours_with_whitespace() {
        assert_eq!(parse_working_hours(" 9 - 18 ").unwrap(), (9, 18));
    }

    #[test]
    fn parse_working_hours_rejects_inverted() {
        assert!(parse_working_hours("18-9").is_err());
        assert!(parse_working_hours("9-9").is_err());
    }

    #[test]
    fn parse_working_hours_rejects_out_of_range() {
        assert!(parse_working_hours("9-25").is_err());
    }

    #[test]
    fn parse_working_hours_rejects_malformed() {
        assert!(parse_working_hours("9").is_err());
        assert!(parse_working_hours("a-b").is_err());
    }

    #[test]
    fn local_time_line_in_hours_weekday_with_color() {
        let out = local_time_line(
            "2026-06-03 14:00:00 IST",
            14,
            Weekday::Wed,
            Some(false),
            (9, 18),
            true,
        );
        assert!(!out.contains(DIM), "in-hours time should not be dimmed");
        assert!(out.contains(GREEN), "weekday should be green");
        assert!(out.contains("Wed — weekday"));
    }

    #[test]
    fn local_time_line_out_of_hours_weekend_with_color() {
        let out = local_time_line(
            "2026-06-06 22:00:00 IST",
            22,
            Weekday::Sat,
            Some(true),
            (9, 18),
            true,
        );
        assert!(out.contains(DIM), "out-of-hours time should be dimmed");
        assert!(out.contains(YELLOW), "weekend should be yellow");
        assert!(out.contains("Sat — weekend"));
    }

    #[test]
    fn local_time_line_no_color_strips_ansi() {
        let out = local_time_line(
            "2026-06-03 14:00:00 IST",
            14,
            Weekday::Wed,
            Some(false),
            (9, 18),
            false,
        );
        assert!(!out.contains('\x1b'));
        assert!(out.contains("Wed — weekday"));
    }

    #[test]
    fn local_time_line_unknown_country_leaves_weekday_plain() {
        let out = local_time_line(
            "2026-06-03 14:00:00 UTC",
            14,
            Weekday::Wed,
            None,
            (9, 18),
            true,
        );
        // The weekday block isn't wrapped in green or yellow.
        assert!(out.contains("Wed"));
        assert!(!out.contains("— weekday"));
        assert!(!out.contains("— weekend"));
    }

    #[test]
    fn local_time_line_end_hour_is_exclusive() {
        // 18:00 sharp should be dimmed (i.e. "after 6 PM").
        let out = local_time_line(
            "2026-06-03 18:00:00 IST",
            18,
            Weekday::Wed,
            Some(false),
            (9, 18),
            true,
        );
        assert!(out.contains(DIM));
    }

    #[test]
    fn local_time_line_start_hour_is_inclusive() {
        // 09:00 sharp should NOT be dimmed.
        let out = local_time_line(
            "2026-06-03 09:00:00 IST",
            9,
            Weekday::Wed,
            Some(false),
            (9, 18),
            true,
        );
        assert!(!out.contains(DIM));
    }
}
