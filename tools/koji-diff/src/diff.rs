// SPDX-License-Identifier: MPL-2.0

//! Parse installed packages from root.log and compute diffs.

use std::collections::BTreeMap;

use serde::Serialize;

/// An installed package parsed from root.log.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct InstalledPackage {
    pub name: String,
    /// epoch:version-release (epoch omitted when 0).
    pub evr: String,
    pub arch: String,
    /// Full NEVRA string as it appeared in the log.
    pub full: String,
}

/// Diff between two sets of installed packages.
#[derive(Debug, Serialize)]
pub struct PackageDiff {
    pub added: Vec<InstalledPackage>,
    pub removed: Vec<InstalledPackage>,
    pub changed: Vec<PackageChange>,
    pub unchanged_count: usize,
}

/// A package whose version changed between two builds.
#[derive(Debug, Serialize)]
pub struct PackageChange {
    pub name: String,
    pub arch: String,
    pub old_evr: String,
    pub new_evr: String,
}

/// Known RPM architectures for NEVRA parsing validation.
const VALID_ARCHES: &[&str] = &[
    "x86_64", "aarch64", "i686", "ppc64le", "s390x", "armv7hl", "noarch", "riscv64", "i386", "i586",
];

/// Parse installed packages from a root.log file.
///
/// Parses the DNF transaction table that both DNF4 and DNF5 produce,
/// with columns: Package, Arch, Version, Repo, Size.  Lines are
/// recognised by having a known RPM arch as the second
/// whitespace-delimited token.
///
/// Also handles mock's `DEBUG util.py:NNN:` prefix.
pub fn parse_installed_packages(root_log: &str) -> Vec<InstalledPackage> {
    let mut packages = Vec::new();

    for line in root_log.lines() {
        let content = strip_mock_prefix(line);
        let trimmed = content.trim();

        // Try to parse as a DNF transaction table row.
        // Format: " name  arch  version  repo  size"
        if let Some(pkg) = parse_transaction_table_row(trimmed) {
            packages.push(pkg);
        }
    }

    // Deduplicate by (name, arch), keeping the last occurrence (which is the
    // latest version if the same package was upgraded).
    let mut seen: BTreeMap<(String, String), InstalledPackage> = BTreeMap::new();
    for pkg in packages {
        seen.insert((pkg.name.clone(), pkg.arch.clone()), pkg);
    }

    let mut result: Vec<_> = seen.into_values().collect();
    result.sort_by(|a, b| a.full.cmp(&b.full));
    result
}

/// Parse a DNF transaction table row.
///
/// Format: `name  arch  [epoch:]version-release  repo  size`
///
/// Example: `libgcc  x86_64  15.2.1-7.fc42  koji-build-xxxx  268 k`
///
/// Recognised by having a valid RPM arch as the second token.
fn parse_transaction_table_row(line: &str) -> Option<InstalledPackage> {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    if tokens.len() < 4 {
        return None;
    }

    let name = tokens[0];
    let arch = tokens[1];
    let evr = tokens[2]; // [epoch:]version-release

    if !VALID_ARCHES.contains(&arch) {
        return None;
    }

    // The version-release must contain a '-' (e.g. "15.2.1-7.fc42").
    if !evr.contains('-') {
        return None;
    }

    // Reject obvious non-package lines (e.g. table headers).
    if name == "Package" || name.starts_with('=') {
        return None;
    }

    let full = format!("{name}-{evr}.{arch}");

    Some(InstalledPackage {
        name: name.to_string(),
        evr: evr.to_string(),
        arch: arch.to_string(),
        full,
    })
}

fn strip_mock_prefix(line: &str) -> &str {
    // Look for the pattern: `DEBUG <source>:  <content>`
    // The source part is like `util.py:446:` or `mockbuild.py:123:`
    // Mock uses two spaces after the final colon.
    if let Some(debug_start) = line.find("DEBUG ") {
        let after_debug = &line[debug_start + 6..];
        // Find the first ": " after the module reference.
        if let Some(colon_pos) = after_debug.find(": ") {
            let rest = &after_debug[colon_pos + 2..];
            // Skip any additional whitespace (mock uses ":  ").
            return rest.trim_start();
        }
    }
    line
}

