// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Regroup a finished report by the repository each item touched.
//!
//! The report is collected domain by domain and service by service,
//! which answers "what did this person do on GitLab" well and "what
//! happened to this repository" not at all — the answer is spread
//! across every section and has to be reassembled by eye.
//!
//! Nothing here queries anything. Every item already carries the
//! repository it belongs to (`MrRef::project`, `PrRef::repo`, and the
//! commit maps are keyed by it), so this is a regrouping of what was
//! gathered rather than a second collection.

use std::collections::BTreeMap;

use serde::Serialize;

/// What one repository saw, summed across domains and forges.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct RepoActivity {
    /// `host/path`, so two repositories with the same path on
    /// different forges do not merge into one row.
    pub repo: String,
    pub opened: usize,
    pub merged: usize,
    pub reviewed: usize,
    pub commented: usize,
    pub commits: u64,
}

impl RepoActivity {
    /// Everything this row counts, for ordering and for dropping
    /// rows that are entirely empty.
    pub fn total(&self) -> u64 {
        self.opened as u64
            + self.merged as u64
            + self.reviewed as u64
            + self.commented as u64
            + self.commits
    }
}

/// The host part of an instance URL, for keying a repository.
///
/// A bare host is returned as-is, so a value that is already a host
/// rather than a URL still keys correctly.
fn host_of(instance: &str) -> &str {
    instance
        .split_once("://")
        .map_or(instance, |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or(instance)
        .trim_end_matches('/')
}

/// `host/path`, the key a repository is gathered under.
fn key(instance: &str, path: &str) -> String {
    format!("{}/{}", host_of(instance), path.trim_start_matches('/'))
}

/// The row for `k`, created empty on first sight.
fn at(rows: &mut BTreeMap<String, RepoActivity>, k: String) -> &mut RepoActivity {
    rows.entry(k.clone()).or_insert_with(|| RepoActivity {
        repo: k,
        ..RepoActivity::default()
    })
}

/// Gather a report's items by repository, busiest first.
///
/// Ties break on the name so the order is stable between runs, which
/// matters because these reports are committed and diffed.
pub fn gather(report: &crate::report::Report) -> Vec<RepoActivity> {
    let mut rows: BTreeMap<String, RepoActivity> = BTreeMap::new();

    for domain in &report.domains {
        if let Some(gl) = &domain.gitlab {
            for (list, field) in [
                (&gl.opened_mrs, Field::Opened),
                (&gl.merged_mrs, Field::Merged),
                (&gl.approved_mrs, Field::Reviewed),
                (&gl.commented_mrs, Field::Commented),
            ] {
                for mr in list {
                    field.add(at(&mut rows, key(&gl.instance, &mr.project)));
                }
            }
            // Pushed commits are counted, not authored ones: a push
            // can carry work the pusher did not write, which the
            // GitLab report documents, so the two are not summed.
            for (project, n) in &gl.commits_pushed {
                at(&mut rows, key(&gl.instance, project)).commits += n;
            }
        }
        if let Some(gh) = &domain.github {
            for (list, field) in [
                (&gh.opened_prs, Field::Opened),
                (&gh.merged_prs, Field::Merged),
                (&gh.reviewed_prs, Field::Reviewed),
                (&gh.commented_prs, Field::Commented),
            ] {
                for pr in list {
                    field.add(at(&mut rows, key(&gh.instance, &pr.repo)));
                }
            }
            for (repo, n) in &gh.commits_authored {
                at(&mut rows, key(&gh.instance, repo)).commits += n;
            }
        }
        if let Some(fj) = &domain.forgejo {
            for (list, field) in [
                (&fj.opened_prs, Field::Opened),
                (&fj.merged_prs, Field::Merged),
            ] {
                for pr in list {
                    field.add(at(&mut rows, key(&fj.instance, &pr.repo)));
                }
            }
        }
    }

    let mut out: Vec<RepoActivity> = rows.into_values().filter(|r| r.total() > 0).collect();
    out.sort_by(|a, b| b.total().cmp(&a.total()).then_with(|| a.repo.cmp(&b.repo)));
    out
}

/// Which counter an item lands in.
#[derive(Clone, Copy)]
enum Field {
    Opened,
    Merged,
    Reviewed,
    Commented,
}

impl Field {
    fn add(self, row: &mut RepoActivity) {
        match self {
            Field::Opened => row.opened += 1,
            Field::Merged => row.merged += 1,
            Field::Reviewed => row.reviewed += 1,
            Field::Commented => row.commented += 1,
        }
    }
}

/// A column: its heading and the counter it reads.
type Column = (&'static str, fn(&RepoActivity) -> u64);

/// Every column a row can carry, in the order they are printed.
/// A column every row leaves at zero is dropped, so a report with no
/// reviews does not carry a column explaining that none happened.
const ALL_COLUMNS: &[Column] = &[
    ("Opened", |r| r.opened as u64),
    ("Merged", |r| r.merged as u64),
    ("Reviewed", |r| r.reviewed as u64),
    ("Commented", |r| r.commented as u64),
    ("Commits", |r| r.commits),
];

/// Render the gathered rows as a Markdown section.
///
/// Empty columns are dropped: a report with no reviews should not
/// carry a column of zeroes explaining that none happened.
pub fn format_markdown(rows: &[RepoActivity]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let cols: Vec<Column> = ALL_COLUMNS
        .iter()
        .copied()
        .filter(|(_, f)| rows.iter().any(|r| f(r) > 0))
        .collect();

    let mut out = String::from("## By repository\n\n| Repository |");
    for (name, _) in &cols {
        out.push_str(&format!(" {name} |"));
    }
    out.push_str("\n| --- |");
    for _ in &cols {
        out.push_str(" ---: |");
    }
    out.push('\n');
    for row in rows {
        out.push_str(&format!("| `{}` |", row.repo));
        for (_, f) in &cols {
            out.push_str(&format!(" {} |", f(row)));
        }
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_is_taken_from_whatever_shape_the_instance_has() {
        assert_eq!(host_of("https://gitlab.com"), "gitlab.com");
        assert_eq!(host_of("https://gitlab.com/"), "gitlab.com");
        assert_eq!(host_of("https://api.github.com/v3"), "api.github.com");
        // Already a host, not a URL.
        assert_eq!(host_of("codeberg.org"), "codeberg.org");
    }

    #[test]
    fn the_same_path_on_two_forges_stays_two_repositories() {
        // `owner/name` is not unique across forges, so the host is
        // part of the key or an unrelated repository merges in.
        assert_eq!(
            key("https://github.com", "slopfest/sandogasa"),
            "github.com/slopfest/sandogasa"
        );
        assert_ne!(
            key("https://github.com", "o/r"),
            key("https://codeberg.org", "o/r")
        );
        // A leading slash on the path does not double the separator.
        assert_eq!(key("gitlab.com", "/a/b"), "gitlab.com/a/b");
    }

    fn row(repo: &str, opened: usize, commits: u64) -> RepoActivity {
        RepoActivity {
            repo: repo.to_string(),
            opened,
            commits,
            ..RepoActivity::default()
        }
    }

    #[test]
    fn an_empty_column_is_not_printed() {
        // Nobody reviewed anything here, so there is no Reviewed
        // column saying zero five times.
        let md = format_markdown(&[row("gitlab.com/a/b", 2, 7)]);
        assert!(md.contains("| Opened |"), "{md}");
        assert!(md.contains("| Commits |"), "{md}");
        assert!(!md.contains("Reviewed"), "{md}");
        assert!(!md.contains("Commented"), "{md}");
        assert!(md.contains("| `gitlab.com/a/b` | 2 | 7 |"), "{md}");
    }

    #[test]
    fn nothing_gathered_renders_nothing() {
        // Not an empty table with a header: a section that says
        // nothing should not appear at all.
        assert_eq!(format_markdown(&[]), "");
    }

    #[test]
    fn rows_are_busiest_first_and_stable_on_ties() {
        // Committed reports are diffed, so two runs over the same
        // data must order identically.
        let rows = vec![
            row("z/quiet", 1, 0),
            row("a/busy", 3, 4),
            row("b/tie", 1, 0),
        ];
        let mut sorted = rows;
        sorted.sort_by(|a, b| b.total().cmp(&a.total()).then_with(|| a.repo.cmp(&b.repo)));
        let names: Vec<&str> = sorted.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(names, ["a/busy", "b/tie", "z/quiet"]);
    }

    #[test]
    fn a_row_totals_every_counter_it_holds() {
        let r = RepoActivity {
            repo: "x".into(),
            opened: 1,
            merged: 2,
            reviewed: 3,
            commented: 4,
            commits: 5,
        };
        assert_eq!(r.total(), 15);
        assert_eq!(RepoActivity::default().total(), 0);
    }
}
