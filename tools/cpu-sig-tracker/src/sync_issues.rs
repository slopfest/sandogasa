// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `sync-issues` subcommand.
//!
//! For each (release, package) pair in the inventory, check
//! whether a tracking issue exists in either the per-package
//! project (`CentOS/proposed_updates/rpms/<pkg>`, the active
//! state once an MR is filed) or the central tracker
//! (`CentOS/proposed_updates/package_tracker`, for proposed-only
//! items without an MR yet). Classifies each pair as
//! `active`, `proposed`, or `missing` and reports a per-release
//! summary. Read-only — filing missing issues will come in a
//! later iteration.

use std::collections::BTreeMap;
use std::process::ExitCode;

use crate::gitlab;

/// GitLab group containing the per-package tracking projects.
const PROPOSED_UPDATES_GROUP: &str = "CentOS/proposed_updates/rpms";

/// Single-project "proposed items" tracker used for packages
/// that don't have a dedicated MR yet.
const PACKAGE_TRACKER_PROJECT: &str = "CentOS/proposed_updates/package_tracker";

/// Label applied to all tool-filed tracking issues.
const TRACKING_LABEL: &str = "cpu-sig-tracker";

/// Hardcoded since all cpu-sig-tracker flows go through gitlab.com.
use crate::utils::gitlab_base;

#[derive(clap::Args)]
pub struct SyncIssuesArgs {
    /// Path to the sandogasa-inventory TOML file.
    #[arg(short, long, default_value = "inventory.toml")]
    pub inventory: String,

    /// Restrict the check to a single release (e.g. `c10s`). If
    /// omitted, every workload in the inventory is checked.
    #[arg(long)]
    pub release: Option<String>,

    /// Label hand-filed tracking issues `cpu-sig-tracker` without
    /// asking, so every command tracks them
    #[arg(long)]
    pub adopt: bool,

    /// Emit a machine-readable JSON array instead of grouped text.
    #[arg(long)]
    pub json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    pub verbose: bool,
}