/// Parse a NEVRA string into an [`InstalledPackage`].
///
/// NEVRA format: `name-[epoch:]version-release.arch`
///
/// Examples:
/// - `gcc-14.2.1-1.fc42.x86_64`
/// - `glibc-2:2.40-1.fc42.x86_64`
pub fn parse_nevra(s: &str) -> Option<InstalledPackage> {
    let s = s.trim();
    if s.is_empty() || !s.contains('-') || !s.contains('.') {
        return None;
    }

    // Split off .arch (last dot).
    let dot_pos = s.rfind('.')?;
    let arch = &s[dot_pos + 1..];
    if !VALID_ARCHES.contains(&arch) {
        return None;
    }

    let name_ver_rel = &s[..dot_pos];

    // Split off -release (last dash).
    let rel_dash = name_ver_rel.rfind('-')?;
    let release = &name_ver_rel[rel_dash + 1..];
    let name_ver = &name_ver_rel[..rel_dash];

    // Split off -[epoch:]version (second-to-last dash).
    let ver_dash = name_ver.rfind('-')?;
    let version = &name_ver[ver_dash + 1..];
    let name = &name_ver[..ver_dash];

    if name.is_empty() || version.is_empty() || release.is_empty() {
        return None;
    }

    let evr = format!("{version}-{release}");

    Some(InstalledPackage {
        name: name.to_string(),
        evr,
        arch: arch.to_string(),
        full: s.to_string(),
    })
}

/// Compute the diff between two sets of installed packages.
pub fn diff_packages(old: &[InstalledPackage], new: &[InstalledPackage]) -> PackageDiff {
    let old_map: BTreeMap<(&str, &str), &InstalledPackage> = old
        .iter()
        .map(|p| ((p.name.as_str(), p.arch.as_str()), p))
        .collect();
    let new_map: BTreeMap<(&str, &str), &InstalledPackage> = new
        .iter()
        .map(|p| ((p.name.as_str(), p.arch.as_str()), p))
        .collect();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged_count = 0;

    for (key, old_pkg) in &old_map {
        match new_map.get(key) {
            None => removed.push((*old_pkg).clone()),
            Some(new_pkg) => {
                if old_pkg.evr != new_pkg.evr {
                    changed.push(PackageChange {
                        name: old_pkg.name.clone(),
                        arch: old_pkg.arch.clone(),
                        old_evr: old_pkg.evr.clone(),
                        new_evr: new_pkg.evr.clone(),
                    });
                } else {
                    unchanged_count += 1;
                }
            }
        }
    }

    for (key, new_pkg) in &new_map {
        if !old_map.contains_key(key) {
            added.push((*new_pkg).clone());
        }
    }

    PackageDiff {
        added,
        removed,
        changed,
        unchanged_count,
    }
}

