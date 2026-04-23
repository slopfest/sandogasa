// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small shared helpers used by more than one subcommand.

/// GitLab base URL, overridable via
/// `CPU_SIG_TRACKER_GITLAB_BASE` for tests pointing at mock
/// servers. Defaults to the real `https://gitlab.com`.
pub fn gitlab_base() -> String {
    std::env::var("CPU_SIG_TRACKER_GITLAB_BASE")
        .unwrap_or_else(|_| "https://gitlab.com".to_string())
}

/// Red Hat JIRA base URL, overridable via
/// `CPU_SIG_TRACKER_JIRA_BASE` for tests pointing at mock
/// servers. Defaults to the real `https://issues.redhat.com`.
pub fn jira_base() -> String {
    std::env::var("CPU_SIG_TRACKER_JIRA_BASE")
        .unwrap_or_else(|_| "https://issues.redhat.com".to_string())
}

/// Pull the calendar-date portion out of an ISO-8601 timestamp
/// like `"2025-04-04T22:17:50.677Z"`. Returns `None` when the
/// input doesn't begin with a `YYYY-MM-DD` chunk.
pub fn parse_iso_date(ts: &str) -> Option<chrono::NaiveDate> {
    let date_part = ts.split(['T', ' ']).next()?;
    chrono::NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_calendar_date() {
        assert_eq!(
            parse_iso_date("2025-04-04T22:17:50.677Z"),
            chrono::NaiveDate::from_ymd_opt(2025, 4, 4),
        );
        assert_eq!(
            parse_iso_date("2026-04-22 14:05:12"),
            chrono::NaiveDate::from_ymd_opt(2026, 4, 22),
        );
    }

    #[test]
    fn none_on_garbage() {
        assert_eq!(parse_iso_date("not a date"), None);
        assert_eq!(parse_iso_date(""), None);
    }
}
