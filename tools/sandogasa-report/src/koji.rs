// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Koji CBS reporting — query packages tagged in CentOS SIG release tags.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::brace;
use crate::config::DomainConfig;

/// Whether a package is new or updated during the reporting period.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// Package was not in the tag at the start of the period.
    New,
    /// Package was in the tag but with a different version.
    Updated,
}

/// A package found across Koji tags, with per-distro versions.
#[derive(Debug, Clone, Serialize)]
pub struct PackageEntry {
    /// Package name (source RPM).
    pub name: String,
    /// Whether this is a new package or an update.
    pub change: ChangeKind,
    /// Distro version → (version, release, tag) for each build.
    /// e.g. "el9" → ("256.12", "1.hs.el9", "hyperscale9s-...")
    pub versions: BTreeMap<String, BuildVersion>,
    /// Owner (from the first build seen).
    pub owner: String,
}

/// Version info for a single distro build.
#[derive(Debug, Clone, Serialize)]
pub struct BuildVersion {
    pub version: String,
    pub release: String,
    pub tag: String,
}

/// Koji CBS report for a domain.
#[derive(Debug, Serialize)]
pub struct KojiReport {
    /// Package name → entry with per-distro versions.
    pub packages: BTreeMap<String, PackageEntry>,
}

/// Extract the distro suffix from a tag name.
///
/// e.g. "hyperscale9s-packages-main-release" → "el9"
///      "hyperscale10-packages-main-release" → "el10"
fn distro_from_tag(tag: &str) -> String {
    // Extract version digits after the SIG name prefix.
    // Pattern: {sig}{version}[s]-packages-...
    // Find the first digit, then take digits (and optional trailing 's').
    let digits_start = tag.find(|c: char| c.is_ascii_digit());
    if let Some(start) = digits_start {
        let rest = &tag[start..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit() && c != 's')
            .unwrap_or(rest.len());
        let ver_part = &rest[..end];
        // Strip trailing 's' (stream indicator) for display.
        let ver = ver_part.trim_end_matches('s');
        return format!("el{ver}");
    }
    tag.to_string()
}

/// Query a tag snapshot: package name → (version, distro, owner).
/// Snapshot type: package name → distro → (version, release, owner).
type TagSnapshot = BTreeMap<String, BTreeMap<String, (String, String, String)>>;

/// Merge builds from a single tag into a snapshot.
fn merge_builds_into_snapshot(
    snapshot: &mut TagSnapshot,
    builds: &[sandogasa_koji::TaggedBuild],
    distro: &str,
) {
    for b in builds {
        let Some((name, version, release)) = sandogasa_koji::parse_nvr(&b.nvr) else {
            continue;
        };
        let distro_map = snapshot.entry(name.to_string()).or_default();
        distro_map
            .entry(distro.to_string())
            .and_modify(|(old_ver, old_rel, old_owner)| {
                if version > old_ver.as_str()
                    || (version == old_ver.as_str() && release > old_rel.as_str())
                {
                    *old_ver = version.to_string();
                    *old_rel = release.to_string();
                    *old_owner = b.owner.clone();
                }
            })
            .or_insert((version.to_string(), release.to_string(), b.owner.clone()));
    }
}

fn query_tag_snapshot(
    tags: &[String],
    profile: Option<&str>,
    timestamp: Option<i64>,
    user: Option<&str>,
    verbose: bool,
) -> TagSnapshot {
    let mut packages: TagSnapshot = BTreeMap::new();

    for tag in tags {
        if verbose {
            eprintln!(
                "[koji] querying tag {tag}{}",
                timestamp.map_or(String::new(), |t| format!(" at ts={t}"))
            );
        }

        let mut builds = match sandogasa_koji::list_tagged(tag, profile, timestamp) {
            Ok(b) => b,
            Err(e) => {
                if verbose {
                    eprintln!("[koji] warning: {e}");
                }
                continue;
            }
        };

        if let Some(user) = user {
            builds.retain(|b| b.owner == user);
        }

        let distro = distro_from_tag(tag);
        merge_builds_into_snapshot(&mut packages, &builds, &distro);
    }

    packages
}

