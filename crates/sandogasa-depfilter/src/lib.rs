// SPDX-License-Identifier: Apache-2.0 OR MIT

//! RPM dependency filtering for cross-branch analysis.
//!
//! Provides functions to classify RPM dependency strings as
//! auto-generated or otherwise ignorable when comparing packages
//! across Fedora/EPEL branches.

/// Return `true` if `dep` is a solib-style dependency (contains `.so.`).
///
/// This is a broad check that matches both soname deps like
/// `libbpf.so.1()(64bit)` and symbol version deps like
/// `libc.so.6(GLIBC_2.38)(64bit)`.
pub fn is_solib_dep(dep: &str) -> bool {
    dep.contains(".so.")
}

/// Return `true` if `dep` is a solib symbol version dependency.
///
/// These have non-empty content in the first parentheses, e.g.
/// `libc.so.6(GLIBC_2.38)(64bit)`.  They are auto-generated at
/// RPM build time and regenerated when the package is rebuilt on
/// the target branch, so they are not meaningful for cross-branch
/// installability checks.
///
/// Soname deps like `libbpf.so.1()(64bit)` return `false` — they
/// are also auto-generated but match the soversion, so changes
/// reflect real ABI bumps that matter for installability.
pub fn is_solib_symbol_dep(dep: &str) -> bool {
    let Some(so_pos) = dep.find(".so.") else {
        return false;
    };
    let after_so = &dep[so_pos..];
    let Some(open) = after_so.find('(') else {
        return false;
    };
    let inside = &after_so[open + 1..];
    let Some(close) = inside.find(')') else {
        return false;
    };
    close > 0
}

/// Return `true` if `dep` is an RPM-internal or auto-generated
/// dependency that is not meaningful for installability checks.
///
/// This covers:
/// - `rpmlib(...)` — RPM feature gates
/// - `auto(...)` — auto-generated deps
/// - `config(...)` — config file deps
pub fn is_rpm_internal_dep(dep: &str) -> bool {
    dep.starts_with("rpmlib(") || dep.starts_with("auto(") || dep.starts_with("config(")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solib_positive() {
        assert!(is_solib_dep("libc.so.6()(64bit)"));
        assert!(is_solib_dep("libc.so.6(GLIBC_2.38)(64bit)"));
        assert!(is_solib_dep("libm.so.6(GLIBC_2.29)(64bit)"));
        assert!(is_solib_dep("libbpf.so.1()(64bit)"));
    }

    #[test]
    fn solib_negative() {
        assert!(!is_solib_dep("glibc"));
        assert!(!is_solib_dep("glibc-devel"));
        assert!(!is_solib_dep("libfoo"));
        assert!(!is_solib_dep("pkgconfig(dracut)"));
        assert!(!is_solib_dep("rpmlib(CompressedFileNames)"));
    }

    #[test]
    fn solib_symbol_positive() {
        assert!(is_solib_symbol_dep("libc.so.6(GLIBC_2.38)(64bit)"));
        assert!(is_solib_symbol_dep("libm.so.6(GLIBC_2.29)(64bit)"));
        assert!(is_solib_symbol_dep("libpthread.so.0(GLIBC_2.12)(64bit)"));
    }

    #[test]
    fn solib_symbol_negative_soname() {
        // Soname deps have empty first parens — also auto-generated
        // but match the soversion, so changes are meaningful.
        assert!(!is_solib_symbol_dep("libbpf.so.1()(64bit)"));
        assert!(!is_solib_symbol_dep("libc.so.6()(64bit)"));
    }

    #[test]
    fn solib_symbol_negative_non_solib() {
        assert!(!is_solib_symbol_dep("glibc"));
        assert!(!is_solib_symbol_dep("pkgconfig(dracut)"));
        assert!(!is_solib_symbol_dep("rpmlib(CompressedFileNames)"));
    }

    #[test]
    fn rpm_internal_positive() {
        assert!(is_rpm_internal_dep("rpmlib(CompressedFileNames)"));
        assert!(is_rpm_internal_dep("rpmlib(PayloadIsZstd)"));
        assert!(is_rpm_internal_dep("auto(gcc)"));
        assert!(is_rpm_internal_dep("config(glibc)"));
    }

    #[test]
    fn rpm_internal_negative() {
        assert!(!is_rpm_internal_dep("glibc"));
        assert!(!is_rpm_internal_dep("libc.so.6()(64bit)"));
        assert!(!is_rpm_internal_dep("pkgconfig(dracut)"));
    }
}
