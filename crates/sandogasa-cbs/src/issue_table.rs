// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The version table in a Hyperscale tracking issue's description,
//! which hs-relmon writes when it files or updates an
//! `rfe::new-version` issue: a fenced block naming the package, then a
//! row per distribution (upstream, Fedora, CentOS Stream, from
//! Repology) and per Hyperscale release (from CBS).
//!
//! hs-relmon draws the whole table ([`render`]); the tools that move
//! builds in CBS redraw only a Hyperscale release's rows once a build
//! reached or left its `-testing` or `-release` ([`refreshed`]), so the
//! table does not contradict the comment they post (#214). Both draw
//! the rows with [`hyperscale_rows`], so they cannot drift apart.

use std::cmp::Ordering;

use crate::{Collection, HYPERSCALE_VARIANTS, Sig, Stage};

/// One row of the table. The Hyperscale rows have a status
/// (`newest`, `outdated`) when there is a version to compare with.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Row {
    /// `Upstream`, `Fedora Stable`, `Hyperscale 9 (testing)`, ….
    pub distro: String,
    pub version: String,
    /// The build (`kexec-tools-2.0.29-17.1.hs+fb.el9`) or the
    /// repository (`fedora_43`); may be empty.
    pub detail: String,
    /// May be empty.
    pub status: String,
}

impl Row {
    pub fn new(distro: &str, version: &str, detail: &str, status: &str) -> Self {
        Row {
            distro: distro.to_string(),
            version: version.to_string(),
            detail: detail.to_string(),
            status: status.to_string(),
        }
    }
}

/// A table read back from a description: the package and its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Table {
    pub package: String,
    pub rows: Vec<Row>,
}

/// `version` against the `reference` the issue tracks: `newest` when
/// it is at least as new, `outdated` when older, and empty without a
/// reference.
pub fn version_status(version: &str, reference: Option<&str>) -> String {
    match reference {
        Some(r) if sandogasa_rpmvercmp::rpmvercmp(version, r) != Ordering::Less => {
            "newest".to_string()
        }
        Some(_) => "outdated".to_string(),
        None => String::new(),
    }
}

/// The version of an NVR: `7.1` of `ethtool-7.1-1.hs.el9`.
pub fn version_of(nvr: &str) -> Option<&str> {
    let mut parts = nvr.rsplitn(3, '-');
    let (_release, version, _name) = (parts.next()?, parts.next()?, parts.next()?);
    Some(version)
}

/// The EL major an NVR is built for: 9 for `…hs+fb.el9`, 10 for
/// `….el10_1`.
pub fn el_major(nvr: &str) -> Option<u32> {
    let (_, after) = nvr.rsplit_once(".el")?;
    after.split('_').next()?.parse().ok()
}

/// The rows of Hyperscale release `major`, given the newest build in
/// any of its `-release` tags and the newest in its `-testing` tags
/// when that one is newer: a `(release)` and a `(testing)` row with
/// both, one plain row with only one of them released, a `(testing)`
/// row with only testing, and `not found` with neither.
pub fn hyperscale_rows(
    major: u32,
    release: Option<&str>,
    testing: Option<&str>,
    reference: Option<&str>,
) -> Vec<Row> {
    let label = format!("Hyperscale {major}");
    let row = |distro: String, nvr: &str| {
        let version = version_of(nvr).unwrap_or(nvr);
        Row {
            distro,
            version: version.to_string(),
            detail: nvr.to_string(),
            status: version_status(version, reference),
        }
    };
    match (release, testing) {
        (Some(rel), Some(test)) => vec![
            row(format!("{label} (release)"), rel),
            row(format!("{label} (testing)"), test),
        ],
        (Some(rel), None) => vec![row(label, rel)],
        (None, Some(test)) => vec![row(format!("{label} (testing)"), test)],
        (None, None) => vec![Row::new(&label, "not found", "", "")],
    }
}

