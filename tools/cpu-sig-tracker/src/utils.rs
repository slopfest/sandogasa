// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Small shared helpers used by more than one subcommand.

/// Outcome of a single precondition check.
pub enum Check {
    Pass(String),
    Fail(String),
    Skipped(String),
}

impl Check {
    pub fn label(&self) -> &'static str {
        match self {
            Check::Pass(_) => "ok",
            Check::Fail(_) => "FAIL",
            Check::Skipped(_) => "skipped",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            Check::Pass(d) | Check::Fail(d) | Check::Skipped(d) => d,
        }
    }
}

/// Print one `check <name>: <label> — <detail>` line.
pub fn report_check(name: &str, check: &Check) {
    println!("check {name}: {} — {}", check.label(), check.detail());
}

/// GitLab base URL, overridable via
/// `CPU_SIG_TRACKER_GITLAB_BASE` for tests pointing at mock
/// servers. Defaults to the real `https://gitlab.com`.
pub fn gitlab_base() -> String {
    std::env::var("CPU_SIG_TRACKER_GITLAB_BASE")
        .unwrap_or_else(|_| "https://gitlab.com".to_string())
}

/// Red Hat JIRA base URL — the Atlassian Cloud site
/// `https://redhat.atlassian.net` (`issues.redhat.com` redirects to
/// it) — overridable via `CPU_SIG_TRACKER_JIRA_BASE` for tests
/// pointing at mock servers. API calls and the links written into
/// tracking issues both use it.
pub fn jira_base() -> String {
    std::env::var("CPU_SIG_TRACKER_JIRA_BASE")
        .unwrap_or_else(|_| "https://redhat.atlassian.net".to_string())
}

/// Find the `RHEL-\d+` key in `- **JIRA**: [KEY](...)`.
pub fn parse_jira_key_from_body(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("- **JIRA**: [")
            && let Some(end) = rest.find(']')
        {
            return Some(rest[..end].to_string());
        }
    }
    None
}

/// Build `- **MR**: [title](url) — state` (suffix omitted when
/// state is unknown).
pub fn format_mr_line(mr_url: &str, mr_title: &str, mr_state: Option<&str>) -> String {
    match mr_state {
        Some(state) => format!("- **MR**: [{mr_title}]({mr_url}) — {state}"),
        None => format!("- **MR**: [{mr_title}]({mr_url})"),
    }
}

/// Build `- **JIRA**: [KEY](url) — status (resolution)` with
/// graceful degradation for missing fields.
pub fn format_jira_line(
    jira_key: Option<&str>,
    jira_status: Option<&str>,
    jira_resolution: Option<&str>,
) -> String {
    let Some(key) = jira_key else {
        return "- **JIRA**: _(not found in MR; set with `--jira`)_".to_string();
    };
    let url = format!("{}/browse/{key}", jira_base());
    let suffix = match canonical_jira_suffix(jira_status, jira_resolution) {
        Some(s) => format!(" — {s}"),
        None => String::new(),
    };
    format!("- **JIRA**: [{key}]({url}){suffix}")
}

/// The `<status>` or `<status> (<resolution>)` text we'd put
/// after ` — ` on the JIRA line. None when no live status.
pub fn canonical_jira_suffix(status: Option<&str>, resolution: Option<&str>) -> Option<String> {
    match (status, resolution) {
        (Some(s), Some(r)) => Some(format!("{s} ({r})")),
        (Some(s), None) => Some(s.to_string()),
        (None, _) => None,
    }
}

/// Pull the calendar-date portion out of an ISO-8601 timestamp
/// like `"2025-04-04T22:17:50.677Z"`. Returns `None` when the
/// input doesn't begin with a `YYYY-MM-DD` chunk.
/// The line recording which stock build carried the fix when a person,
/// not a commit, established it: `- **Stock fix**: <nvr> — verified by
/// <who> on <date>`. `timeline` reads it ahead of any inference.
pub fn format_stock_fix_line(nvr: &str, by: &str, on: chrono::NaiveDate) -> String {
    format!("- **Stock fix**: {nvr} — verified by {by} on {on}")
}

/// The `(nvr, note)` of a body's `- **Stock fix**:` line, the note
/// being whatever follows the dash.
pub fn parse_stock_fix_line(body: &str) -> Option<(String, String)> {
    body.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("- **Stock fix**:")?.trim();
        let (nvr, note) = rest.split_once(" — ").unwrap_or((rest, ""));
        (!nvr.is_empty()).then(|| (nvr.trim().to_string(), note.trim().to_string()))
    })
}

/// `body` with its `- **Stock fix**:` line replaced by `line`, or
/// `line` appended after the metadata when there is none.
pub fn with_stock_fix_line(body: &str, line: &str) -> String {
    if parse_stock_fix_line(body).is_some() {
        return body
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("- **Stock fix**:") {
                    line.to_string()
                } else {
                    l.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
    }
    format!("{}\n{line}\n", body.trim_end())
}

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

    #[test]
    fn parse_jira_key_from_standard_body() {
        let body = "- **JIRA**: [RHEL-1](https://example/) — Closed (Done)\n";
        assert_eq!(parse_jira_key_from_body(body).as_deref(), Some("RHEL-1"));
    }

    #[test]
    fn parse_jira_key_returns_none_when_missing() {
        assert_eq!(parse_jira_key_from_body("no jira"), None);
    }

    #[test]
    fn stock_fix_line_round_trips_and_replaces_in_place() {
        let on = chrono::NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
        let line = format_stock_fix_line("mutter-47.5-10.el10", "salimma", on);
        assert_eq!(
            parse_stock_fix_line(&line),
            Some((
                "mutter-47.5-10.el10".to_string(),
                "verified by salimma on 2026-09-24".to_string()
            ))
        );
        let body = "- **MR**: [t](https://x) — opened\n- **Release**: c10s\n";
        let once = with_stock_fix_line(body, &line);
        assert!(once.ends_with("- **Release**: c10s\n- **Stock fix**: mutter-47.5-10.el10 — verified by salimma on 2026-09-24\n"));
        let again = with_stock_fix_line(
            &once,
            &format_stock_fix_line("mutter-47.5-11.el10", "salimma", on),
        );
        assert_eq!(again.matches("- **Stock fix**:").count(), 1);
        assert!(again.contains("mutter-47.5-11.el10") && !again.contains("47.5-10"));
        assert_eq!(parse_stock_fix_line(body), None);
    }
}