/// Run Koji CBS reporting for a domain config.
///
/// Compares tag state at the start and end of the reporting period
/// to identify new and updated packages.
pub fn koji_report(
    domain: &DomainConfig,
    user: Option<&str>,
    since: chrono::NaiveDate,
    until: chrono::NaiveDate,
    verbose: bool,
) -> Result<KojiReport, String> {
    let profile = domain.koji_profile.as_deref();

    let tags: Vec<String> = domain
        .koji_tags
        .iter()
        .flat_map(|pattern| brace::expand(pattern))
        .collect();

    if tags.is_empty() {
        return Err("no koji_tags configured for this domain".to_string());
    }

    // Query at start of period and end of period (next day, inclusive).
    let start_ts = since.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp();
    let end_ts = until
        .succ_opt()
        .unwrap_or(until)
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp();

    let before = query_tag_snapshot(&tags, profile, Some(start_ts), user, verbose);
    let after = query_tag_snapshot(&tags, profile, Some(end_ts), user, verbose);

    let packages = diff_snapshots(&before, &after);
    Ok(KojiReport { packages })
}

/// Diff two tag snapshots to find new and updated packages.
fn diff_snapshots(before: &TagSnapshot, after: &TagSnapshot) -> BTreeMap<String, PackageEntry> {
    let mut packages: BTreeMap<String, PackageEntry> = BTreeMap::new();

    for (name, after_versions) in after {
        let before_versions = before.get(name);

        let mut versions = BTreeMap::new();
        let mut any_change = false;
        let mut is_new = before_versions.is_none();

        for (distro, (version, release, _owner)) in after_versions {
            let changed = match before_versions.and_then(|bv| bv.get(distro)) {
                Some((old_ver, _, _)) => old_ver != version,
                None => {
                    if before_versions.is_some() {
                        true
                    } else {
                        is_new = true;
                        true
                    }
                }
            };

            if changed {
                any_change = true;
                versions.insert(
                    distro.clone(),
                    BuildVersion {
                        version: version.clone(),
                        release: release.clone(),
                        tag: String::new(),
                    },
                );
            }
        }

        if !any_change {
            continue;
        }

        let owner = after_versions
            .values()
            .next()
            .map(|(_, _, o)| o.clone())
            .unwrap_or_default();

        packages.insert(
            name.clone(),
            PackageEntry {
                name: name.clone(),
                change: if is_new {
                    ChangeKind::New
                } else {
                    ChangeKind::Updated
                },
                versions,
                owner,
            },
        );
    }

    packages
}

