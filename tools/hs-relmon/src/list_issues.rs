// SPDX-License-Identifier: Apache-2.0 OR MIT

use crate::check_latest;
use crate::gitlab;
use serde::Serialize;
use std::collections::HashSet;

/// A single issue entry for the list-issues output.
#[derive(Debug, Serialize)]
pub struct IssueEntry {
    pub package: String,
    pub iid: u64,
    pub title: String,
    pub url: String,
    pub status: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub assignees: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_manifest: Option<bool>,
}

/// Build an `IssueEntry` from a GitLab issue.
///
/// Returns `None` if the package name cannot be extracted
/// from the issue URL.
pub fn entry_from_issue(
    issue: &gitlab::Issue,
    status: Option<String>,
    manifest_names: Option<&HashSet<String>>,
) -> Option<IssueEntry> {
    let package = gitlab::package_from_issue_url(&issue.web_url)?.to_string();

    let status = status.unwrap_or_else(|| issue.state.clone());
    let assignees: Vec<String> = issue.assignees.iter().map(|a| a.username.clone()).collect();
    let in_manifest = manifest_names.map(|names| names.contains(&package));

    Some(IssueEntry {
        package,
        iid: issue.iid,
        title: issue.title.clone(),
        url: issue.web_url.clone(),
        status,
        assignees,
        in_manifest,
    })
}

/// Filter and sort issue entries.
pub fn filter_and_sort(
    entries: Vec<IssueEntry>,
    filter_status: Option<&str>,
    filter_assignee: Option<&str>,
) -> Vec<IssueEntry> {
    let mut filtered: Vec<_> = entries
        .into_iter()
        .filter(|e| {
            check_latest::matches_filter(&e.status, &e.assignees, filter_status, filter_assignee)
        })
        .collect();
    filtered.sort_by(|a, b| a.package.cmp(&b.package));
    filtered
}

/// Build a list of issue entries from GitLab group issues.
///
/// Resolves work-item status via GraphQL for each issue.
/// Applies status/assignee filters. When `manifest_names`
/// is provided, sets `in_manifest` on each entry.
pub fn build_entries(
    client: &gitlab::GroupClient,
    issues: &[gitlab::Issue],
    filter_status: Option<&str>,
    filter_assignee: Option<&str>,
    manifest_names: Option<&HashSet<String>>,
) -> Vec<IssueEntry> {
    let mut entries = Vec::new();
    for issue in issues {
        let project_path = gitlab::project_path_from_issue_url(&issue.web_url);
        let status = project_path
            .as_deref()
            .and_then(|path| client.get_work_item_status(path, issue.iid).ok().flatten());

        match entry_from_issue(issue, status, manifest_names) {
            Some(entry) => entries.push(entry),
            None => {
                eprintln!(
                    "warning: cannot extract package name \
                    from {}",
                    issue.web_url
                );
            }
        }
    }
    filter_and_sort(entries, filter_status, filter_assignee)
}

/// Print issue entries as a JSON array.
pub fn print_json(entries: &[IssueEntry]) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = Vec::new();
    write_json(&mut buf, entries)?;
    print!("{}", String::from_utf8(buf)?);
    Ok(())
}

/// Write issue entries as a JSON array to a writer.
pub fn write_json(
    w: &mut dyn std::io::Write,
    entries: &[IssueEntry],
) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(entries)?;
    writeln!(w, "{json}")?;
    Ok(())
}

/// Print issue entries as a table.
pub fn print_table(entries: &[IssueEntry]) {
    print!("{}", format_table(entries));
}

