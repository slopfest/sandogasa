// SPDX-License-Identifier: Apache-2.0 OR MIT

/// Known package ecosystems detectable from Fedora package names and spec files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ecosystem {
    JavaScript,
    Rust,
    Python,
}

/// Fedora package name prefixes for each ecosystem.
const JS_NAME_PREFIXES: &[&str] = &["nodejs-"];
const RUST_NAME_PREFIXES: &[&str] = &["rust-"];
const PYTHON_NAME_PREFIXES: &[&str] = &["python-"];

/// BuildRequires and macro patterns that indicate each ecosystem.
const JS_BUILD_INDICATORS: &[&str] = &["nodejs", "npm("];
const JS_MACRO_INDICATORS: &[&str] = &["%nodejs_"];

const RUST_BUILD_INDICATORS: &[&str] = &["cargo-rpm-macros", "rust-packaging"];
const RUST_MACRO_INDICATORS: &[&str] = &["%cargo_"];

const PYTHON_BUILD_INDICATORS: &[&str] = &["python3-devel", "pyproject-rpm-macros"];
const PYTHON_MACRO_INDICATORS: &[&str] = &["%py3_", "%pyproject_"];

/// Detect the ecosystem of a Fedora package.
///
/// When `spec` is `None`, only the package name is used (quick mode).
/// Returns `None` if the ecosystem cannot be determined from the
/// available information.
pub fn detect_ecosystem(name: &str, spec: Option<&str>) -> Option<Ecosystem> {
    if is_js_package(name, spec) == Some(true) {
        return Some(Ecosystem::JavaScript);
    }
    if is_rust_package(name, spec) == Some(true) {
        return Some(Ecosystem::Rust);
    }
    if is_python_package(name, spec) == Some(true) {
        return Some(Ecosystem::Python);
    }
    None
}

/// Check whether a package is a JavaScript/Node.js package.
///
/// Uses the package name for a quick heuristic, falling back to spec
/// file inspection when `spec` is provided.  Returns `None` if the
/// answer cannot be determined from the available information.
pub fn is_js_package(name: &str, spec: Option<&str>) -> Option<bool> {
    if name_matches_any(name, JS_NAME_PREFIXES) {
        return Some(true);
    }
    if name_matches_other_ecosystem(name, JS_NAME_PREFIXES) {
        return Some(false);
    }
    spec.map(|s| spec_matches(s, JS_BUILD_INDICATORS, JS_MACRO_INDICATORS))
}

/// Check whether a package is a Rust package.
pub fn is_rust_package(name: &str, spec: Option<&str>) -> Option<bool> {
    if name_matches_any(name, RUST_NAME_PREFIXES) {
        return Some(true);
    }
    if name_matches_other_ecosystem(name, RUST_NAME_PREFIXES) {
        return Some(false);
    }
    spec.map(|s| spec_matches(s, RUST_BUILD_INDICATORS, RUST_MACRO_INDICATORS))
}

/// Check whether a package is a Python package.
pub fn is_python_package(name: &str, spec: Option<&str>) -> Option<bool> {
    if name_matches_any(name, PYTHON_NAME_PREFIXES) {
        return Some(true);
    }
    if name_matches_other_ecosystem(name, PYTHON_NAME_PREFIXES) {
        return Some(false);
    }
    spec.map(|s| spec_matches(s, PYTHON_BUILD_INDICATORS, PYTHON_MACRO_INDICATORS))
}

/// Check if a package name matches any of the given prefixes.
fn name_matches_any(name: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| name.starts_with(p))
}

/// Check if a package name matches a *different* ecosystem's prefix,
/// meaning it definitely does NOT belong to the ecosystem being tested.
fn name_matches_other_ecosystem(name: &str, own_prefixes: &[&str]) -> bool {
    let all_prefixes: &[&[&str]] = &[JS_NAME_PREFIXES, RUST_NAME_PREFIXES, PYTHON_NAME_PREFIXES];
    for prefixes in all_prefixes {
        if std::ptr::eq(*prefixes, own_prefixes) {
            continue;
        }
        if prefixes.iter().any(|p| name.starts_with(p)) {
            return true;
        }
    }
    false
}

