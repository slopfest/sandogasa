// SPDX-License-Identifier: MPL-2.0

use version_compare::Version;

/// A parsed Name-Version-Release (NVR) string.
///
/// Example: "freerdp-3.23.0-1.fc42" → name="freerdp", version="3.23.0", release="1.fc42"
#[derive(Debug, PartialEq)]
pub struct Nvr {
    pub name: String,
    pub version: String,
    pub release: String,
}

impl Nvr {
    /// Parse an NVR string like "freerdp-3.23.0-1.fc42" into its components.
    /// Returns None if the string doesn't contain enough dashes.
    pub fn parse(nvr: &str) -> Option<Self> {
        // Split from the right: release is after the last dash, version after the second-to-last
        let release_pos = nvr.rfind('-')?;
        let release = &nvr[release_pos + 1..];
        let name_version = &nvr[..release_pos];

        let version_pos = name_version.rfind('-')?;
        let version = &name_version[version_pos + 1..];
        let name = &name_version[..version_pos];

        if name.is_empty() || version.is_empty() || release.is_empty() {
            return None;
        }

        Some(Nvr {
            name: name.to_string(),
            version: version.to_string(),
            release: release.to_string(),
        })
    }
}

/// Compare two upstream version strings. Returns true if `build_version` >= `fixed_version`.
pub fn version_gte(build_version: &str, fixed_version: &str) -> bool {
    match (Version::from(build_version), Version::from(fixed_version)) {
        (Some(bv), Some(fv)) => bv >= fv,
        _ => false,
    }
}

/// Map a Bugzilla bug's Version field to a Bodhi release name.
///
/// Examples:
/// - "42" → "F42"
/// - "41" → "F41"
/// - "rawhide" → "F43" (current rawhide, but we skip rawhide for now)
/// - "epel9" → "EPEL-9"
/// - "epel10" → "EPEL-10"
pub fn fedora_release_from_version(version: &str) -> Option<String> {
    let lower = version.to_lowercase();

    if let Some(rest) = lower.strip_prefix("epel") {
        let num = rest.trim_start_matches('-');
        if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
            return Some(format!("EPEL-{num}"));
        }
        return None;
    }

    if lower == "rawhide" {
        return None;
    }

    // Plain numeric version → Fedora release
    if version.chars().all(|c| c.is_ascii_digit()) && !version.is_empty() {
        return Some(format!("F{version}"));
    }

    None
}

