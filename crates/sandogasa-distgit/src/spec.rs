// SPDX-License-Identifier: MPL-2.0

/// Extract the package name from the `Name:` field in a spec file.
pub fn extract_package_name(spec: &str) -> Option<String> {
    for line in spec.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("Name:") {
            let name = rest.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Extract shipped binary names from `%{_bindir}` and `%{_libexecdir}` entries
/// in the `%files` sections of an RPM spec file.
///
/// Returns the final path component of each binary, with `%{name}` expanded
/// to the package name from the `Name:` field.
pub fn shipped_binaries(spec: &str) -> Vec<String> {
    let name = extract_package_name(spec);
    let mut binaries = Vec::new();
    let mut in_files = false;

    for line in spec.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        if trimmed.starts_with("%files") {
            in_files = true;
            continue;
        }

        if in_files && is_section_start(trimmed) {
            in_files = false;
        }

        if !in_files {
            continue;
        }

        if let Some(binary) = extract_binary(trimmed, name.as_deref()) {
            if !binaries.contains(&binary) {
                binaries.push(binary);
            }
        }
    }

    binaries
}

/// Check if a line starts a new RPM spec section (ending the current %files block).
///
/// Lines starting with `%` that are NOT file-list entries indicate a new section.
fn is_section_start(line: &str) -> bool {
    if !line.starts_with('%') {
        return false;
    }
    // Macro expansions like %{_bindir} are file entries, not section starts
    if line.starts_with("%{") || line.starts_with("%(") {
        return false;
    }
    // These are valid directives within %files sections
    const FILES_DIRECTIVES: &[&str] = &[
        "%license", "%doc", "%dir", "%config", "%ghost", "%defattr", "%attr", "%verify", "%caps",
        "%exclude",
    ];
    !FILES_DIRECTIVES.iter().any(|d| line.starts_with(d))
}

/// Extract a binary name from a %files line containing %{_bindir} or %{_libexecdir}.
fn extract_binary(line: &str, package_name: Option<&str>) -> Option<String> {
    const PREFIXES: &[&str] = &["%{_bindir}/", "%{_libexecdir}/"];

    for prefix in PREFIXES {
        if let Some(pos) = line.find(prefix) {
            let rest = &line[pos + prefix.len()..];
            // Take the first path component (stop at whitespace or /)
            let name_part = rest.split_whitespace().next().unwrap_or(rest);
            // For paths like %{_libexecdir}/%{name}/helper, take the last component
            let binary = name_part.rsplit('/').next().unwrap_or(name_part);
            // Skip globs
            if binary.contains('*') {
                return None;
            }
            let resolved = match package_name {
                Some(pkg) => binary.replace("%{name}", pkg),
                None => binary.to_string(),
            };
            if !resolved.is_empty() {
                return Some(resolved);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- extract_package_name ----

    #[test]
    fn extract_name_standard() {
        let spec = "Name:    pcem\nVersion: 17\n";
        assert_eq!(extract_package_name(spec), Some("pcem".to_string()));
    }

    #[test]
    fn extract_name_no_name_field() {
        let spec = "Version: 1.0\nRelease: 1\n";
        assert_eq!(extract_package_name(spec), None);
    }

    #[test]
    fn extract_name_empty_value() {
        let spec = "Name:\nVersion: 1.0\n";
        assert_eq!(extract_package_name(spec), None);
    }

    #[test]
    fn extract_name_with_leading_spaces() {
        let spec = "  Name:   mypackage  \n";
        assert_eq!(extract_package_name(spec), Some("mypackage".to_string()));
    }

    // ---- shipped_binaries ----

    #[test]
    fn shipped_binaries_bindir_with_name_macro() {
        let spec = "\
Name: pcem
Version: 17

%files
%license COPYING
%{_bindir}/%{name}
%{_datadir}/%{name}/roms/
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["pcem"]);
    }

    #[test]
    fn shipped_binaries_multiple_entries() {
        let spec = "\
Name: libxml2
Version: 2.12

%files
%{_bindir}/xmllint
%{_bindir}/xmlcatalog

%files devel
%{_bindir}/xml2-config
%{_libexecdir}/libxml2/helper
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["xmllint", "xmlcatalog", "xml2-config", "helper"]);
    }

    #[test]
    fn shipped_binaries_libexecdir() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
%{_libexecdir}/%{name}/my-helper
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["my-helper"]);
    }

    #[test]
    fn shipped_binaries_skips_globs() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
%{_bindir}/*
%{_bindir}/real-tool
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["real-tool"]);
    }

    #[test]
    fn shipped_binaries_stops_at_new_section() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
%{_bindir}/tool1

%changelog
* Mon Jan 01 2026 Someone
- Initial build
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["tool1"]);
    }

    #[test]
    fn shipped_binaries_ignores_datadir() {
        let spec = "\
Name: pcem
Version: 17

%files
%{_bindir}/%{name}
%{_datadir}/%{name}/roms/
%{_datadir}/applications/pcem.desktop
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["pcem"]);
    }

    #[test]
    fn shipped_binaries_empty_files_section() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
%license COPYING
%doc README
";
        let bins = shipped_binaries(spec);
        assert!(bins.is_empty());
    }

    #[test]
    fn shipped_binaries_no_files_section() {
        let spec = "\
Name: mypkg
Version: 1.0

%description
A package.
";
        let bins = shipped_binaries(spec);
        assert!(bins.is_empty());
    }

    #[test]
    fn shipped_binaries_no_duplicates() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
%{_bindir}/tool

%files extras
%{_bindir}/tool
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["tool"]);
    }

    #[test]
    fn shipped_binaries_comments_ignored() {
        let spec = "\
Name: mypkg
Version: 1.0

%files
# %{_bindir}/commented-out
%{_bindir}/real
";
        let bins = shipped_binaries(spec);
        assert_eq!(bins, vec!["real"]);
    }

    // ---- is_section_start ----

    #[test]
    fn section_start_prep() {
        assert!(is_section_start("%prep"));
    }

    #[test]
    fn section_start_build() {
        assert!(is_section_start("%build"));
    }

    #[test]
    fn section_start_install() {
        assert!(is_section_start("%install"));
    }

    #[test]
    fn section_start_changelog() {
        assert!(is_section_start("%changelog"));
    }

    #[test]
    fn section_start_package() {
        assert!(is_section_start("%package devel"));
    }

    #[test]
    fn section_start_files_is_new_section() {
        assert!(is_section_start("%files extras"));
    }

    #[test]
    fn not_section_start_license() {
        assert!(!is_section_start("%license COPYING"));
    }

    #[test]
    fn not_section_start_doc() {
        assert!(!is_section_start("%doc README"));
    }

    #[test]
    fn not_section_start_dir() {
        assert!(!is_section_start("%dir /some/path"));
    }

    #[test]
    fn not_section_start_config() {
        assert!(!is_section_start("%config(noreplace) /etc/foo"));
    }

    #[test]
    fn not_section_start_macro() {
        assert!(!is_section_start("%{_bindir}/foo"));
    }

    #[test]
    fn not_section_start_non_percent() {
        assert!(!is_section_start("/usr/bin/foo"));
    }

    // ---- extract_binary ----

    #[test]
    fn extract_binary_bindir() {
        assert_eq!(
            extract_binary("%{_bindir}/xmllint", None),
            Some("xmllint".to_string())
        );
    }

    #[test]
    fn extract_binary_bindir_with_name() {
        assert_eq!(
            extract_binary("%{_bindir}/%{name}", Some("pcem")),
            Some("pcem".to_string())
        );
    }

    #[test]
    fn extract_binary_libexecdir_nested() {
        assert_eq!(
            extract_binary("%{_libexecdir}/%{name}/helper", Some("mypkg")),
            Some("helper".to_string())
        );
    }

    #[test]
    fn extract_binary_no_match() {
        assert_eq!(
            extract_binary("%{_datadir}/%{name}/roms/", Some("pcem")),
            None
        );
    }

    #[test]
    fn extract_binary_glob_returns_none() {
        assert_eq!(extract_binary("%{_bindir}/*", None), None);
    }

    #[test]
    fn extract_binary_plain_path() {
        assert_eq!(extract_binary("/usr/bin/foo", None), None);
    }
}