pub fn run(args: &SyncIssuesArgs) -> ExitCode {
    match build_rows(args) {
        Ok(rows) => {
            if args.json {
                match serde_json::to_string_pretty(&rows) {
                    Ok(j) => println!("{j}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                print_human(&rows);
            }
            if let Err(e) = adopt(&rows, args) {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Label the hand-filed tracking issues `cpu-sig-tracker`: with
/// `--adopt` outright, otherwise after asking — and only at a
/// terminal, since an unattended run must not relabel unasked. An
/// issue filed by a person drifts from the tool's conventions by
/// nature; the label is what brings it back into every command's
/// view.
fn adopt(rows: &[Row], args: &SyncIssuesArgs) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::IsTerminal;
    let hand_filed: Vec<&Row> = rows
        .iter()
        .filter(|r| r.status == TrackingStatus::HandFiled)
        .collect();
    if hand_filed.is_empty() {
        return Ok(());
    }
    let go = if args.adopt {
        true
    } else if args.json || !std::io::stdin().is_terminal() {
        eprintln!(
            "{} hand-filed tracking issue(s) lack the `{TRACKING_LABEL}` label; pass --adopt \
             to label them",
            hand_filed.len()
        );
        false
    } else {
        sandogasa_cli::confirm(
            &format!(
                "Label {} hand-filed tracking issue(s) `{TRACKING_LABEL}` so every command \
                 tracks them?",
                hand_filed.len()
            ),
            true,
        )?
    };
    if !go {
        return Ok(());
    }
    for r in hand_filed {
        let (Some(url), Some(iid)) = (&r.issue_url, r.issue_iid) else {
            continue;
        };
        let Some(project) = crate::status::tracking_project_of(url) else {
            eprintln!("warning: {url}: unrecognised issue URL; not labelled");
            continue;
        };
        gitlab::client(&gitlab_base(), &project)?.edit_issue(
            iid,
            &sandogasa_gitlab::IssueUpdate {
                add_labels: Some(TRACKING_LABEL.to_string()),
                ..Default::default()
            },
        )?;
        println!("labelled {url} `{TRACKING_LABEL}`");
    }
    Ok(())
}

/// Per-(release, package) classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackingStatus {
    /// Open issue in `rpms/<pkg>` with the release label and the
    /// tool's own, so every command sees it.
    Active,
    /// Open issue in `rpms/<pkg>` with the release label but not
    /// the tool's: filed by a person, invisible to the other
    /// commands until `--adopt` labels it.
    HandFiled,
    /// Open issue in `package_tracker` whose title starts with
    /// the package name. No per-package MR-backed issue yet.
    Proposed,
    /// No open issue in either location.
    Missing,
}

impl TrackingStatus {
    fn as_str(self) -> &'static str {
        match self {
            TrackingStatus::Active => "active",
            TrackingStatus::HandFiled => "hand-filed",
            TrackingStatus::Proposed => "proposed",
            TrackingStatus::Missing => "missing",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Row {
    pub release: String,
    pub package: String,
    pub status: TrackingStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    /// The issue's iid in its project, for `--adopt`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_iid: Option<u64>,
}

pub(crate) fn build_rows(args: &SyncIssuesArgs) -> Result<Vec<Row>, Box<dyn std::error::Error>> {
    let inventory = sandogasa_inventory::load(&args.inventory)?;

    let releases: Vec<String> = match &args.release {
        Some(r) => {
            if !inventory.inventory.workloads.contains_key(r) {
                return Err(format!(
                    "release '{r}' not found in inventory; available: {:?}",
                    inventory.workload_names()
                )
                .into());
            }
            vec![r.clone()]
        }
        None => inventory.inventory.workloads.keys().cloned().collect(),
    };

    let group_client = gitlab::group_client(&gitlab_base(), PROPOSED_UPDATES_GROUP)?;
    let tracker_client = gitlab::client(&gitlab_base(), PACKAGE_TRACKER_PROJECT)?;

    let mut rows: Vec<Row> = Vec::new();
    for release in &releases {
        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching {release} issues");
        }
        // By the release label alone: the tool's own issues and the
        // hand-filed ones both carry it, and the tool's label tells
        // them apart in `classify`.
        let active = group_client.list_issues(release, Some("opened"))?;

        if args.verbose {
            eprintln!("[cpu-sig-tracker] fetching proposed issues for {release}");
        }
        let proposed = tracker_client.list_issues(release, Some("opened"))?;

        let packages = inventory
            .inventory
            .workloads
            .get(release)
            .map(|w| w.packages.clone())
            .unwrap_or_default();

        for pkg in packages {
            rows.push(classify(release, &pkg, &active, &proposed));
        }
    }
    Ok(rows)
}

/// Decide which bucket a (release, package) falls into given
/// pre-fetched issue lists: `active` is every open issue in the
/// per-package projects with the release label, the tool's own
/// (labelled) first.
fn classify(
    release: &str,
    package: &str,
    active: &[gitlab::Issue],
    proposed: &[gitlab::Issue],
) -> Row {
    let row = |status, issue: Option<&gitlab::Issue>| Row {
        release: release.to_string(),
        package: package.to_string(),
        status,
        issue_url: issue.map(|i| i.web_url.clone()),
        issue_iid: issue.map(|i| i.iid),
    };
    let ours: Vec<&gitlab::Issue> = active
        .iter()
        .filter(|i| gitlab::package_from_issue_url(&i.web_url) == Some(package))
        .collect();
    if let Some(issue) = ours
        .iter()
        .find(|i| i.labels.iter().any(|l| l == TRACKING_LABEL))
    {
        return row(TrackingStatus::Active, Some(issue));
    }
    if let Some(issue) = ours.first() {
        return row(TrackingStatus::HandFiled, Some(issue));
    }
    if let Some(issue) = proposed
        .iter()
        .find(|i| title_matches_package(&i.title, package))
    {
        return row(TrackingStatus::Proposed, Some(issue));
    }
    row(TrackingStatus::Missing, None)
}

/// Does a package_tracker issue title reference the given
/// package? The convention is `"<pkg>: ..."` or `"<pkg> ..."`;
/// match on the package name followed by `:` or whitespace, so
/// that "xz: fix CVE" matches but "xz-utils: …" does not when
/// looking up "xz".
fn title_matches_package(title: &str, package: &str) -> bool {
    match title.strip_prefix(package) {
        Some(rest) => rest
            .chars()
            .next()
            .is_some_and(|c| c == ':' || c.is_whitespace()),
        None => false,
    }
}

fn print_human(rows: &[Row]) {
    let mut by_release: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for r in rows {
        by_release.entry(&r.release).or_default().push(r);
    }

    let pkg_width = rows
        .iter()
        .map(|r| r.package.chars().count())
        .max()
        .unwrap_or(0);

    let mut first = true;
    for (release, rs) in by_release {
        if !first {
            println!();
        }
        first = false;
        println!("release {release}:");
        let mut counts = [0usize; 4];
        for r in rs {
            let idx = match r.status {
                TrackingStatus::Active => 0,
                TrackingStatus::HandFiled => 1,
                TrackingStatus::Proposed => 2,
                TrackingStatus::Missing => 3,
            };
            counts[idx] += 1;
            let status = r.status.as_str();
            match &r.issue_url {
                Some(url) => println!("  {:<pkg_width$}  {:<10}  {url}", r.package, status),
                None => println!("  {:<pkg_width$}  {:<10}", r.package, status),
            }
        }
        println!(
            "  → {} active, {} hand-filed, {} proposed, {} missing",
            counts[0], counts[1], counts[2], counts[3]
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn issue(url: &str, title: &str) -> gitlab::Issue {
        labelled_issue(url, title, &["c10s", TRACKING_LABEL])
    }

    fn labelled_issue(url: &str, title: &str, labels: &[&str]) -> gitlab::Issue {
        serde_json::from_value(serde_json::json!({
            "labels": labels, "iid": 7, "title": title, "state": "opened",
            "web_url": url, "assignees": []
        }))
        .unwrap()
    }

    #[test]
    fn classify_calls_an_unlabelled_per_package_issue_hand_filed() {
        // blktrace#2 as a person filed it: the release label, not the
        // tool's. It is the tracking issue all the same, and wins over
        // a package_tracker entry — but is told apart from active.
        let active = vec![labelled_issue(
            "https://gitlab.com/CentOS/proposed_updates/rpms/blktrace/-/issues/2",
            "blktrace: Move librsvg2-tools runtime requirement",
            &["bugfix", "c10s"],
        )];
        let proposed = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/package_tracker/-/issues/4",
            "blktrace: proposed",
        )];
        let row = classify("c10s", "blktrace", &active, &proposed);
        assert_eq!(row.status, TrackingStatus::HandFiled);
        assert_eq!(row.issue_iid, Some(7));
        assert!(
            row.issue_url
                .unwrap()
                .ends_with("/rpms/blktrace/-/issues/2")
        );
        // With both kinds present, the labelled one is the active issue.
        let mut both = active;
        both.push(issue(
            "https://gitlab.com/CentOS/proposed_updates/rpms/blktrace/-/issues/3",
            "blktrace: 1.3.0-12 → 1.3.0-13",
        ));
        assert_eq!(
            classify("c10s", "blktrace", &both, &[]).status,
            TrackingStatus::Active
        );
    }

    #[test]
    fn title_match_colon() {
        assert!(title_matches_package("xz: fix CVE", "xz"));
    }

    #[test]
    fn title_match_space() {
        assert!(title_matches_package("xz update for c10s", "xz"));
    }

    #[test]
    fn title_no_match_different_package() {
        assert!(!title_matches_package("xz-utils: fix", "xz"));
    }

    #[test]
    fn title_no_match_prefix_word() {
        assert!(!title_matches_package("xzcat: fix", "xz"));
    }

    #[test]
    fn title_no_match_when_only_package_name() {
        // Bare title without any separator is ambiguous; reject
        // to avoid false positives on substrings.
        assert!(!title_matches_package("xz", "xz"));
    }

    #[test]
    fn classify_active_wins_over_proposed() {
        let active = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/rpms/xz/-/issues/1",
            "xz-5.4 → xz-5.6",
        )];
        let proposed = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/package_tracker/-/issues/9",
            "xz: proposed",
        )];
        let row = classify("c10s", "xz", &active, &proposed);
        assert_eq!(row.status, TrackingStatus::Active);
        assert_eq!(
            row.issue_url.as_deref(),
            Some("https://gitlab.com/CentOS/proposed_updates/rpms/xz/-/issues/1"),
        );
    }

    #[test]
    fn classify_proposed_when_only_tracker_match() {
        let active: Vec<gitlab::Issue> = vec![];
        let proposed = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/package_tracker/-/issues/9",
            "xz: proposed update",
        )];
        let row = classify("c10s", "xz", &active, &proposed);
        assert_eq!(row.status, TrackingStatus::Proposed);
        assert_eq!(
            row.issue_url.as_deref(),
            Some("https://gitlab.com/CentOS/proposed_updates/package_tracker/-/issues/9"),
        );
    }

    #[test]
    fn classify_missing_when_neither_matches() {
        let active = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/rpms/other/-/issues/1",
            "other",
        )];
        let proposed = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/package_tracker/-/issues/9",
            "other: proposed",
        )];
        let row = classify("c10s", "xz", &active, &proposed);
        assert_eq!(row.status, TrackingStatus::Missing);
        assert_eq!(row.issue_url, None);
    }

    #[test]
    fn classify_active_matches_on_project_not_title() {
        // The active-project lookup keys off the URL's rpms/<pkg>
        // segment, not the title — titles in rpms/<pkg> can be
        // arbitrary (e.g. "Fix CVE-2026-0001").
        let active = vec![issue(
            "https://gitlab.com/CentOS/proposed_updates/rpms/PackageKit/-/issues/1",
            "Fix CVE-2026-0001",
        )];
        let proposed: Vec<gitlab::Issue> = vec![];
        let row = classify("c10s", "PackageKit", &active, &proposed);
        assert_eq!(row.status, TrackingStatus::Active);
    }

    // ---- end-to-end wiremock test ----
    //
    // Drives `build_rows` against a live wiremock server that
    // impersonates GitLab. Sets a handful of env vars
    // (GITLAB_TOKEN, CPU_SIG_TRACKER_GITLAB_BASE) so the
    // wrapper's token loader and the base-URL helper point at
    // the fake. serial_test serializes the test body so
    // parallel runs don't stomp on each other's env mutations.

    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SAMPLE_INVENTORY: &str = r#"
