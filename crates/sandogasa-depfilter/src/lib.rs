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
/// Soname deps like `libbpf.so.1()(64bit)` return `false` here; see
/// [`is_library_dep`] for why they are discounted when the question is
/// whether a package can be *rebuilt* on another branch.
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

/// Return `true` if `dep` is a library a linker produced, soname and
/// all: `libcrypto.so.4()(64bit)`, `libabsl_log.so.2608.0.0()(64bit)`.
///
/// These record what the build they came from linked against, not what
/// the source needs. Rebuilt on another branch, the same source links
/// whatever that branch ships and the dependency is regenerated to
/// match — so measuring a branch by them asks a question nobody will
/// ask. `et` built in Rawhide requires `libprotobuf.so.33.5`, while its
/// spec asks only for `protobuf-lite-devel`, which CentOS Stream 10
/// satisfies at 3.19.6.
///
/// What the source really needs is in its BuildRequires, which is what
/// the closure walk reads.
pub fn is_library_dep(dep: &str) -> bool {
    dep.contains(".so.") || dep.ends_with(".so")
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

/// The capability a Provide string names, without its version:
/// `tor = 0.4.9.5-1.el9` is `tor`, `config(tor) = 1` is `config(tor)`,
/// and `libthing.so.1()(64bit)` is itself.
pub fn provide_name(provide: &str) -> &str {
    provide
        .split_once(" = ")
        .or_else(|| provide.split_once(" < "))
        .or_else(|| provide.split_once(" > "))
        .or_else(|| provide.split_once(" <= "))
        .or_else(|| provide.split_once(" >= "))
        .map(|(name, _)| name)
        .unwrap_or(provide)
}

/// A Provide one side has and the other lacks as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvideChange {
    /// The old Provide string, `tor = 0.4.9.5-1.el9`.
    pub old: String,
    /// The new Provides of the same capability, if there still are
    /// any: a version change. Several versions (a compat package
    /// alongside) are listed in order, joined by `, `. `None` is a
    /// Provide dropped.
    pub new: Option<String>,
}

/// The Provides in `old` that `new` does not have as they were, each
/// with the new Provide of the same capability when there is one.
/// Compare the union of all the packages on each side: a capability
/// that moved to another package, such as a compat package shipping the
/// old version, is then not taken for one dropped.
pub fn compare_provides(
    old: &std::collections::BTreeSet<String>,
    new: &std::collections::BTreeSet<String>,
) -> Vec<ProvideChange> {
    let mut new_by_name: std::collections::BTreeMap<&str, Vec<&str>> =
        std::collections::BTreeMap::new();
    for p in new {
        new_by_name
            .entry(provide_name(p))
            .or_default()
            .push(p.as_str());
    }
    old.difference(new)
        .map(|old_provide| ProvideChange {
            old: old_provide.clone(),
            new: new_by_name
                .get(provide_name(old_provide))
                .map(|all| all.join(", ")),
        })
        .collect()
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

    #[test]
    fn library_deps_are_what_a_linker_wrote() {
        assert!(is_library_dep("libcrypto.so.4()(64bit)"));
        assert!(is_library_dep(
            "libabsl_log_internal_message.so.2608.0.0()(64bit)"
        ));
        assert!(is_library_dep("libc.so.6(GLIBC_2.38)(64bit)"));
        assert!(is_library_dep("libfoo.so"));
        // Capabilities a spec writes by hand are not.
        assert!(!is_library_dep("pkgconfig(openssl)"));
        assert!(!is_library_dep("protobuf-lite-devel"));
        assert!(!is_library_dep("crate(serde) = 1.0.229"));
        assert!(!is_library_dep("/usr/bin/pkg-config"));
    }

    #[test]
    fn a_provide_is_named_without_its_version() {
        assert_eq!(provide_name("tor = 0.4.9.5-1.el9"), "tor");
        assert_eq!(provide_name("config(tor) = 1"), "config(tor)");
        assert_eq!(provide_name("x >= 2"), "x");
        assert_eq!(
            provide_name("libthing.so.1()(64bit)"),
            "libthing.so.1()(64bit)"
        );
    }

    #[test]
    fn provides_compare_as_dropped_or_changed() {
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        let changed = compare_provides(
            &set(&["bash", "libfoo.so.1()(64bit)", "tor = 0.4.9.5-1.el9"]),
            &set(&["bash", "tor = 0.4.9.6-1.el9"]),
        );
        assert_eq!(
            changed,
            [
                ProvideChange {
                    old: "libfoo.so.1()(64bit)".into(),
                    new: None
                },
                ProvideChange {
                    old: "tor = 0.4.9.5-1.el9".into(),
                    new: Some("tor = 0.4.9.6-1.el9".into())
                },
            ]
        );
        // Several new versions are all named, in order.
        assert_eq!(
            compare_provides(&set(&["foo = 1.5"]), &set(&["foo = 1.0", "foo = 2.0"]))[0].new,
            Some("foo = 1.0, foo = 2.0".to_string())
        );
        // A capability another package on the new side still ships as
        // it was is not a change.
        assert!(
            compare_provides(
                &set(&["crate(const-oid/std) = 0.9.6"]),
                &set(&["crate(const-oid) = 0.10.2", "crate(const-oid/std) = 0.9.6"]),
            )
            .is_empty()
        );
    }
}
