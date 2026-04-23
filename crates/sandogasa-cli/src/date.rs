// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared calendar date range parsing for CLI tools.
//!
//! Two forms are supported on the command line side:
//!
//! - `--since YYYY-MM-DD [--until YYYY-MM-DD]` — explicit,
//!   inclusive range. `until` defaults to today when omitted.
//! - `--period <token>` — a `YYYY`, `YYYYQ1..Q4`, or
//!   `YYYYH1..H2` shortcut that expands to the matching
//!   calendar range.
//!
//! Tools wire these up as two option groups and pass the raw
//! values into [`resolve_date_range`]. See
//! [`parse_period`] for the period token grammar.
//!
//! ```
//! use chrono::NaiveDate;
//! use sandogasa_cli::date::{parse_period, resolve_date_range};
//!
//! let (start, end) = parse_period("2026Q1").unwrap();
//! assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
//! assert_eq!(end,   NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
//!
//! let (s, e) = resolve_date_range(None, None, Some("2026H2")).unwrap();
//! assert_eq!(s, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
//! assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
//! ```

use chrono::NaiveDate;

/// Resolve a `(--since, --until, --period)` triple into an
/// inclusive date range.
///
/// Precedence: `period` wins when supplied. Otherwise
/// `since` + `until` are used (with `until` defaulting to
/// today's local date when absent). When all three are `None`,
/// the range is unbounded (`NaiveDate::MIN..=NaiveDate::MAX`).
///
/// Errors when `since` is strictly after `until`.
pub fn resolve_date_range(
    since: Option<NaiveDate>,
    until: Option<NaiveDate>,
    period: Option<&str>,
) -> Result<(NaiveDate, NaiveDate), String> {
    if let Some(p) = period {
        return parse_period(p);
    }
    let Some(since) = since else {
        return Ok((NaiveDate::MIN, NaiveDate::MAX));
    };
    let until = until.unwrap_or_else(|| chrono::Local::now().date_naive());
    if since > until {
        return Err(format!("--since ({since}) is after --until ({until})"));
    }
    Ok((since, until))
}

/// Parse a calendar-period shortcut into an inclusive
/// `(start, end)` range.
///
/// Accepted forms (case-insensitive on the suffix):
///
/// - `YYYY` — the full calendar year.
/// - `YYYYQ1` / `Q2` / `Q3` / `Q4` — that calendar quarter.
/// - `YYYYH1` / `H2` — the first or second half of the year.
pub fn parse_period(period: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let period = period.trim();
    if period.len() < 4 {
        return Err(format!(
            "invalid period: {period} (expected e.g. 2026, 2026Q1, or 2026H1)"
        ));
    }
    let (year_str, kind) = period.split_at(4);
    let year: i32 = year_str
        .parse()
        .map_err(|_| format!("invalid year in period: {period}"))?;
    if kind.is_empty() {
        return Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        ));
    }
    match kind.to_uppercase().as_str() {
        "Q1" => Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 3, 31).unwrap(),
        )),
        "Q2" => Ok((
            NaiveDate::from_ymd_opt(year, 4, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 6, 30).unwrap(),
        )),
        "Q3" => Ok((
            NaiveDate::from_ymd_opt(year, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 9, 30).unwrap(),
        )),
        "Q4" => Ok((
            NaiveDate::from_ymd_opt(year, 10, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        )),
        "H1" => Ok((
            NaiveDate::from_ymd_opt(year, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 6, 30).unwrap(),
        )),
        "H2" => Ok((
            NaiveDate::from_ymd_opt(year, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(year, 12, 31).unwrap(),
        )),
        _ => Err(format!(
            "invalid period: {period} (expected Q1-Q4 or H1-H2)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_period_bare_year() {
        let (s, e) = parse_period("2026").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_quarters_and_halves() {
        let (s, e) = parse_period("2026Q2").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 4, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 6, 30).unwrap());
        let (s, e) = parse_period("2026H1").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 6, 30).unwrap());
        let (s, e) = parse_period("2026h2").unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 7, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 12, 31).unwrap());
    }

    #[test]
    fn parse_period_rejects_garbage() {
        assert!(parse_period("202").is_err());
        assert!(parse_period("abcd").is_err());
        assert!(parse_period("2026Q9").is_err());
    }

    #[test]
    fn resolve_date_range_defaults_to_open() {
        let (s, e) = resolve_date_range(None, None, None).unwrap();
        assert_eq!(s, NaiveDate::MIN);
        assert_eq!(e, NaiveDate::MAX);
    }

    #[test]
    fn resolve_date_range_prefers_period() {
        let (s, e) = resolve_date_range(None, None, Some("2026Q1")).unwrap();
        assert_eq!(s, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2026, 3, 31).unwrap());
    }

    #[test]
    fn resolve_date_range_rejects_inverted_range() {
        let err = resolve_date_range(
            NaiveDate::from_ymd_opt(2026, 6, 1),
            NaiveDate::from_ymd_opt(2026, 1, 1),
            None,
        )
        .unwrap_err();
        assert!(err.contains("is after"));
    }
}