/// Check a spec file for ecosystem-specific BuildRequires and macros.
fn spec_matches(spec: &str, build_indicators: &[&str], macro_indicators: &[&str]) -> bool {
    for line in spec.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("BuildRequires:") {
            let lower = rest.to_lowercase();
            if build_indicators.iter().any(|ind| lower.contains(ind)) {
                return true;
            }
        }
        let lower = trimmed.to_lowercase();
        if macro_indicators.iter().any(|ind| lower.starts_with(ind)) {
            return true;
        }
    }
    false
}

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

        if let Some(binary) = extract_binary(trimmed, name.as_deref())
            && !binaries.contains(&binary)
        {
            binaries.push(binary);
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

    // ---- is_js_package ----

    #[test]
    fn js_package_by_name() {
        assert_eq!(is_js_package("nodejs-elliptic", None), Some(true));
    }

    #[test]
    fn js_package_by_spec_buildrequires() {
        let spec = "BuildRequires: nodejs-devel\n";
        assert_eq!(is_js_package("elliptic", Some(spec)), Some(true));
    }

    #[test]
    fn js_package_by_spec_npm() {
        let spec = "BuildRequires: npm(bn.js)\n";
        assert_eq!(is_js_package("elliptic", Some(spec)), Some(true));
    }

    #[test]
    fn js_package_by_spec_macro() {
        let spec = "%nodejs_symlink_deps\n";
        assert_eq!(is_js_package("elliptic", Some(spec)), Some(true));
    }

    #[test]
    fn js_package_rust_name_shortcircuit() {
        // Name says Rust, no spec needed
        assert_eq!(is_js_package("rust-elliptic-curve", None), Some(false));
    }

    #[test]
    fn js_package_python_name_shortcircuit() {
        assert_eq!(is_js_package("python-cryptography", None), Some(false));
    }

    #[test]
    fn js_package_unknown_name_no_spec() {
        // Can't tell without spec
        assert_eq!(is_js_package("openssl", None), None);
    }

    #[test]
    fn js_package_unknown_name_with_spec() {
        let spec = "BuildRequires: gcc\n";
        assert_eq!(is_js_package("openssl", Some(spec)), Some(false));
    }

    #[test]
    fn js_package_spec_case_insensitive() {
        let spec = "BuildRequires: NodeJS-devel\n";
        assert_eq!(is_js_package("mypkg", Some(spec)), Some(true));
    }

    #[test]
    fn js_package_requires_not_buildrequires() {
        let spec = "Requires: nodejs\nBuildRequires: cargo-rpm-macros\n";
        assert_eq!(is_js_package("mypkg", Some(spec)), Some(false));
    }

    // ---- is_rust_package ----

    #[test]
    fn rust_package_by_name() {
        assert_eq!(is_rust_package("rust-elliptic-curve", None), Some(true));
    }

    #[test]
    fn rust_package_by_spec() {
        let spec = "BuildRequires: cargo-rpm-macros >= 26\n%cargo_prep\n";
        assert_eq!(is_rust_package("elliptic-curve", Some(spec)), Some(true));
    }

    #[test]
    fn rust_package_js_name_shortcircuit() {
        assert_eq!(is_rust_package("nodejs-elliptic", None), Some(false));
    }

    #[test]
    fn rust_package_unknown_name_no_spec() {
        assert_eq!(is_rust_package("openssl", None), None);
    }

    // ---- is_python_package ----

    #[test]
    fn python_package_by_name() {
        assert_eq!(is_python_package("python-cryptography", None), Some(true));
    }

    #[test]
    fn python_package_by_spec() {
        let spec = "BuildRequires: python3-devel\n%py3_build\n";
        assert_eq!(is_python_package("cryptography", Some(spec)), Some(true));
    }

    #[test]
    fn python_package_by_spec_pyproject() {
        let spec = "BuildRequires: pyproject-rpm-macros\n%pyproject_wheel\n";
        assert_eq!(is_python_package("mypkg", Some(spec)), Some(true));
    }

    #[test]
    fn python_package_rust_name_shortcircuit() {
        assert_eq!(is_python_package("rust-pyo3", None), Some(false));
    }

    // ---- detect_ecosystem ----

    #[test]
    fn detect_ecosystem_js_by_name() {
        assert_eq!(
            detect_ecosystem("nodejs-elliptic", None),
            Some(Ecosystem::JavaScript)
        );
    }

    #[test]
    fn detect_ecosystem_rust_by_name() {
        assert_eq!(
            detect_ecosystem("rust-elliptic-curve", None),
            Some(Ecosystem::Rust)
        );
    }

    #[test]
    fn detect_ecosystem_python_by_name() {
        assert_eq!(
            detect_ecosystem("python-cryptography", None),
            Some(Ecosystem::Python)
        );
    }

    #[test]
    fn detect_ecosystem_unknown_without_spec() {
        assert_eq!(detect_ecosystem("openssl", None), None);
    }

    #[test]
    fn detect_ecosystem_rust_by_spec() {
        let spec = "BuildRequires: cargo-rpm-macros\n%cargo_build\n";
        assert_eq!(
            detect_ecosystem("elliptic-curve", Some(spec)),
            Some(Ecosystem::Rust)
        );
    }

    #[test]
    fn detect_ecosystem_unknown_c_package() {
        let spec = "BuildRequires: gcc\nBuildRequires: cmake\n";
        assert_eq!(detect_ecosystem("openssl", Some(spec)), None);
    }

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