/// Print the package diff in human-readable format.
pub fn print_diff(diff: &PackageDiff, ref1_label: &str, ref2_label: &str) {
    if diff.added.is_empty() && diff.removed.is_empty() && diff.changed.is_empty() {
        println!("No differences in installed packages.");
        println!("({} packages unchanged)", diff.unchanged_count);
        return;
    }

    println!("Buildroot package changes (root.log):");

    if !diff.changed.is_empty() {
        println!("  Changed ({}):", diff.changed.len());
        for pkg in &diff.changed {
            println!(
                "    {}.{}: {} -> {}",
                pkg.name, pkg.arch, pkg.old_evr, pkg.new_evr
            );
        }
    }

    if !diff.added.is_empty() {
        println!("  Added in {} ({}):", ref2_label, diff.added.len());
        for pkg in &diff.added {
            println!("    + {}", pkg.full);
        }
    }

    if !diff.removed.is_empty() {
        println!("  Removed vs {} ({}):", ref1_label, diff.removed.len());
        for pkg in &diff.removed {
            println!("    - {}", pkg.full);
        }
    }

    println!("  ({} packages unchanged)", diff.unchanged_count);
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- NEVRA parsing ---

    #[test]
    fn test_parse_nevra_simple() {
        let pkg = parse_nevra("gcc-14.2.1-1.fc42.x86_64").unwrap();
        assert_eq!(pkg.name, "gcc");
        assert_eq!(pkg.evr, "14.2.1-1.fc42");
        assert_eq!(pkg.arch, "x86_64");
    }

    #[test]
    fn test_parse_nevra_with_epoch() {
        let pkg = parse_nevra("glibc-2:2.40-1.fc42.x86_64").unwrap();
        assert_eq!(pkg.name, "glibc");
        assert_eq!(pkg.evr, "2:2.40-1.fc42");
        assert_eq!(pkg.arch, "x86_64");
    }

    #[test]
    fn test_parse_nevra_noarch() {
        let pkg = parse_nevra("python3-setuptools-75.0-1.fc42.noarch").unwrap();
        assert_eq!(pkg.name, "python3-setuptools");
        assert_eq!(pkg.arch, "noarch");
    }

    #[test]
    fn test_parse_nevra_complex_name() {
        let pkg = parse_nevra("xorg-x11-server-Xwayland-24.1.1-1.fc42.x86_64").unwrap();
        assert_eq!(pkg.name, "xorg-x11-server-Xwayland");
        assert_eq!(pkg.evr, "24.1.1-1.fc42");
    }

    #[test]
    fn test_parse_nevra_invalid_arch() {
        assert!(parse_nevra("foo-1.0-1.fc42.badarch").is_none());
    }

    #[test]
    fn test_parse_nevra_no_dashes() {
        assert!(parse_nevra("nodashes").is_none());
    }

    #[test]
    fn test_parse_nevra_empty() {
        assert!(parse_nevra("").is_none());
    }

    #[test]
    fn test_parse_nevra_size_suffix_ignored() {
        // In some DNF output, the NEVRA might be followed by a size.
        // We only parse the first whitespace-delimited token.
        assert!(parse_nevra("123k").is_none());
    }

    // --- Mock prefix stripping ---

    #[test]
    fn test_strip_mock_prefix() {
        assert_eq!(
            strip_mock_prefix("DEBUG util.py:446:  Installed:"),
            "Installed:"
        );
    }

    #[test]
    fn test_strip_mock_prefix_no_prefix() {
        assert_eq!(strip_mock_prefix("Installed:"), "Installed:");
    }

    #[test]
    fn test_strip_mock_prefix_with_content() {
        assert_eq!(
            strip_mock_prefix("DEBUG util.py:446:    gcc-14.2.1-1.fc42.x86_64"),
            "gcc-14.2.1-1.fc42.x86_64"
        );
    }

    // --- Transaction table parsing ---

    #[test]
    fn test_parse_transaction_table_row() {
        let pkg = parse_transaction_table_row(
            "libgcc                       x86_64  14.3.1-4.4.el10              build  140 k",
        )
        .unwrap();
        assert_eq!(pkg.name, "libgcc");
        assert_eq!(pkg.arch, "x86_64");
        assert_eq!(pkg.evr, "14.3.1-4.4.el10");
        assert_eq!(pkg.full, "libgcc-14.3.1-4.4.el10.x86_64");
    }

    #[test]
    fn test_parse_transaction_table_row_with_epoch() {
        let pkg = parse_transaction_table_row(
            "dbus-libs                    x86_64  1:1.14.10-5.el10             build  156 k",
        )
        .unwrap();
        assert_eq!(pkg.name, "dbus-libs");
        assert_eq!(pkg.evr, "1:1.14.10-5.el10");
        assert_eq!(pkg.full, "dbus-libs-1:1.14.10-5.el10.x86_64");
    }

    #[test]
    fn test_parse_transaction_table_row_noarch() {
        let pkg = parse_transaction_table_row(
            "setup                        noarch  2.14.5-7.el10                build  153 k",
        )
        .unwrap();
        assert_eq!(pkg.name, "setup");
        assert_eq!(pkg.arch, "noarch");
    }

    #[test]
    fn test_parse_transaction_table_row_rejects_header() {
        assert!(
            parse_transaction_table_row(
                "Package                      Arch    Version                      Repo    Size"
            )
            .is_none()
        );
    }

    #[test]
    fn test_parse_transaction_table_row_rejects_separator() {
        assert!(
            parse_transaction_table_row(
                "================================================================================"
            )
            .is_none()
        );
    }

    #[test]
    fn test_parse_transaction_table_row_rejects_section_header() {
        assert!(parse_transaction_table_row("Installing:").is_none());
        assert!(parse_transaction_table_row("Installing dependencies:").is_none());
        assert!(parse_transaction_table_row("Transaction Summary").is_none());
    }

    // --- root.log parsing ---

    #[test]
    fn test_parse_installed_packages_dnf4_table() {
        let log = "\
DEBUG util.py:463:  Installing:
DEBUG util.py:463:   gcc                          x86_64  14.2.1-1.fc42                build   31 M
DEBUG util.py:463:  Installing dependencies:
DEBUG util.py:463:   glibc                        x86_64  2.40-1.fc42                  build   12 M
DEBUG util.py:463:   glibc-common                 x86_64  2.40-1.fc42                  build  323 k
";
        let pkgs = parse_installed_packages(log);
        assert_eq!(pkgs.len(), 3);
        assert!(pkgs.iter().any(|p| p.name == "gcc"));
        assert!(pkgs.iter().any(|p| p.name == "glibc"));
        assert!(pkgs.iter().any(|p| p.name == "glibc-common"));
    }

    #[test]
    fn test_parse_installed_packages_with_mock_prefix() {
        let log = "\
DEBUG util.py:463:   libgcc                       x86_64  14.3.1-4.4.el10              build  140 k
DEBUG util.py:463:   bash                         x86_64  5.2.26-6.el10                build  1.8 M
";
        let pkgs = parse_installed_packages(log);
        assert_eq!(pkgs.len(), 2);
        assert!(pkgs.iter().any(|p| p.name == "libgcc"));
        assert!(pkgs.iter().any(|p| p.name == "bash"));
    }

    #[test]
    fn test_parse_installed_packages_deduplication() {
        // Same package in two transaction tables → keep last version.
        let log = "\
 gcc                          x86_64  14.2.1-1.fc42                build   31 M
 gcc                          x86_64  14.2.1-2.fc42                build   31 M
";
        let pkgs = parse_installed_packages(log);
        assert_eq!(pkgs.len(), 1);
        assert_eq!(pkgs[0].evr, "14.2.1-2.fc42");
    }

    #[test]
    fn test_parse_installed_packages_empty() {
        let log = "No packages installed.\n";
        let pkgs = parse_installed_packages(log);
        assert!(pkgs.is_empty());
    }

    // --- Diff computation ---

    #[test]
    fn test_diff_identical() {
        let pkgs = vec![InstalledPackage {
            name: "gcc".into(),
            evr: "14.2.1-1.fc42".into(),
            arch: "x86_64".into(),
            full: "gcc-14.2.1-1.fc42.x86_64".into(),
        }];
        let diff = diff_packages(&pkgs, &pkgs);
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(diff.unchanged_count, 1);
    }

    #[test]
    fn test_diff_added_package() {
        let old = vec![];
        let new = vec![InstalledPackage {
            name: "gcc".into(),
            evr: "14.2.1-1.fc42".into(),
            arch: "x86_64".into(),
            full: "gcc-14.2.1-1.fc42.x86_64".into(),
        }];
        let diff = diff_packages(&old, &new);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].name, "gcc");
    }

    #[test]
    fn test_diff_removed_package() {
        let old = vec![InstalledPackage {
            name: "gcc".into(),
            evr: "14.2.1-1.fc42".into(),
            arch: "x86_64".into(),
            full: "gcc-14.2.1-1.fc42.x86_64".into(),
        }];
        let new = vec![];
        let diff = diff_packages(&old, &new);
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].name, "gcc");
    }

    #[test]
    fn test_diff_changed_version() {
        let old = vec![InstalledPackage {
            name: "gcc".into(),
            evr: "14.2.1-1.fc42".into(),
            arch: "x86_64".into(),
            full: "gcc-14.2.1-1.fc42.x86_64".into(),
        }];
        let new = vec![InstalledPackage {
            name: "gcc".into(),
            evr: "14.2.1-2.fc42".into(),
            arch: "x86_64".into(),
            full: "gcc-14.2.1-2.fc42.x86_64".into(),
        }];
        let diff = diff_packages(&old, &new);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].old_evr, "14.2.1-1.fc42");
        assert_eq!(diff.changed[0].new_evr, "14.2.1-2.fc42");
    }

    #[test]
    fn test_diff_mixed_changes() {
        let old = vec![
            InstalledPackage {
                name: "gcc".into(),
                evr: "14.2.1-1.fc42".into(),
                arch: "x86_64".into(),
                full: "gcc-14.2.1-1.fc42.x86_64".into(),
            },
            InstalledPackage {
                name: "removed-pkg".into(),
                evr: "1.0-1.fc42".into(),
                arch: "x86_64".into(),
                full: "removed-pkg-1.0-1.fc42.x86_64".into(),
            },
            InstalledPackage {
                name: "unchanged".into(),
                evr: "1.0-1.fc42".into(),
                arch: "noarch".into(),
                full: "unchanged-1.0-1.fc42.noarch".into(),
            },
        ];
        let new = vec![
            InstalledPackage {
                name: "gcc".into(),
                evr: "14.2.1-2.fc42".into(),
                arch: "x86_64".into(),
                full: "gcc-14.2.1-2.fc42.x86_64".into(),
            },
            InstalledPackage {
                name: "new-pkg".into(),
                evr: "2.0-1.fc42".into(),
                arch: "x86_64".into(),
                full: "new-pkg-2.0-1.fc42.x86_64".into(),
            },
            InstalledPackage {
                name: "unchanged".into(),
                evr: "1.0-1.fc42".into(),
                arch: "noarch".into(),
                full: "unchanged-1.0-1.fc42.noarch".into(),
            },
        ];
        let diff = diff_packages(&old, &new);
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.unchanged_count, 1);
    }
}
