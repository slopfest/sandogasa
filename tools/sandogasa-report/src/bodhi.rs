// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bodhi activity reporting — updates pushed, per-release breakdown.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use serde::Serialize;

use crate::config::DomainConfig;

/// Bodhi activity report.
#[derive(Debug, Serialize)]
pub struct BodhiReport {
    /// Total updates in the period.
    pub total_updates: usize,
    /// Total builds across all updates.
    pub total_builds: usize,
    /// State summary.
    pub submitted: usize,
    pub pushed_to_testing: usize,
    pub pushed_to_stable: usize,
    /// Per-release breakdown: release name → list of updates.
    pub by_release: BTreeMap<String, Vec<UpdateEntry>>,
}

/// A single Bodhi update entry.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateEntry {
    pub alias: String,
    pub status: String,
    /// Short human-readable summary, resolved once at fetch time
    /// from `display_name` (if set), then the first non-empty
    /// line of `notes`, else the single build NVR (if exactly
    /// one build), else `None`. Used by the condensed detail
    /// level so a 20-build update renders as `summary (20 builds)`
    /// instead of a wall of NVRs.
    pub summary: Option<String>,
    pub builds: Vec<String>,
    pub date_submitted: Option<String>,
    pub date_testing: Option<String>,
    pub date_stable: Option<String>,
}

/// Resolve the best short human-readable summary for an update,
/// preferring Bodhi's `display_name`, then the first non-empty
/// line of `notes`, then the single build NVR if the update has
/// exactly one build. Returns `None` when nothing usable exists
/// (the formatter will fall back to the bare "N builds" count).
fn derive_summary(u: &sandogasa_bodhi::models::Update) -> Option<String> {
    if let Some(name) = u.display_name.as_deref()
        && !name.trim().is_empty()
    {
        return Some(name.trim().to_string());
    }
    if let Some(notes) = u.notes.as_deref()
        && let Some(line) = notes.lines().map(str::trim).find(|l| !l.is_empty())
    {
        // Strip leading markdown heading hashes that sometimes
        // appear in notes.
        let cleaned = line.trim_start_matches('#').trim();
        if !cleaned.is_empty() {
            return Some(cleaned.to_string());
        }
    }
    if u.builds.len() == 1 {
        return Some(u.builds[0].nvr.clone());
    }
    None
}

/// Parse a Bodhi date string like "2026-02-25 11:55:26" into a NaiveDate.
fn parse_bodhi_date(date_str: &str) -> Option<NaiveDate> {
    // Bodhi dates are "YYYY-MM-DD HH:MM:SS" — just take the date part.
    let date_part = date_str.split_whitespace().next()?;
    NaiveDate::parse_from_str(date_part, "%Y-%m-%d").ok()
}

/// Check if a release name matches any of the configured patterns.
fn matches_release(name: &str, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return true;
    }
    patterns.iter().any(|pat| {
        if let Some(prefix) = pat.strip_suffix('*') {
            name.starts_with(prefix)
        } else {
            name == pat
        }
    })
}

