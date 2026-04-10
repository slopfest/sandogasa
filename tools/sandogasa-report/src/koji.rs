// SPDX-License-Identifier: MPL-2.0

//! Koji CBS reporting — query packages tagged in CentOS SIG release tags.

use std::collections::BTreeMap;

use crate::brace;
use crate::config::DomainConfig;

/// Koji CBS report for a domain.
pub struct KojiReport {
    /// Tag → list of builds.
    pub tags: BTreeMap<String, Vec<sandogasa_koji::TaggedBuild>>,
    /// Total builds across all tags.
    pub total_builds: usize,
    /// Unique package names.
    pub unique_packages: usize,
}

/// Run Koji CBS reporting for a domain config.
pub fn koji_report(
    domain: &DomainConfig,
    user: Option<&str>,
    verbose: bool,
) -> Result<KojiReport, String> {
    let profile = domain.koji_profile.as_deref();

    // Expand brace patterns in tag names.
    let tags: Vec<String> = domain
        .koji_tags
        .iter()
        .flat_map(|pattern| brace::expand(pattern))
        .collect();

    if tags.is_empty() {
        return Err("no koji_tags configured for this domain".to_string());
    }

    let mut all_tags: BTreeMap<String, Vec<sandogasa_koji::TaggedBuild>> = BTreeMap::new();
    let mut total = 0;
    let mut packages: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    for tag in &tags {
        if verbose {
            eprintln!("[koji] querying tag {tag}");
        }

        let mut builds = match sandogasa_koji::list_tagged(tag, profile) {
            Ok(b) => b,
            Err(e) => {
                if verbose {
                    eprintln!("[koji] warning: {e}");
                }
                continue;
            }
        };

        // Filter by owner if user is specified.
        if let Some(user) = user {
            builds.retain(|b| b.owner == user);
        }

        for b in &builds {
            if let Some(name) = sandogasa_koji::parse_nvr_name(&b.nvr) {
                packages.insert(name.to_string());
            }
        }

        total += builds.len();

        if !builds.is_empty() {
            all_tags.insert(tag.clone(), builds);
        }
    }

    Ok(KojiReport {
        tags: all_tags,
        total_builds: total,
        unique_packages: packages.len(),
    })
}

/// Format the Koji report as Markdown.
pub fn format_markdown(
    report: &KojiReport,
    detailed: bool,
    groups: &BTreeMap<String, Vec<String>>,
) -> String {
    let mut out = String::new();

    out.push_str("## Koji CBS\n\n");
    out.push_str(&format!(
        "**{}** build(s) across **{}** unique package(s) in **{}** tag(s).\n\n",
        report.total_builds,
        report.unique_packages,
        report.tags.len(),
    ));

    if !detailed {
        // Summary: per-tag counts.
        for (tag, builds) in &report.tags {
            out.push_str(&format!("- **{tag}**: {} build(s)\n", builds.len()));
        }
        return out;
    }

    // Detailed: list builds per tag, grouped if config has groups.
    for (tag, builds) in &report.tags {
        out.push_str(&format!("### {tag}\n\n"));

        if groups.is_empty() {
            for b in builds {
                out.push_str(&format!("- {} ({})\n", b.nvr, b.owner));
            }
        } else {
            // Categorize builds by group.
            let mut grouped: BTreeMap<&str, Vec<&sandogasa_koji::TaggedBuild>> = BTreeMap::new();
            let mut ungrouped: Vec<&sandogasa_koji::TaggedBuild> = Vec::new();

            for b in builds {
                let pkg_name = sandogasa_koji::parse_nvr_name(&b.nvr).unwrap_or(&b.nvr);
                let group = groups
                    .iter()
                    .find(|(_, pkgs)| pkgs.iter().any(|p| p == pkg_name))
                    .map(|(name, _)| name.as_str());

                match group {
                    Some(g) => grouped.entry(g).or_default().push(b),
                    None => ungrouped.push(b),
                }
            }

            for (group, builds) in &grouped {
                out.push_str(&format!("**{group}**:\n"));
                for b in builds {
                    out.push_str(&format!("- {} ({})\n", b.nvr, b.owner));
                }
                out.push('\n');
            }

            if !ungrouped.is_empty() {
                if !grouped.is_empty() {
                    out.push_str("**Other**:\n");
                }
                for b in &ungrouped {
                    out.push_str(&format!("- {} ({})\n", b.nvr, b.owner));
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

    #[test]
    fn format_summary() {
        let report = KojiReport {
            tags: BTreeMap::from([(
                "test-tag".to_string(),
                vec![sandogasa_koji::TaggedBuild {
                    nvr: "foo-1.0-1.el10".to_string(),
                    tag: "test-tag".to_string(),
                    owner: "user".to_string(),
                }],
            )]),
            total_builds: 1,
            unique_packages: 1,
        };
        let md = format_markdown(&report, false, &BTreeMap::new());
        assert!(md.contains("**1** build(s)"));
        assert!(md.contains("- **test-tag**: 1 build(s)"));
    }

    #[test]
    fn format_detailed_with_groups() {
        let report = KojiReport {
            tags: BTreeMap::from([(
                "test-tag".to_string(),
                vec![
                    sandogasa_koji::TaggedBuild {
                        nvr: "mesa-24.0-1.el10".to_string(),
                        tag: "test-tag".to_string(),
                        owner: "user".to_string(),
                    },
                    sandogasa_koji::TaggedBuild {
                        nvr: "fish-4.0-1.el10".to_string(),
                        tag: "test-tag".to_string(),
                        owner: "user".to_string(),
                    },
                ],
            )]),
            total_builds: 2,
            unique_packages: 2,
        };
        let groups = BTreeMap::from([("hardware".to_string(), vec!["mesa".to_string()])]);
        let md = format_markdown(&report, true, &groups);
        assert!(md.contains("**hardware**:"));
        assert!(md.contains("mesa-24.0-1.el10"));
        assert!(md.contains("**Other**:"));
        assert!(md.contains("fish-4.0-1.el10"));
    }
}