/// Extract a Fedora release tag from a bug summary like "[fedora-42]" or "[epel-9]".
pub fn release_from_summary(summary: &str) -> Option<String> {
    // Look for [fedora-NN] pattern
    if let Some(start) = summary.find("[fedora-") {
        let after = &summary[start + 8..];
        if let Some(end) = after.find(']') {
            let num = &after[..end];
            if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
                return Some(format!("F{num}"));
            }
        }
    }

    // Look for [epel-NN] pattern
    if let Some(start) = summary.find("[epel-") {
        let after = &summary[start + 6..];
        if let Some(end) = after.find(']') {
            let num = &after[..end];
            if num.chars().all(|c| c.is_ascii_digit()) && !num.is_empty() {
                return Some(format!("EPEL-{num}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Nvr::parse ----

    #[test]
    fn nvr_parse_standard() {
        let nvr = Nvr::parse("freerdp-3.23.0-1.fc42").unwrap();
        assert_eq!(nvr.name, "freerdp");
        assert_eq!(nvr.version, "3.23.0");
        assert_eq!(nvr.release, "1.fc42");
    }

    #[test]
    fn nvr_parse_multi_dash_name() {
        let nvr = Nvr::parse("python-azure-sdk-5.0.0-2.fc42").unwrap();
        assert_eq!(nvr.name, "python-azure-sdk");
        assert_eq!(nvr.version, "5.0.0");
        assert_eq!(nvr.release, "2.fc42");
    }

    #[test]
    fn nvr_parse_epoch_in_version() {
        // Some NVRs might have complex versions
        let nvr = Nvr::parse("kernel-6.12.8-200.fc41").unwrap();
        assert_eq!(nvr.name, "kernel");
        assert_eq!(nvr.version, "6.12.8");
        assert_eq!(nvr.release, "200.fc41");
    }

    #[test]
    fn nvr_parse_single_dash_fails() {
        assert!(Nvr::parse("freerdp-3.23.0").is_none());
    }

    #[test]
    fn nvr_parse_no_dash_fails() {
        assert!(Nvr::parse("freerdp").is_none());
    }

    #[test]
    fn nvr_parse_empty_fails() {
        assert!(Nvr::parse("").is_none());
    }

    #[test]
    fn nvr_parse_trailing_dashes_empty_parts() {
        // "-1.0-1.fc42" would give empty name
        assert!(Nvr::parse("-1.0-1.fc42").is_none());
    }

    #[test]
    fn nvr_parse_empty_version() {
        // "foo--1.fc42" would give empty version
        assert!(Nvr::parse("foo--1.fc42").is_none());
    }

    #[test]
    fn nvr_parse_empty_release() {
        // "foo-1.0-" would give empty release
        assert!(Nvr::parse("foo-1.0-").is_none());
    }

    // ---- version_gte ----

    #[test]
    fn version_gte_equal() {
        assert!(version_gte("3.23.0", "3.23.0"));
    }

    #[test]
    fn version_gte_greater() {
        assert!(version_gte("3.24.0", "3.23.0"));
    }

    #[test]
    fn version_gte_less() {
        assert!(!version_gte("3.22.0", "3.23.0"));
    }

    #[test]
    fn version_gte_major_diff() {
        assert!(version_gte("4.0.0", "3.99.99"));
    }

    #[test]
    fn version_gte_single_component() {
        assert!(version_gte("10", "9"));
    }

    #[test]
    fn version_gte_different_depth() {
        assert!(version_gte("3.23.1", "3.23"));
    }

    // ---- fedora_release_from_version ----

    #[test]
    fn release_from_version_numeric() {
        assert_eq!(fedora_release_from_version("42"), Some("F42".to_string()));
    }

    #[test]
    fn release_from_version_numeric_41() {
        assert_eq!(fedora_release_from_version("41"), Some("F41".to_string()));
    }

    #[test]
    fn release_from_version_epel9() {
        assert_eq!(
            fedora_release_from_version("epel9"),
            Some("EPEL-9".to_string())
        );
    }

    #[test]
    fn release_from_version_epel_dash_10() {
        assert_eq!(
            fedora_release_from_version("epel-10"),
            Some("EPEL-10".to_string())
        );
    }

    #[test]
    fn release_from_version_rawhide_skipped() {
        assert_eq!(fedora_release_from_version("rawhide"), None);
    }

    #[test]
    fn release_from_version_empty() {
        assert_eq!(fedora_release_from_version(""), None);
    }

    #[test]
    fn release_from_version_garbage() {
        assert_eq!(fedora_release_from_version("foobar"), None);
    }

    #[test]
    fn release_from_version_epel_no_number() {
        assert_eq!(fedora_release_from_version("epel"), None);
    }

    // ---- release_from_summary ----

    #[test]
    fn summary_fedora_42() {
        assert_eq!(
            release_from_summary("CVE-2026-12345 freerdp: buffer overflow [fedora-42]"),
            Some("F42".to_string())
        );
    }

    #[test]
    fn summary_epel_9() {
        assert_eq!(
            release_from_summary("CVE-2026-12345 freerdp: overflow [epel-9]"),
            Some("EPEL-9".to_string())
        );
    }

    #[test]
    fn summary_no_tag() {
        assert_eq!(
            release_from_summary("CVE-2026-12345 freerdp: buffer overflow"),
            None
        );
    }

    #[test]
    fn summary_fedora_tag_no_number() {
        assert_eq!(release_from_summary("CVE-2026-12345 [fedora-] foo"), None);
    }
}