/// Run Bodhi reporting for a user and domain.
pub async fn bodhi_report(
    username: &str,
    domain: &DomainConfig,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BodhiReport, String> {
    bodhi_report_with_client(
        &sandogasa_bodhi::BodhiClient::new(),
        username,
        domain,
        since,
        until,
        verbose,
    )
    .await
}

async fn bodhi_report_with_client(
    client: &sandogasa_bodhi::BodhiClient,
    username: &str,
    domain: &DomainConfig,
    since: NaiveDate,
    until: NaiveDate,
    verbose: bool,
) -> Result<BodhiReport, String> {
    if verbose {
        eprintln!("[bodhi] fetching updates for {username}");
    }

    // Bodhi filters by submission date server-side, so we pass
    // our report window plus a small buffer to catch updates
    // submitted just before `since` that still pushed within it.
    // `submitted_before` is widened past `until` so the endpoint
    // includes the boundary day. The client then re-checks each
    // update's submitted/testing/stable dates against the exact
    // window.
    let submitted_since = since - chrono::Duration::days(30);
    let submitted_before = until + chrono::Duration::days(1);
    let updates = client
        .updates_for_user(
            username,
            500,
            Some(submitted_since),
            Some(submitted_before),
            |page, total_pages, count| {
                if verbose {
                    eprintln!("[bodhi] fetched page {page}/{total_pages} ({count} updates so far)");
                }
            },
        )
        .await
        .map_err(|e| format!("Bodhi query failed: {e}"))?;

    let in_period = |date_str: Option<&str>| -> bool {
        date_str
            .and_then(parse_bodhi_date)
            .is_some_and(|d| d >= since && d <= until)
    };

    let mut by_release: BTreeMap<String, Vec<UpdateEntry>> = BTreeMap::new();
    let mut total_updates = 0;
    let mut total_builds = 0;
    let mut submitted = 0;
    let mut pushed_to_testing = 0;
    let mut pushed_to_stable = 0;

    for update in &updates {
        let submitted_date = update.date_submitted.as_deref().and_then(parse_bodhi_date);

        // An update is relevant if any event happened in the period.
        let was_submitted = in_period(update.date_submitted.as_deref());
        let was_tested = in_period(update.date_testing.as_deref());
        let was_stabled = in_period(update.date_stable.as_deref());

        if !was_submitted && !was_tested && !was_stabled {
            // If the submission is before our period and no state
            // changes happened in the period, skip. If submission
            // is after, keep scanning (Bodhi sorts by submitted).
            if let Some(d) = submitted_date
                && d < since
            {
                break;
            }
            continue;
        }

        // Filter by release pattern.
        let release_name = update
            .release
            .as_ref()
            .map(|r| r.name.clone())
            .unwrap_or_else(|| "unknown".to_string());

        if !matches_release(&release_name, &domain.bodhi_releases) {
            continue;
        }

        if was_submitted {
            submitted += 1;
        }
        if was_tested {
            pushed_to_testing += 1;
        }
        if was_stabled {
            pushed_to_stable += 1;
        }

        let entry = UpdateEntry {
            alias: update.alias.clone(),
            status: update.status.clone(),
            summary: derive_summary(update),
            builds: update.builds.iter().map(|b| b.nvr.clone()).collect(),
            date_submitted: update.date_submitted.clone(),
            date_testing: update.date_testing.clone(),
            date_stable: update.date_stable.clone(),
        };

        total_builds += entry.builds.len();
        total_updates += 1;
        by_release.entry(release_name).or_default().push(entry);
    }

    if verbose {
        eprintln!(
            "[bodhi] {total_updates} update(s), {total_builds} build(s) \
             ({submitted} submitted, {pushed_to_testing} tested, \
             {pushed_to_stable} stable)"
        );
    }

    Ok(BodhiReport {
        total_updates,
        total_builds,
        submitted,
        pushed_to_testing,
        pushed_to_stable,
        by_release,
    })
}

/// Format the Bodhi report as Markdown.
/// Extract a numeric sort key from a release name.
///
/// "F45" → (45, 0), "EPEL-10.3" → (10, 3), "EPEL-9" → (9, 0).
/// Sorts by major version first, then minor, both descending.
fn release_sort_key(name: &str) -> (u32, u32) {
    let version_str: String = name
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let mut parts = version_str.splitn(2, '.');
    let major = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    (major, minor)
}

/// Sort release names by version number, newest first.
fn sorted_releases(by_release: &BTreeMap<String, Vec<UpdateEntry>>) -> Vec<&str> {
    let mut releases: Vec<&str> = by_release.keys().map(|s| s.as_str()).collect();
    releases.sort_by_key(|r| std::cmp::Reverse(release_sort_key(r)));
    releases
}

/// Render the Bodhi section.
///
/// - `detail == 0` — summary only (totals, per-release counts)
/// - `detail == 1` — list each update as
///   `[alias](url) (status, date)` with the Bodhi-provided title
///   indented below as `Title (N builds)`. Keeps the line
///   compact even when an update bundles 20+ sub-packages.
/// - `detail >= 2` — same as level 1 plus, for multi-build
///   updates only, list every build NVR as indented sub-bullets.
pub fn format_markdown(report: &BodhiReport, detail: u8) -> String {
    let mut out = String::new();

    out.push_str("## Bodhi\n\n");

    out.push_str(&format!(
        "**{}** update(s) with **{}** build(s) across **{}** release(s).\n\n",
        report.total_updates,
        report.total_builds,
        report.by_release.len()
    ));

    out.push_str(&format!(
        "- **{}** submitted, **{}** pushed to testing, **{}** pushed to stable\n\n",
        report.submitted, report.pushed_to_testing, report.pushed_to_stable
    ));

    let releases = sorted_releases(&report.by_release);

    // Per-release summary.
    for release in &releases {
        let updates = &report.by_release[*release];
        let builds: usize = updates.iter().map(|u| u.builds.len()).sum();
        out.push_str(&format!(
            "- **{release}**: {} update(s), {builds} build(s)\n",
            updates.len()
        ));
    }
    out.push('\n');

    if detail == 0 {
        return out;
    }

    for release in &releases {
        let updates = &report.by_release[*release];
        out.push_str(&format!("### {release}\n\n"));
        for u in updates {
            let date = u
                .date_submitted
                .as_deref()
                .and_then(|d| d.split_whitespace().next())
                .unwrap_or("?");
            out.push_str(&format!(
                "- [{}](https://bodhi.fedoraproject.org/updates/{}) \
                 ({}, {date})\n",
                u.alias, u.alias, u.status
            ));
            // Always show the human summary + build count on an
            // indented line. Singular/plural differ for 1 vs >1
            // builds. Missing summaries fall back to just the count.
            let n = u.builds.len();
            let unit = if n == 1 { "build" } else { "builds" };
            match u.summary.as_deref() {
                Some(s) => out.push_str(&format!("  {s} ({n} {unit})\n")),
                None => out.push_str(&format!("  {n} {unit}\n")),
            }
            // List build NVRs as indented sub-bullets when either:
            //   - detail >= 2 (always list everything)
            //   - there's exactly 1 build (the summary may be a
            //     human phrase like "Initial release" that doesn't
            //     identify the package, so the one NVR is helpful
            //     at level 1 too)
            if detail >= 2 || n == 1 {
                for nvr in &u.builds {
                    out.push_str(&format!("  - {nvr}\n"));
                }
            }
        }
        out.push('\n');
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path_regex};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn parse_bodhi_date_valid() {
        let d = parse_bodhi_date("2026-02-25 11:55:26").unwrap();
        assert_eq!(d, NaiveDate::from_ymd_opt(2026, 2, 25).unwrap());
    }

    #[test]
    fn parse_bodhi_date_invalid() {
        assert!(parse_bodhi_date("not-a-date").is_none());
    }

    #[test]
    fn matches_release_wildcard() {
        assert!(matches_release("F42", &["F*".to_string()]));
        assert!(matches_release("EPEL-9", &["EPEL-*".to_string()]));
        assert!(!matches_release("F42", &["EPEL-*".to_string()]));
    }

    #[test]
    fn matches_release_exact() {
        assert!(matches_release("F42", &["F42".to_string()]));
        assert!(!matches_release("F43", &["F42".to_string()]));
    }

    #[test]
    fn matches_release_empty_matches_all() {
        assert!(matches_release("anything", &[]));
    }

    #[test]
    fn sort_key_fedora() {
        assert_eq!(release_sort_key("F45"), (45, 0));
        assert_eq!(release_sort_key("F9"), (9, 0));
    }

    #[test]
    fn sort_key_epel() {
        assert_eq!(release_sort_key("EPEL-10"), (10, 0));
        assert_eq!(release_sort_key("EPEL-10.3"), (10, 3));
        assert_eq!(release_sort_key("EPEL-9"), (9, 0));
    }

    #[test]
    fn sort_order_newest_first() {
        let mut map = BTreeMap::new();
        map.insert("F42".to_string(), vec![]);
        map.insert("EPEL-10.1".to_string(), vec![]);
        map.insert("EPEL-10.3".to_string(), vec![]);
        map.insert("F45".to_string(), vec![]);
        map.insert("EPEL-9".to_string(), vec![]);
        let sorted = sorted_releases(&map);
        assert_eq!(
            sorted,
            vec!["F45", "F42", "EPEL-10.3", "EPEL-10.1", "EPEL-9"]
        );
    }

    fn make_report() -> BodhiReport {
        let mut by_release = BTreeMap::new();
        by_release.insert(
            "F45".to_string(),
            vec![
                UpdateEntry {
                    alias: "FEDORA-2026-abc123".to_string(),
                    status: "stable".to_string(),
                    summary: Some("foo-1.0-1.fc45".to_string()),
                    builds: vec!["foo-1.0-1.fc45".to_string()],
                    date_submitted: Some("2026-01-15 10:00:00".to_string()),
                    date_testing: Some("2026-01-15 12:00:00".to_string()),
                    date_stable: Some("2026-01-16 10:00:00".to_string()),
                },
                UpdateEntry {
                    alias: "FEDORA-2026-def456".to_string(),
                    status: "testing".to_string(),
                    summary: Some("Latest bar crates".to_string()),
                    builds: vec![
                        "bar-2.0-1.fc45".to_string(),
                        "bar-extra-2.0-1.fc45".to_string(),
                    ],
                    date_submitted: Some("2026-02-01 09:00:00".to_string()),
                    date_testing: Some("2026-02-01 11:00:00".to_string()),
                    date_stable: None,
                },
            ],
        );
        by_release.insert(
            "EPEL-9".to_string(),
            vec![UpdateEntry {
                alias: "FEDORA-EPEL-2026-ghi789".to_string(),
                status: "stable".to_string(),
                summary: Some("baz-3.0-1.el9".to_string()),
                builds: vec!["baz-3.0-1.el9".to_string()],
                date_submitted: Some("2026-01-20 08:00:00".to_string()),
                date_testing: None,
                date_stable: Some("2026-01-21 08:00:00".to_string()),
            }],
        );
        BodhiReport {
            total_updates: 3,
            total_builds: 4,
            submitted: 3,
            pushed_to_testing: 2,
            pushed_to_stable: 2,
            by_release,
        }
    }

    #[test]
    fn format_summary() {
        let report = make_report();
        let md = format_markdown(&report, 0);
        assert!(md.contains("**3** update(s)"));
        assert!(md.contains("**4** build(s)"));
        assert!(md.contains("**3** submitted"));
        assert!(md.contains("**2** pushed to stable"));
        // Newest first.
        let f45_pos = md.find("**F45**").unwrap();
        let epel9_pos = md.find("**EPEL-9**").unwrap();
        assert!(f45_pos < epel9_pos);
    }

    #[test]
    fn format_detailed_level_1_single_build_still_shown() {
        let report = make_report();
        let md = format_markdown(&report, 1);
        // Summary indented on its own line with the build count.
        assert!(md.contains("\n  foo-1.0-1.fc45 (1 build)\n"));
        assert!(md.contains("\n  Latest bar crates (2 builds)\n"));
        // Single-build update gets its NVR listed — summary may
        // be a human phrase that doesn't identify the package.
        assert!(md.contains("  - foo-1.0-1.fc45\n"));
        // Multi-build update does NOT get the expanded list at
        // level 1.
        assert!(!md.contains("  - bar-2.0-1.fc45\n"));
        assert!(!md.contains("  - bar-extra-2.0-1.fc45\n"));
    }

    #[test]
    fn format_detailed_level_2_lists_every_build() {
        let report = make_report();
        let md = format_markdown(&report, 2);
        assert!(md.contains("\n  Latest bar crates (2 builds)\n"));
        assert!(md.contains("  - foo-1.0-1.fc45\n"));
        assert!(md.contains("  - bar-2.0-1.fc45\n"));
        assert!(md.contains("  - bar-extra-2.0-1.fc45\n"));
    }

    #[test]
    fn derive_summary_prefers_display_name() {
        let mut u: sandogasa_bodhi::models::Update = serde_json::from_str(
            r#"{"alias":"a","status":"stable","display_name":"Latest selinux crates",
                "notes":"Some notes","builds":[{"nvr":"foo-1-1"},{"nvr":"bar-1-1"}]}"#,
        )
        .unwrap();
        assert_eq!(derive_summary(&u).as_deref(), Some("Latest selinux crates"));
        // When display_name is absent the first non-empty notes
        // line wins.
        u.display_name = Some(String::new());
        assert_eq!(derive_summary(&u).as_deref(), Some("Some notes"));
        // With everything empty and a single build, fall back to
        // the NVR so the output isn't just "1 build".
        u.notes = None;
        u.builds = vec![sandogasa_bodhi::models::Build {
            nvr: "solo-1-1".to_string(),
        }];
        assert_eq!(derive_summary(&u).as_deref(), Some("solo-1-1"));
        // Multi-build with no display_name/notes → no summary.
        u.builds = vec![
            sandogasa_bodhi::models::Build {
                nvr: "a-1-1".into(),
            },
            sandogasa_bodhi::models::Build {
                nvr: "b-1-1".into(),
            },
        ];
        assert!(derive_summary(&u).is_none());
    }

    #[test]
    fn derive_summary_strips_markdown_heading() {
        // Regular (non-raw) string so \n expands; JSON's " gets
        // escaped with \".
        let u: sandogasa_bodhi::models::Update = serde_json::from_str(
            "{\"alias\":\"a\",\"status\":\"stable\",\
             \"notes\":\"# Big fix\\n\\nDetails...\",\
             \"builds\":[{\"nvr\":\"x-1-1\"},{\"nvr\":\"y-1-1\"}]}",
        )
        .unwrap();
        assert_eq!(derive_summary(&u).as_deref(), Some("Big fix"));
    }

    #[tokio::test]
    async fn bodhi_report_filters_by_date_and_release() {
        let server = MockServer::start().await;
        let client = sandogasa_bodhi::BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-in-range",
                        "status": "stable",
                        "builds": [{"nvr": "foo-1.0-1.fc45"}],
                        "release": {"name": "F45"},
                        "date_submitted": "2026-02-15 10:00:00",
                        "date_stable": "2026-02-16 10:00:00"
                    },
                    {
                        "alias": "FEDORA-2026-too-new",
                        "status": "pending",
                        "builds": [{"nvr": "bar-2.0-1.fc45"}],
                        "release": {"name": "F45"},
                        "date_submitted": "2026-04-01 10:00:00"
                    },
                    {
                        "alias": "FEDORA-2026-too-old",
                        "status": "stable",
                        "builds": [{"nvr": "baz-3.0-1.fc45"}],
                        "release": {"name": "F45"},
                        "date_submitted": "2025-12-01 10:00:00",
                        "date_stable": "2025-12-02 10:00:00"
                    },
                    {
                        "alias": "EPEL-2026-wrong-release",
                        "status": "stable",
                        "builds": [{"nvr": "qux-1.0-1.el9"}],
                        "release": {"name": "EPEL-9"},
                        "date_submitted": "2026-02-20 10:00:00"
                    }
                ],
                "total": 4,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let domain = DomainConfig {
            bodhi: true,
            bodhi_releases: vec!["F*".to_string()],
            ..Default::default()
        };

        let report = bodhi_report_with_client(
            &client,
            "testuser",
            &domain,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            false,
        )
        .await
        .unwrap();

        assert_eq!(report.total_updates, 1);
        assert_eq!(report.submitted, 1);
        assert_eq!(report.pushed_to_stable, 1);
        assert!(report.by_release.contains_key("F45"));
        assert!(!report.by_release.contains_key("EPEL-9"));
    }

    #[tokio::test]
    async fn bodhi_report_includes_state_change_in_period() {
        let server = MockServer::start().await;
        let client = sandogasa_bodhi::BodhiClient::with_base_url(&server.uri());

        Mock::given(method("GET"))
            .and(path_regex("/updates/.*"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "updates": [
                    {
                        "alias": "FEDORA-2026-stabled-in-q1",
                        "status": "stable",
                        "builds": [{"nvr": "old-pkg-1.0-1.fc45"}],
                        "release": {"name": "F45"},
                        "date_submitted": "2025-12-15 10:00:00",
                        "date_testing": "2025-12-16 10:00:00",
                        "date_stable": "2026-01-05 10:00:00"
                    }
                ],
                "total": 1,
                "page": 1,
                "pages": 1
            })))
            .expect(1)
            .mount(&server)
            .await;

        let domain = DomainConfig {
            bodhi: true,
            ..Default::default()
        };

        let report = bodhi_report_with_client(
            &client,
            "testuser",
            &domain,
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 3, 31).unwrap(),
            false,
        )
        .await
        .unwrap();

        // Submitted before Q1, but pushed to stable during Q1.
        assert_eq!(report.total_updates, 1);
        assert_eq!(report.submitted, 0);
        assert_eq!(report.pushed_to_stable, 1);
    }
}