/// The table of `package` with `rows`, as hs-relmon prints it and puts
/// in an issue (inside a fence); empty without rows.
pub fn render(package: &str, rows: &[Row]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let width = |f: fn(&Row) -> &str, title: &str| {
        rows.iter()
            .map(|r| f(r).chars().count())
            .max()
            .unwrap_or(0)
            .max(title.len())
    };
    let distro_w = width(|r| &r.distro, "Distribution");
    let version_w = width(|r| &r.version, "Version");
    let detail_w = width(|r| &r.detail, "Detail");
    let has_status = rows.iter().any(|r| !r.status.is_empty());
    let rule = |w: usize| "─".repeat(w);
    let mut out = format!("{package}\n");
    if has_status {
        out.push_str(&format!(
            "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  Status\n",
            "Distribution", "Version", "Detail"
        ));
        out.push_str(&format!(
            "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  ──────\n",
            rule(distro_w),
            rule(version_w),
            rule(detail_w)
        ));
    } else {
        out.push_str(&format!(
            "  {:<distro_w$}  {:<version_w$}  Detail\n",
            "Distribution", "Version"
        ));
        out.push_str(&format!(
            "  {:<distro_w$}  {:<version_w$}  ──────\n",
            rule(distro_w),
            rule(version_w)
        ));
    }
    for r in rows {
        let line = if !r.status.is_empty() {
            format!(
                "  {:<distro_w$}  {:<version_w$}  {:<detail_w$}  {}",
                r.distro, r.version, r.detail, r.status
            )
        } else if r.detail.is_empty() {
            format!("  {:<distro_w$}  {}", r.distro, r.version)
        } else {
            format!(
                "  {:<distro_w$}  {:<version_w$}  {}",
                r.distro, r.version, r.detail
            )
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The table [`render`] drew, read back from `text` (the lines inside
/// the fence); `None` for anything else.
pub fn parse(text: &str) -> Option<Table> {
    let mut lines = text.lines();
    let package = lines.next()?.trim().to_string();
    let header = lines.next()?;
    if !header.trim_start().starts_with("Distribution") {
        return None;
    }
    let has_status = header.trim_end().ends_with("Status");
    lines.next()?.trim_start().starts_with('─').then_some(())?;
    let mut rows = Vec::new();
    for line in lines.filter(|l| !l.trim().is_empty()) {
        // Columns are two spaces or more apart; a value has at most
        // single spaces (`not found`, `Hyperscale 9 (testing)`).
        let fields: Vec<&str> = line
            .trim()
            .split("  ")
            .map(str::trim)
            .filter(|f| !f.is_empty())
            .collect();
        let row = match fields[..] {
            [d, v] => Row::new(d, v, "", ""),
            [d, v, x] => Row::new(d, v, x, ""),
            [d, v, x, s] if has_status => Row::new(d, v, x, s),
            _ => return None,
        };
        rows.push(row);
    }
    if package.is_empty() || rows.is_empty() {
        return None;
    }
    Some(Table { package, rows })
}

/// `description` with the rows of Hyperscale release `major` in its
/// table redrawn from the builds CBS has there: `release`, every
/// `-release` tag's newest, and `testing`, every `-testing` tag's (see
/// [`hyperscale_builds`]). The newest of each is shown, the testing one
/// only when newer than the release one (see [`hyperscale_rows`]); of
/// builds of the same version, the one a row shows already stays, so
/// that a variant's build hs-relmon picked is not swapped for another's.
/// The rows are measured against `reference` when the table has a
/// status column. Every other row, and everything around the table,
/// stays as it is. `None` when nothing changes, or when the description
/// has no such table or no row for `major` (hs-relmon was not showing
/// that release).
pub fn refreshed(
    description: &str,
    major: u32,
    release: &[String],
    testing: &[String],
    reference: Option<&str>,
) -> Option<String> {
    let (description, body_start, close, table) = fenced(description)?;
    let first = table
        .rows
        .iter()
        .position(|r| is_hyperscale_row(&r.distro, major))?;
    let shown = |suffixes: &[&str]| {
        table.rows.iter().find_map(|r| {
            let rest = r.distro.strip_prefix(&format!("Hyperscale {major}"))?;
            (suffixes.contains(&rest) && !r.detail.is_empty()).then_some(r.detail.as_str())
        })
    };
    let rel = pick(release, shown(&["", " (release)"]));
    let test = pick(testing, shown(&[" (testing)"]))
        .filter(|t| rel.is_none_or(|r| cmp_nvr(t, r) == Ordering::Greater));
    // A status column means hs-relmon had a version to compare with.
    let has_status = table.rows.iter().any(|r| !r.status.is_empty());
    let mut rows: Vec<Row> = table.rows[..first].to_vec();
    rows.extend(hyperscale_rows(
        major,
        rel,
        test,
        reference.filter(|_| has_status),
    ));
    rows.extend(
        table.rows[first..]
            .iter()
            .filter(|r| !is_hyperscale_row(&r.distro, major))
            .cloned(),
    );
    if rows == table.rows {
        return None;
    }
    Some(format!(
        "{}{}{}",
        &description[..body_start],
        render(&table.package, &rows),
        &description[close..]
    ))
}

/// `description` (a table in a fence, and what follows it) with the rows
/// for `distros` that it lacks taken from `existing`, the description
/// it replaces: for rows whose source could not be read this run (a
/// Repology refusal), so an update keeps what the issue said rather than
/// dropping it. `distros` come first, in their order, then the rest of
/// `description`'s rows. `description` as it is when either has no
/// table, or nothing is missing.
pub fn carry_rows(description: &str, existing: &str, distros: &[&str]) -> String {
    let (Some((new, start, close, table)), Some((_, _, _, old))) =
        (fenced(description), fenced(existing))
    else {
        return description.to_string();
    };
    let find = |rows: &[Row], d: &str| rows.iter().find(|r| r.distro == d).cloned();
    let mut rows: Vec<Row> = distros
        .iter()
        .filter_map(|d| find(&table.rows, d).or_else(|| find(&old.rows, d)))
        .collect();
    rows.extend(
        table
            .rows
            .iter()
            .filter(|r| !distros.contains(&r.distro.as_str()))
            .cloned(),
    );
    if rows == table.rows {
        return description.to_string();
    }
    format!(
        "{}{}{}",
        &new[..start],
        render(&table.package, &rows),
        &new[close..]
    )
}

/// The `-release` and `-testing` tags of every Hyperscale collection on
/// EL `major`: each variant's on CentOS Stream, the RHEL ones', and the
/// hotfixes destination. Some do not exist in CBS (no asahi on 9);
/// [`crate::packages::latest_in_tags`] reads them as having no build.
pub fn hyperscale_tags(major: u32) -> (Vec<String>, Vec<String>) {
    let mut collections: Vec<Collection> = HYPERSCALE_VARIANTS
        .iter()
        .map(|v| Collection::new(Sig::Hyperscale.name(), major, true, v.name))
        .collect();
    collections.extend(
        HYPERSCALE_VARIANTS
            .iter()
            .filter(|v| v.rhel)
            .map(|v| Collection::new(Sig::Hyperscale.name(), major, false, v.name)),
    );
    collections.push(Collection::new(
        Sig::Hyperscale.name(),
        major,
        true,
        "hotfixes",
    ));
    (
        collections.iter().map(|c| c.tag(Stage::Release)).collect(),
        collections.iter().map(|c| c.tag(Stage::Testing)).collect(),
    )
}

/// The builds of `package` on Hyperscale release `major` the table
/// chooses from ([`refreshed`]): the newest in each `-release` tag, and
/// the newest in each `-testing` tag, read with `latest`
/// ([`crate::packages::latest_in_tags`]) in one request.
pub fn hyperscale_builds(
    major: u32,
    package: &str,
    latest: crate::notify::LatestIn,
) -> Result<(Vec<String>, Vec<String>), String> {
    let (release_tags, testing_tags) = hyperscale_tags(major);
    let n = release_tags.len();
    let tags: Vec<String> = release_tags.into_iter().chain(testing_tags).collect();
    let found = latest(&tags, package)?;
    if found.len() != tags.len() {
        return Err(format!(
            "the hub answered {} of {} tags",
            found.len(),
            tags.len()
        ));
    }
    let each = |found: &[Option<String>]| found.iter().flatten().cloned().collect::<Vec<_>>();
    Ok((each(&found[..n]), each(&found[n..])))
}

/// `a` against `b`, two builds of one package: by version, then by
/// release.
fn cmp_nvr(a: &str, b: &str) -> Ordering {
    let vr = |nvr: &str| {
        let mut parts = nvr.rsplitn(3, '-');
        let release = parts.next().unwrap_or("").to_string();
        let version = parts.next().unwrap_or("").to_string();
        (version, release)
    };
    let ((va, ra), (vb, rb)) = (vr(a), vr(b));
    sandogasa_rpmvercmp::rpmvercmp(&va, &vb).then_with(|| sandogasa_rpmvercmp::rpmvercmp(&ra, &rb))
}

/// `description` with CRLF endings made LF (GitLab's web UI may store
/// them), where its fenced table's text starts and its closing fence
/// is, and the table; `None` without one.
fn fenced(description: &str) -> Option<(String, usize, usize, Table)> {
    let description = description.replace("\r\n", "\n");
    let open = description.find("```\n")?;
    let start = open + 4;
    let close = start + description[start..].find("```")?;
    let table = parse(&description[start..close])?;
    Some((description, start, close, table))
}

/// Whether `distro` is a row of Hyperscale release `major`.
fn is_hyperscale_row(distro: &str, major: u32) -> bool {
    let label = format!("Hyperscale {major}");
    distro == label || distro.starts_with(&format!("{label} ("))
}

/// The newest of `found`, or `shown` when it is one of them and of the
/// same version: builds of one version in several variants sort by
/// their release (`hs+fb` above `hs`), where hs-relmon takes the one
/// built last, so the one hs-relmon showed is kept.
fn pick<'a>(found: &'a [String], shown: Option<&str>) -> Option<&'a str> {
    let newest = found.iter().max_by(|a, b| cmp_nvr(a, b))?;
    let kept = shown.and_then(|s| {
        found
            .iter()
            .find(|f| *f == s && version_of(f) == version_of(newest))
    });
    Some(kept.unwrap_or(newest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(nvrs: &[&str]) -> Vec<String> {
        nvrs.iter().map(|s| s.to_string()).collect()
    }

    /// kexec-tools' table on 2026-11-20 (michel-slm/sandogasa#214),
    /// as hs-relmon drew it.
    fn kexec_table() -> String {
        render(
            "kexec-tools",
            &[
                Row::new("Upstream", "2.0.32", "", ""),
                Row::new("Fedora Rawhide", "2.0.32", "", ""),
                Row::new("CentOS Stream", "2.0.29", "centos_stream_10", ""),
                Row::new(
                    "Hyperscale 9 (release)",
                    "2.0.29",
                    "kexec-tools-2.0.29-17.1.hs+fb.el9",
                    "outdated",
                ),
                Row::new(
                    "Hyperscale 9 (testing)",
                    "2.0.29",
                    "kexec-tools-2.0.29-18.1.hs+fb.el9",
                    "outdated",
                ),
                Row::new(
                    "Hyperscale 10",
                    "2.0.29",
                    "kexec-tools-2.0.29-17.1.hs+fb.el10",
                    "outdated",
                ),
            ],
        )
    }

    fn issue(table: &str) -> String {
        format!(
            "```\n{table}```\n\n{} 1 commit on c9s-hs+fb",
            crate::unbuilt::LINE
        )
    }

    #[test]
    fn a_table_reads_back_as_drawn() {
        let text = kexec_table();
        let table = parse(&text).unwrap();
        assert_eq!(table.package, "kexec-tools");
        assert_eq!(table.rows.len(), 6);
        assert_eq!(table.rows[2].detail, "centos_stream_10");
        assert_eq!(table.rows[3].status, "outdated");
        assert_eq!(render(&table.package, &table.rows), text);
        // Without a status column, and with a version that has a space.
        let plain = render(
            "foo",
            &[
                Row::new("Upstream", "1.0", "", ""),
                Row::new("Hyperscale 9", "not found", "", ""),
            ],
        );
        assert!(!plain.contains("Status"));
        let back = parse(&plain).unwrap();
        assert_eq!(back.rows[1], Row::new("Hyperscale 9", "not found", "", ""));
        assert_eq!(render("foo", &back.rows), plain);
    }

    #[test]
    fn text_that_is_not_a_table_is_not_read_as_one() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("foo\nsome prose\n"), None);
        assert_eq!(parse("foo\n  Distribution  Version  Detail\n"), None);
        assert_eq!(
            parse("foo\n  Distribution  Version  Detail\n  ─  ─  ──────\n"),
            None
        );
        assert_eq!(render("foo", &[]), "");
    }

    #[test]
    fn a_promotion_to_testing_rewrites_only_that_releases_rows() {
        let before = issue(&kexec_table());
        let after = refreshed(
            &before,
            9,
            &v(&["kexec-tools-2.0.29-17.1.hs+fb.el9"]),
            &v(&[
                "kexec-tools-2.0.32-1.1.hs+fb.el9",
                "kexec-tools-2.0.29-18.1.hs+fb.el9",
            ]),
            Some("2.0.32"),
        )
        .unwrap();
        let table = parse(&after[4..after.rfind("```").unwrap()]).unwrap();
        assert_eq!(
            table.rows[4],
            Row::new(
                "Hyperscale 9 (testing)",
                "2.0.32",
                "kexec-tools-2.0.32-1.1.hs+fb.el9",
                "newest"
            )
        );
        // The Repology rows, the other release and the not-built line
        // stay as they were.
        let old = parse(&kexec_table()).unwrap();
        assert_eq!(table.rows[..4], old.rows[..4]);
        assert_eq!(table.rows[5], old.rows[5]);
        assert!(after.ends_with(&format!("{} 1 commit on c9s-hs+fb", crate::unbuilt::LINE)));
    }

    #[test]
    fn a_release_turns_two_rows_into_one_and_back() {
        let before = issue(&kexec_table());
        let released = refreshed(
            &before,
            9,
            &v(&["kexec-tools-2.0.32-1.1.hs+fb.el9"]),
            &[],
            Some("2.0.32"),
        )
        .unwrap();
        let rows = parse(&released[4..released.rfind("```").unwrap()])
            .unwrap()
            .rows;
        assert_eq!(rows.len(), 5);
        assert_eq!(
            rows[3],
            Row::new(
                "Hyperscale 9",
                "2.0.32",
                "kexec-tools-2.0.32-1.1.hs+fb.el9",
                "newest"
            )
        );
        assert_eq!(rows[4].distro, "Hyperscale 10");
        // A new build in testing on 10 splits its row.
        let split = refreshed(
            &released,
            10,
            &v(&["kexec-tools-2.0.29-17.1.hs+fb.el10"]),
            &v(&["kexec-tools-2.0.32-1.1.hs+fb.el10"]),
            Some("2.0.32"),
        )
        .unwrap();
        let rows = parse(&split[4..split.rfind("```").unwrap()]).unwrap().rows;
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[4].distro, "Hyperscale 10 (release)");
        assert_eq!(rows[5].distro, "Hyperscale 10 (testing)");
        assert_eq!(rows[5].status, "newest");
        // Untagging every build leaves `not found`.
        let gone = refreshed(&split, 10, &[], &[], Some("2.0.32")).unwrap();
        let rows = parse(&gone[4..gone.rfind("```").unwrap()]).unwrap().rows;
        assert_eq!(rows[4], Row::new("Hyperscale 10", "not found", "", ""));
    }

    #[test]
    fn nothing_is_rewritten_when_nothing_changed_or_there_is_no_table() {
        let before = issue(&kexec_table());
        assert_eq!(
            refreshed(
                &before,
                9,
                &v(&["kexec-tools-2.0.29-17.1.hs+fb.el9"]),
                &v(&["kexec-tools-2.0.29-18.1.hs+fb.el9"]),
                Some("2.0.32"),
            ),
            None
        );
        assert_eq!(
            refreshed("Filed by hand.", 9, &v(&["a-1-1.el9"]), &[], None),
            None
        );
        // A table without a row for the release: hs-relmon was not
        // showing it, so none is added.
        let only_nine = issue(&render(
            "foo",
            &[Row::new("Hyperscale 9", "1.0", "foo-1.0-1.hs.el9", "")],
        ));
        assert_eq!(
            refreshed(&only_nine, 10, &v(&["foo-2.0-1.hs.el10"]), &[], None),
            None
        );
    }

    #[test]
    fn rows_without_a_status_get_none() {
        let before = issue(&render(
            "foo",
            &[Row::new("Hyperscale 9", "1.0", "foo-1.0-1.hs.el9", "")],
        ));
        let after = refreshed(&before, 9, &v(&["foo-2.0-1.hs.el9"]), &[], Some("2.0")).unwrap();
        assert!(!after.contains("newest"));
        assert!(after.contains("foo-2.0-1.hs.el9"));
    }

    #[test]
    fn statuses_and_versions_are_compared_as_rpm_does() {
        assert_eq!(version_status("2.0.10", Some("2.0.9")), "newest");
        assert_eq!(version_status("2.0.9", Some("2.0.10")), "outdated");
        assert_eq!(version_status("2.0.9", None), "");
        assert_eq!(
            version_of("kexec-tools-2.0.32-1.1.hs+fb.el9"),
            Some("2.0.32")
        );
        assert_eq!(version_of("nodash"), None);
        assert_eq!(el_major("kexec-tools-2.0.32-1.1.hs+fb.el9"), Some(9));
        assert_eq!(el_major("foo-1-1.hs.el10_1"), Some(10));
        assert_eq!(el_major("<the build in foo>"), None);
        assert_eq!(
            cmp_nvr("foo-1.10-1.el9", "foo-1.9-5.el9"),
            Ordering::Greater
        );
        assert_eq!(
            cmp_nvr("foo-1.9-10.el9", "foo-1.9-9.el9"),
            Ordering::Greater
        );
    }

    #[test]
    fn every_variant_and_hotfixes_is_asked_about() {
        let (release, testing) = hyperscale_tags(9);
        assert!(release.contains(&"hyperscale9s-packages-main-release".to_string()));
        assert!(release.contains(&"hyperscale9-packages-facebook-release".to_string()));
        assert!(release.contains(&"hyperscale9s-packages-hotfixes-release".to_string()));
        assert!(testing.contains(&"hyperscale9s-packages-facebook-testing".to_string()));
        assert_eq!(release.len(), testing.len());
    }

    #[test]
    fn each_tags_builds_are_read_at_once() {
        let (release_tags, _) = hyperscale_tags(9);
        let n = release_tags.len();
        let mut found = vec![None; 2 * n];
        found[0] = Some("foo-1.0-1.hs.el9".to_string());
        found[6] = Some("foo-1.1-1.hs+fb.el9".to_string());
        found[n + 6] = Some("foo-1.2-1.hs+fb.el9".to_string());
        let latest = |tags: &[String], _: &str| {
            assert_eq!(tags.len(), 2 * n);
            Ok(found.clone())
        };
        assert_eq!(
            hyperscale_builds(9, "foo", &latest).unwrap(),
            (
                v(&["foo-1.0-1.hs.el9", "foo-1.1-1.hs+fb.el9"]),
                v(&["foo-1.2-1.hs+fb.el9"])
            )
        );
        // An answer short of the tags, or an error, is an error.
        let short = |_: &[String], _: &str| Ok(vec![None]);
        assert!(hyperscale_builds(9, "foo", &short).is_err());
        let fails = |_: &[String], _: &str| Err("down".to_string());
        assert!(hyperscale_builds(9, "foo", &fails).is_err());
    }

    #[test]
    fn the_newest_release_and_a_newer_testing_build_are_shown() {
        let table = |rows: &[Row]| issue(&render("foo", rows));
        let rows = |d: &str| parse(&d[4..d.rfind("```").unwrap()]).unwrap().rows;
        let before = table(&[Row::new("Hyperscale 9", "not found", "", "")]);
        // A testing build no newer than the release one is not shown.
        let after = refreshed(
            &before,
            9,
            &v(&["foo-1.0-1.hs.el9", "foo-1.1-1.hs.el9"]),
            &v(&["foo-1.1-1.hs.el9", "foo-1.0-2.hs.el9"]),
            None,
        )
        .unwrap();
        assert_eq!(
            rows(&after),
            [Row::new("Hyperscale 9", "1.1", "foo-1.1-1.hs.el9", "")]
        );
    }

    #[test]
    fn a_first_build_gets_a_status_when_the_table_has_them() {
        let before = issue(&render(
            "foo",
            &[
                Row::new("Hyperscale 9", "1.0", "foo-1.0-1.hs.el9", "outdated"),
                Row::new("Hyperscale 10", "not found", "", ""),
            ],
        ));
        let after = refreshed(&before, 10, &[], &v(&["foo-2.0-1.hs.el10"]), Some("2.0")).unwrap();
        let rows = parse(&after[4..after.rfind("```").unwrap()]).unwrap().rows;
        assert_eq!(
            rows[1],
            Row::new(
                "Hyperscale 10 (testing)",
                "2.0",
                "foo-2.0-1.hs.el10",
                "newest"
            )
        );
    }

    #[test]
    fn the_variants_build_a_row_shows_stays_for_its_version() {
        // hs-relmon showed main's build; facebook's of the same version
        // sorts above it, but the row keeps main's.
        let before = issue(&render(
            "foo",
            &[Row::new(
                "Hyperscale 9",
                "1.0",
                "foo-1.0-1.hs.el9",
                "newest",
            )],
        ));
        let found = v(&["foo-1.0-1.hs+fb.el9", "foo-1.0-1.hs.el9"]);
        assert_eq!(refreshed(&before, 9, &found, &[], Some("1.0")), None);
        // A newer version replaces it.
        let found = v(&["foo-1.0-1.hs.el9", "foo-1.1-1.hs+fb.el9"]);
        assert!(
            refreshed(&before, 9, &found, &[], Some("1.0"))
                .unwrap()
                .contains("foo-1.1-1.hs+fb.el9")
        );
    }

    #[test]
    fn rows_a_run_could_not_read_are_kept_from_the_issue() {
        let repology = ["Upstream", "Fedora Rawhide"];
        let old = issue(&kexec_table());
        // This run has no Upstream row (Repology refused) but a new
        // Fedora Rawhide one, from fedrq.
        let now = issue(&render(
            "kexec-tools",
            &[
                Row::new("Fedora Rawhide", "2.0.33", "", ""),
                Row::new(
                    "Hyperscale 9",
                    "2.0.32",
                    "kexec-tools-2.0.32-1.1.hs+fb.el9",
                    "newest",
                ),
            ],
        ));
        let kept = carry_rows(&now, &old, &repology);
        let rows = parse(&kept[4..kept.rfind("```").unwrap()]).unwrap().rows;
        assert_eq!(rows[0], Row::new("Upstream", "2.0.32", "", ""));
        assert_eq!(rows[1], Row::new("Fedora Rawhide", "2.0.33", "", ""));
        assert_eq!(rows[2].distro, "Hyperscale 9");
        assert_eq!(rows.len(), 3);
        assert!(kept.ends_with(&format!("{} 1 commit on c9s-hs+fb", crate::unbuilt::LINE)));
        // Nothing missing, or nothing to take from: unchanged.
        assert_eq!(carry_rows(&old, &now, &repology), old);
        assert_eq!(carry_rows(&now, "Filed by hand.", &repology), now);
        assert_eq!(carry_rows("no table", &old, &repology), "no table");
    }

    #[test]
    fn a_description_with_crlf_endings_is_read() {
        let before = issue(&render(
            "foo",
            &[Row::new("Hyperscale 9", "1.0", "foo-1.0-1.hs.el9", "")],
        ))
        .replace('\n', "\r\n");
        let after = refreshed(&before, 9, &v(&["foo-2.0-1.hs.el9"]), &[], None).unwrap();
        assert!(after.contains("foo-2.0-1.hs.el9"));
        assert!(after.contains(crate::unbuilt::LINE));
    }
}