[inventory]
name = "cpu-sig"
description = "test"
maintainer = "test"

[inventory.workloads.c10s]
packages = ["PackageKit", "missingpkg"]

[[package]]
name = "PackageKit"

[[package]]
name = "missingpkg"
"#;

    fn gitlab_issue_json(web_url: &str, title: &str) -> serde_json::Value {
        json!({
            "iid": 1,
            "title": title,
            "description": "",
            "state": "opened",
            "web_url": web_url,
            "labels": ["c10s", TRACKING_LABEL],
            "assignees": [],
        })
    }

    #[test]
    #[serial_test::serial]
    fn build_rows_classifies_active_proposed_missing() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(MockServer::start());
        let rpms_path = "/api/v4/groups/CentOS%2Fproposed_updates%2Frpms/issues";
        let tracker_path = "/api/v4/projects/CentOS%2Fproposed_updates%2Fpackage_tracker/issues";
        runtime.block_on(async {
            Mock::given(method("GET"))
                .and(path(rpms_path))
                .and(query_param("labels", "c10s"))
                .and(query_param("state", "opened"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!([gitlab_issue_json(
                        "https://gitlab.example/CentOS/proposed_updates/rpms/PackageKit/-/issues/3",
                        "PackageKit fix",
                    ),])),
                )
                .expect(1)
                .mount(&server)
                .await;

            Mock::given(method("GET"))
                .and(path(tracker_path))
                .and(query_param("labels", "c10s"))
                .and(query_param("state", "opened"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
                .expect(1)
                .mount(&server)
                .await;
        });

        let dir = tempdir().unwrap();
        let inv_path = dir.path().join("inv.toml");
        std::fs::write(&inv_path, SAMPLE_INVENTORY).unwrap();

        // SAFETY: env mutations are serialized via
        // serial_test; no other threads read these vars
        // concurrently.
        unsafe {
            std::env::set_var("GITLAB_TOKEN", "test-token");
            std::env::set_var("CPU_SIG_TRACKER_GITLAB_BASE", server.uri());
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
        }

        let args = SyncIssuesArgs {
            adopt: false,
            inventory: inv_path.to_string_lossy().into_owned(),
            release: Some("c10s".to_string()),
            json: false,
            verbose: false,
        };
        let rows = build_rows(&args).expect("build_rows");

        unsafe {
            std::env::remove_var("GITLAB_TOKEN");
            std::env::remove_var("CPU_SIG_TRACKER_GITLAB_BASE");
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        assert_eq!(rows.len(), 2);
        let package_kit = rows.iter().find(|r| r.package == "PackageKit").unwrap();
        assert_eq!(package_kit.status, TrackingStatus::Active);
        assert!(
            package_kit
                .issue_url
                .as_deref()
                .unwrap()
                .ends_with("/rpms/PackageKit/-/issues/3"),
        );
        let missing = rows.iter().find(|r| r.package == "missingpkg").unwrap();
        assert_eq!(missing.status, TrackingStatus::Missing);
        assert_eq!(missing.issue_url, None);
    }
}