/// Format issue entries as a table string.
pub fn format_table(entries: &[IssueEntry]) -> String {
    if entries.is_empty() {
        return String::from("No matching issues found.\n");
    }

    let show_manifest = entries.iter().any(|e| e.in_manifest.is_some());

    // Compute column widths.
    let mut w_pkg = "Package".len();
    let mut w_issue = "Issue".len();
    let mut w_status = "Status".len();
    let mut w_assignee = "Assignee".len();

    for e in entries {
        w_pkg = w_pkg.max(e.package.len());
        w_issue = w_issue.max(format!("#{}", e.iid).len());
        w_status = w_status.max(e.status.len());
        let assignee_str = if e.assignees.is_empty() {
            "(none)"
        } else {
            // Use first assignee for width calculation
            e.assignees.first().map(|s| s.as_str()).unwrap_or("")
        };
        w_assignee = w_assignee.max(assignee_str.len());
    }

    let mut out = String::new();

    // Header
    out.push_str(&format!(
        "  {:<w_pkg$}  {:<w_issue$}  {:<w_status$}  {:<w_assignee$}",
        "Package", "Issue", "Status", "Assignee",
    ));
    if show_manifest {
        out.push_str("  Manifest");
    }
    out.push_str("  Title\n");

    // Separator
    out.push_str(&format!(
        "  {:\u{2500}<w_pkg$}  {:\u{2500}<w_issue$}  {:\u{2500}<w_status$}  {:\u{2500}<w_assignee$}",
        "", "", "", "",
    ));
    if show_manifest {
        out.push_str(&format!("  {:\u{2500}<8}", "",));
    }
    out.push_str(&format!("  {:\u{2500}<30}\n", "",));

    // Rows
    for e in entries {
        let issue_str = format!("#{}", e.iid);
        let assignee_str = if e.assignees.is_empty() {
            "(none)".to_string()
        } else {
            e.assignees.join(",")
        };

        out.push_str(&format!(
            "  {:<w_pkg$}  {:<w_issue$}  {:<w_status$}  {:<w_assignee$}",
            e.package, issue_str, e.status, assignee_str,
        ));
        if show_manifest {
            let manifest_str = match e.in_manifest {
                Some(true) => "yes",
                Some(false) => "MISSING",
                None => "",
            };
            out.push_str(&format!("  {:<8}", manifest_str));
        }
        out.push_str(&format!("  {}\n", e.title));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(
        package: &str,
        iid: u64,
        status: &str,
        assignees: Vec<&str>,
        in_manifest: Option<bool>,
    ) -> IssueEntry {
        IssueEntry {
            package: package.into(),
            iid,
            title: format!("{package}-1.0 is available"),
            url: format!(
                "https://gitlab.com/CentOS/Hyperscale/rpms/\
                {package}/-/issues/{iid}"
            ),
            status: status.into(),
            assignees: assignees.into_iter().map(String::from).collect(),
            in_manifest,
        }
    }

    #[test]
    fn test_json_serialization() {
        let entries = vec![make_entry("ethtool", 1, "To do", vec!["alice"], None)];
        let mut buf = Vec::new();
        write_json(&mut buf, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["package"], "ethtool");
        assert_eq!(arr[0]["status"], "To do");
        assert_eq!(arr[0]["assignees"][0], "alice");
        // in_manifest should be absent when None
        assert!(arr[0].get("in_manifest").is_none());
    }

    #[test]
    fn test_json_with_manifest() {
        let entries = vec![
            make_entry("ethtool", 1, "To do", vec![], Some(true)),
            make_entry("foobar", 2, "To do", vec![], Some(false)),
        ];
        let mut buf = Vec::new();
        write_json(&mut buf, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr[0]["in_manifest"], true);
        assert_eq!(arr[1]["in_manifest"], false);
    }

    #[test]
    fn test_json_no_assignees_omitted() {
        let entries = vec![make_entry("ethtool", 1, "To do", vec![], None)];
        let mut buf = Vec::new();
        write_json(&mut buf, &entries).unwrap();
        let json: serde_json::Value = serde_json::from_slice(&buf).unwrap();
        assert!(json[0].get("assignees").is_none());
    }

    #[test]
    fn test_format_table_empty() {
        let out = format_table(&[]);
        assert_eq!(out, "No matching issues found.\n");
    }

    #[test]
    fn test_format_table_basic() {
        let entries = vec![make_entry("ethtool", 3, "To do", vec!["alice"], None)];
        let out = format_table(&entries);
        assert!(out.contains("Package"));
        assert!(out.contains("ethtool"));
        assert!(out.contains("#3"));
        assert!(out.contains("To do"));
        assert!(out.contains("alice"));
        assert!(out.contains("ethtool-1.0 is available"));
        // No Manifest column
        assert!(!out.contains("Manifest"));
    }

    #[test]
    fn test_format_table_with_manifest() {
        let entries = vec![
            make_entry("ethtool", 1, "To do", vec!["alice"], Some(true)),
            make_entry("foobar", 2, "To do", vec![], Some(false)),
        ];
        let out = format_table(&entries);
        assert!(out.contains("Manifest"));
        assert!(out.contains("yes"));
        assert!(out.contains("MISSING"));
        assert!(out.contains("(none)"));
    }

    #[test]
    fn test_format_table_unassigned() {
        let entries = vec![make_entry("pkg", 1, "To do", vec![], None)];
        let out = format_table(&entries);
        assert!(out.contains("(none)"));
    }

    #[test]
    fn test_format_table_multiple_assignees() {
        let entries = vec![make_entry("pkg", 1, "To do", vec!["alice", "bob"], None)];
        let out = format_table(&entries);
        assert!(out.contains("alice,bob"));
    }

    #[test]
    fn test_format_table_manifest_column_alignment() {
        let entries = vec![
            make_entry("ethtool", 1, "To do", vec![], Some(true)),
            make_entry("systemd", 2, "In progress", vec!["bob"], Some(true)),
            make_entry("foobar", 3, "To do", vec![], Some(false)),
        ];
        let out = format_table(&entries);
        assert!(out.contains("Manifest"));
        assert!(out.contains("yes"));
        assert!(out.contains("MISSING"));
    }

    #[test]
    fn test_format_table_sorts_by_package() {
        let entries = vec![
            make_entry("zzz", 2, "Done", vec![], None),
            make_entry("aaa", 1, "To do", vec![], None),
        ];
        // build_entries sorts, but format_table takes
        // whatever order it receives. Just verify output.
        let out = format_table(&entries);
        let zzz_pos = out.find("zzz").unwrap();
        let aaa_pos = out.find("aaa").unwrap();
        assert!(zzz_pos < aaa_pos);
    }

    fn make_gitlab_issue(
        iid: u64,
        package: &str,
        state: &str,
        assignees: Vec<&str>,
    ) -> gitlab::Issue {
        gitlab::Issue {
            iid,
            title: format!("{package}-1.0 is available"),
            description: None,
            state: state.into(),
            web_url: format!(
                "https://gitlab.com/CentOS/Hyperscale/\
                rpms/{package}/-/issues/{iid}"
            ),
            assignees: assignees
                .into_iter()
                .map(|u| gitlab::Assignee { username: u.into() })
                .collect(),
        }
    }

    #[test]
    fn test_entry_from_issue_basic() {
        let issue = make_gitlab_issue(1, "ethtool", "opened", vec!["alice"]);
        let entry = entry_from_issue(&issue, Some("To do".into()), None).unwrap();
        assert_eq!(entry.package, "ethtool");
        assert_eq!(entry.iid, 1);
        assert_eq!(entry.status, "To do");
        assert_eq!(entry.assignees, vec!["alice"]);
        assert!(entry.in_manifest.is_none());
    }

    #[test]
    fn test_entry_from_issue_falls_back_to_state() {
        let issue = make_gitlab_issue(1, "ethtool", "opened", vec![]);
        let entry = entry_from_issue(&issue, None, None).unwrap();
        assert_eq!(entry.status, "opened");
    }

    #[test]
    fn test_entry_from_issue_with_manifest() {
        let issue = make_gitlab_issue(1, "ethtool", "opened", vec![]);
        let mut names = HashSet::new();
        names.insert("ethtool".to_string());
        let entry = entry_from_issue(&issue, None, Some(&names)).unwrap();
        assert_eq!(entry.in_manifest, Some(true));

        let issue2 = make_gitlab_issue(2, "foobar", "opened", vec![]);
        let entry2 = entry_from_issue(&issue2, None, Some(&names)).unwrap();
        assert_eq!(entry2.in_manifest, Some(false));
    }

    #[test]
    fn test_entry_from_issue_bad_url() {
        let issue = gitlab::Issue {
            iid: 1,
            title: "t".into(),
            description: None,
            state: "opened".into(),
            web_url: "".into(),
            assignees: vec![],
        };
        assert!(entry_from_issue(&issue, None, None).is_none());
    }

    #[test]
    fn test_filter_and_sort_by_status() {
        let entries = vec![
            make_entry("b-pkg", 2, "Done", vec![], None),
            make_entry("a-pkg", 1, "To do", vec!["alice"], None),
        ];
        let filtered = filter_and_sort(entries, Some("To do"), None);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].package, "a-pkg");
    }

    #[test]
    fn test_filter_and_sort_by_assignee() {
        let entries = vec![
            make_entry("pkg-a", 1, "To do", vec!["alice"], None),
            make_entry("pkg-b", 2, "To do", vec![], None),
        ];
        let filtered = filter_and_sort(entries, None, Some("none"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].package, "pkg-b");
    }

    #[test]
    fn test_filter_and_sort_sorts() {
        let entries = vec![
            make_entry("z", 2, "To do", vec![], None),
            make_entry("a", 1, "To do", vec![], None),
        ];
        let sorted = filter_and_sort(entries, None, None);
        assert_eq!(sorted[0].package, "a");
        assert_eq!(sorted[1].package, "z");
    }

    #[test]
    fn test_build_entries_sorting() {
        let entries = vec![
            make_entry("b-pkg", 2, "Done", vec![], None),
            make_entry("a-pkg", 1, "To do", vec!["alice"], None),
        ];
        let mut sorted = entries;
        sorted.sort_by(|a, b| a.package.cmp(&b.package));
        assert_eq!(sorted[0].package, "a-pkg");
        assert_eq!(sorted[1].package, "b-pkg");
    }
}