/// Prettify a group key: replace hyphens with spaces, capitalize
/// each word.
fn prettify_group_name(name: &str) -> String {
    name.split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    format!("{upper}{}", chars.as_str())
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Format the Koji report as Markdown in the quarterly report style.
///
/// Groups by config groups, merges versions across distros when
/// they match.
pub fn format_markdown(
    report: &KojiReport,
    detailed: bool,
    groups: &BTreeMap<String, crate::config::GroupConfig>,
    heading_suffix: Option<&str>,
) -> String {
    let mut out = String::new();

    let heading = match heading_suffix {
        Some(s) => format!("## Koji CBS ({s})\n\n"),
        None => "## Koji CBS\n\n".to_string(),
    };

    if report.packages.is_empty() {
        out.push_str("No Koji CBS packages found.\n\n");
        return out;
    }

    out.push_str(&heading);

    let new_count = report
        .packages
        .values()
        .filter(|e| e.change == ChangeKind::New)
        .count();
    let updated_count = report
        .packages
        .values()
        .filter(|e| e.change == ChangeKind::Updated)
        .count();

    if !detailed {
        out.push_str(&format!(
            "**{}** new package(s), **{}** updated package(s).\n\n",
            new_count, updated_count
        ));
        return out;
    }

    // Categorize by group.
    let mut grouped: BTreeMap<&str, Vec<&PackageEntry>> = BTreeMap::new();
    let mut ungrouped: Vec<&PackageEntry> = Vec::new();

    for entry in report.packages.values() {
        let group = groups
            .iter()
            .find(|(_, gc)| gc.packages.iter().any(|p| p == &entry.name))
            .map(|(name, _)| name.as_str());

        match group {
            Some(g) => grouped.entry(g).or_default().push(entry),
            None => ungrouped.push(entry),
        }
    }

    // Format a single package entry.
    let format_entry = |out: &mut String, entry: &PackageEntry| {
        let versions: Vec<&BuildVersion> = entry.versions.values().collect();
        let distros: Vec<&str> = entry.versions.keys().map(|s| s.as_str()).collect();
        let all_same = versions.windows(2).all(|w| w[0].version == w[1].version);
        let is_new = entry.change == ChangeKind::New;

        if all_same && !versions.is_empty() {
            if is_new {
                out.push_str(&format!(
                    "- **{}** {} added\n",
                    entry.name, versions[0].version
                ));
            } else {
                out.push_str(&format!(
                    "- **{}** rebased to {}\n",
                    entry.name, versions[0].version
                ));
            }
        } else if is_new {
            out.push_str(&format!("- **{}** ", entry.name));
            let parts: Vec<String> = distros
                .iter()
                .zip(versions.iter())
                .map(|(d, v)| format!("{} ({})", v.version, d))
                .collect();
            out.push_str(&format!("{} added\n", parts.join(", ")));
        } else {
            out.push_str(&format!("- **{}** rebased to ", entry.name));
            let parts: Vec<String> = distros
                .iter()
                .zip(versions.iter())
                .map(|(d, v)| format!("{} ({})", v.version, d))
                .collect();
            out.push_str(&format!("{}\n", parts.join(", ")));
        }
    };

    // Helper: format a section (new or updated) with grouped entries.
    let format_section = |out: &mut String,
                          heading: &str,
                          grouped: &BTreeMap<&str, Vec<&PackageEntry>>,
                          ungrouped: &[&PackageEntry]| {
        if grouped.is_empty() && ungrouped.is_empty() {
            return;
        }
        out.push_str(&format!("### {heading}\n\n"));

        for (group_key, entries) in grouped {
            let gc = &groups[*group_key];
            let label = prettify_group_name(group_key);
            out.push_str(&format!("**{label}:**\n"));
            if let Some(ref desc) = gc.description {
                out.push_str(&format!("{desc}\n"));
            }
            out.push('\n');
            for entry in entries {
                format_entry(out, entry);
            }
            out.push('\n');
        }

        if !ungrouped.is_empty() {
            if !grouped.is_empty() {
                out.push_str("**Other:**\n\n");
            }
            for entry in ungrouped {
                format_entry(out, entry);
            }
            out.push('\n');
        }
    };

    // Split into new and updated, then group each.
    let split_and_group =
        |kind: ChangeKind| -> (BTreeMap<&str, Vec<&PackageEntry>>, Vec<&PackageEntry>) {
            let mut grp: BTreeMap<&str, Vec<&PackageEntry>> = BTreeMap::new();
            let mut ungrp: Vec<&PackageEntry> = Vec::new();

            for entry in report.packages.values().filter(|e| e.change == kind) {
                let group = groups
                    .iter()
                    .find(|(_, gc)| gc.packages.iter().any(|p| p == &entry.name))
                    .map(|(name, _)| name.as_str());
                match group {
                    Some(g) => grp.entry(g).or_default().push(entry),
                    None => ungrp.push(entry),
                }
            }

            (grp, ungrp)
        };

    let (new_grouped, new_ungrouped) = split_and_group(ChangeKind::New);
    let (upd_grouped, upd_ungrouped) = split_and_group(ChangeKind::Updated);

    format_section(&mut out, "New packages", &new_grouped, &new_ungrouped);
    format_section(&mut out, "Package updates", &upd_grouped, &upd_ungrouped);

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distro_from_tag_el9() {
        assert_eq!(distro_from_tag("hyperscale9s-packages-main-release"), "el9");
    }

    #[test]
    fn distro_from_tag_el10() {
        assert_eq!(
            distro_from_tag("hyperscale10-packages-main-release"),
            "el10"
        );
    }

    #[test]
    fn distro_from_tag_el10_stream() {
        assert_eq!(
            distro_from_tag("hyperscale10s-packages-main-release"),
            "el10"
        );
    }

    #[test]
    fn distro_from_tag_proposed_updates() {
        assert_eq!(
            distro_from_tag("proposed_updates9s-packages-main-release"),
            "el9"
        );
    }

    #[test]
    fn format_same_version_across_distros() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "systemd".to_string(),
            PackageEntry {
                name: "systemd".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([
                    (
                        "el9".to_string(),
                        BuildVersion {
                            version: "256.12".to_string(),
                            release: "1.hs.el9".to_string(),
                            tag: "tag9".to_string(),
                        },
                    ),
                    (
                        "el10".to_string(),
                        BuildVersion {
                            version: "256.12".to_string(),
                            release: "1.hs.el10".to_string(),
                            tag: "tag10".to_string(),
                        },
                    ),
                ]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("**systemd** rebased to 256.12"));
        // Should NOT contain distro-specific versions.
        assert!(!md.contains("(el9)"));
    }

    #[test]
    fn format_different_versions() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "mesa".to_string(),
            PackageEntry {
                name: "mesa".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([
                    (
                        "el9".to_string(),
                        BuildVersion {
                            version: "24.0".to_string(),
                            release: "1.hs.el9".to_string(),
                            tag: "tag9".to_string(),
                        },
                    ),
                    (
                        "el10".to_string(),
                        BuildVersion {
                            version: "24.3".to_string(),
                            release: "1.hs.el10".to_string(),
                            tag: "tag10".to_string(),
                        },
                    ),
                ]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("**mesa** rebased to"));
        assert!(md.contains("(el9)"));
        assert!(md.contains("(el10)"));
    }

    #[test]
    fn prettify_group_names() {
        assert_eq!(
            prettify_group_name("hardware-enablement"),
            "Hardware Enablement"
        );
        assert_eq!(prettify_group_name("developer-tools"), "Developer Tools");
        assert_eq!(prettify_group_name("benchmarking"), "Benchmarking");
    }

    #[test]
    fn group_description_overrides_name() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "mesa".to_string(),
            PackageEntry {
                name: "mesa".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "24.3".to_string(),
                        release: "1.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let groups = BTreeMap::from([(
            "hw".to_string(),
            crate::config::GroupConfig {
                description: Some("Hardware enablement and GPU support".to_string()),
                packages: vec!["mesa".to_string()],
            },
        )]);
        let md = format_markdown(&report, true, &groups, None);
        assert!(md.contains("**Hw:**"));
        assert!(md.contains("Hardware enablement and GPU support"));
    }

    #[test]
    fn group_without_description_prettifies() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "fish".to_string(),
            PackageEntry {
                name: "fish".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "4.0".to_string(),
                        release: "1.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let groups = BTreeMap::from([(
            "developer-tools".to_string(),
            crate::config::GroupConfig {
                description: None,
                packages: vec!["fish".to_string()],
            },
        )]);
        let md = format_markdown(&report, true, &groups, None);
        assert!(md.contains("**Developer Tools:**"));
    }

    #[test]
    fn format_new_packages() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "neovim".to_string(),
            PackageEntry {
                name: "neovim".to_string(),
                change: ChangeKind::New,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "0.10.0".to_string(),
                        release: "1.hs.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("### New packages"));
        assert!(md.contains("**neovim** 0.10.0 added"));
        assert!(!md.contains("rebased to"));
    }

    #[test]
    fn format_mixed_new_and_updated() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "fish".to_string(),
            PackageEntry {
                name: "fish".to_string(),
                change: ChangeKind::New,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "4.0".to_string(),
                        release: "1.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        packages.insert(
            "systemd".to_string(),
            PackageEntry {
                name: "systemd".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "256.12".to_string(),
                        release: "1.hs.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("### New packages"));
        assert!(md.contains("**fish** 4.0 added"));
        assert!(md.contains("### Package updates"));
        assert!(md.contains("**systemd** rebased to 256.12"));
    }

    #[test]
    fn format_summary_only() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "foo".to_string(),
            PackageEntry {
                name: "foo".to_string(),
                change: ChangeKind::New,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "1.0".to_string(),
                        release: "1.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        packages.insert(
            "bar".to_string(),
            PackageEntry {
                name: "bar".to_string(),
                change: ChangeKind::Updated,
                versions: BTreeMap::from([(
                    "el10".to_string(),
                    BuildVersion {
                        version: "2.0".to_string(),
                        release: "1.el10".to_string(),
                        tag: "tag".to_string(),
                    },
                )]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, false, &BTreeMap::new(), None);
        assert!(md.contains("**1** new package(s)"));
        assert!(md.contains("**1** updated package(s)"));
        // Summary should not have detailed sections.
        assert!(!md.contains("### New packages"));
        // Trailing blank line so the next section isn't rammed
        // up against this one.
        assert!(md.ends_with(").\n\n"));
    }

    #[test]
    fn format_empty_report() {
        let report = KojiReport {
            packages: BTreeMap::new(),
        };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("No Koji CBS packages found"));
    }

    #[test]
    fn format_new_different_versions() {
        let mut packages = BTreeMap::new();
        packages.insert(
            "mesa".to_string(),
            PackageEntry {
                name: "mesa".to_string(),
                change: ChangeKind::New,
                versions: BTreeMap::from([
                    (
                        "el9".to_string(),
                        BuildVersion {
                            version: "24.0".to_string(),
                            release: "1.el9".to_string(),
                            tag: "tag".to_string(),
                        },
                    ),
                    (
                        "el10".to_string(),
                        BuildVersion {
                            version: "24.3".to_string(),
                            release: "1.el10".to_string(),
                            tag: "tag".to_string(),
                        },
                    ),
                ]),
                owner: "user".to_string(),
            },
        );
        let report = KojiReport { packages };
        let md = format_markdown(&report, true, &BTreeMap::new(), None);
        assert!(md.contains("**mesa** 24.3 (el10), 24.0 (el9) added"));
    }

    #[test]
    fn merge_builds_keeps_newest() {
        let mut snapshot = TagSnapshot::new();
        let builds = vec![
            sandogasa_koji::TaggedBuild {
                nvr: "foo-1.0-1.hs.el10".to_string(),
                tag: "tag1".to_string(),
                owner: "user1".to_string(),
            },
            sandogasa_koji::TaggedBuild {
                nvr: "foo-2.0-1.hs.el10".to_string(),
                tag: "tag2".to_string(),
                owner: "user2".to_string(),
            },
        ];
        merge_builds_into_snapshot(&mut snapshot, &builds, "el10");
        let (ver, _, owner) = &snapshot["foo"]["el10"];
        assert_eq!(ver, "2.0");
        assert_eq!(owner, "user2");
    }

    #[test]
    fn merge_builds_multiple_distros() {
        let mut snapshot = TagSnapshot::new();
        let el9 = vec![sandogasa_koji::TaggedBuild {
            nvr: "foo-1.0-1.el9".to_string(),
            tag: "tag9".to_string(),
            owner: "user".to_string(),
        }];
        let el10 = vec![sandogasa_koji::TaggedBuild {
            nvr: "foo-2.0-1.el10".to_string(),
            tag: "tag10".to_string(),
            owner: "user".to_string(),
        }];
        merge_builds_into_snapshot(&mut snapshot, &el9, "el9");
        merge_builds_into_snapshot(&mut snapshot, &el10, "el10");
        assert_eq!(snapshot["foo"]["el9"].0, "1.0");
        assert_eq!(snapshot["foo"]["el10"].0, "2.0");
    }

    #[test]
    fn diff_detects_new_package() {
        let before = TagSnapshot::new();
        let mut after = TagSnapshot::new();
        after.insert(
            "foo".to_string(),
            BTreeMap::from([(
                "el10".to_string(),
                ("1.0".to_string(), "1.el10".to_string(), "user".to_string()),
            )]),
        );
        let result = diff_snapshots(&before, &after);
        assert_eq!(result.len(), 1);
        assert_eq!(result["foo"].change, ChangeKind::New);
    }

    #[test]
    fn diff_detects_updated_package() {
        let mut before = TagSnapshot::new();
        before.insert(
            "foo".to_string(),
            BTreeMap::from([(
                "el10".to_string(),
                ("1.0".to_string(), "1.el10".to_string(), "user".to_string()),
            )]),
        );
        let mut after = TagSnapshot::new();
        after.insert(
            "foo".to_string(),
            BTreeMap::from([(
                "el10".to_string(),
                ("2.0".to_string(), "1.el10".to_string(), "user".to_string()),
            )]),
        );
        let result = diff_snapshots(&before, &after);
        assert_eq!(result.len(), 1);
        assert_eq!(result["foo"].change, ChangeKind::Updated);
        assert_eq!(result["foo"].versions["el10"].version, "2.0");
    }

    #[test]
    fn diff_ignores_unchanged() {
        let mut before = TagSnapshot::new();
        before.insert(
            "foo".to_string(),
            BTreeMap::from([(
                "el10".to_string(),
                ("1.0".to_string(), "1.el10".to_string(), "user".to_string()),
            )]),
        );
        let result = diff_snapshots(&before, &before);
        assert!(result.is_empty());
    }

    #[test]
    fn diff_new_distro_is_update() {
        let mut before = TagSnapshot::new();
        before.insert(
            "foo".to_string(),
            BTreeMap::from([(
                "el9".to_string(),
                ("1.0".to_string(), "1.el9".to_string(), "user".to_string()),
            )]),
        );
        let mut after = TagSnapshot::new();
        after.insert(
            "foo".to_string(),
            BTreeMap::from([
                (
                    "el9".to_string(),
                    ("1.0".to_string(), "1.el9".to_string(), "user".to_string()),
                ),
                (
                    "el10".to_string(),
                    ("1.0".to_string(), "1.el10".to_string(), "user".to_string()),
                ),
            ]),
        );
        let result = diff_snapshots(&before, &after);
        assert_eq!(result.len(), 1);
        // Existing in el9 (unchanged) but new in el10 → update, not new.
        assert_eq!(result["foo"].change, ChangeKind::Updated);
        // Only the changed distro appears in versions.
        assert!(result["foo"].versions.contains_key("el10"));
        assert!(!result["foo"].versions.contains_key("el9"));
    }
}
