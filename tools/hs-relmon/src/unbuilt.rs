// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `check-unbuilt`: Hyperscale branches with merged commits their
//! newest build does not have ([`sandogasa_cbs::unbuilt`]), each
//! build's branch read from its dist tag.

use std::collections::BTreeMap;

use sandogasa_cbs::unbuilt::Target;

use crate::cbs::Build;
use crate::repos::build_branch_tag;

pub use sandogasa_cbs::unbuilt::DEFAULT_GRACE_DAYS;

/// The branches builds carrying the dist tag `(release, suffix)` come
/// from, the likeliest first. Hyperscale's convention: a package built
/// for both the main repository and a flavor builds both from the main
/// branch (`c9s-hs`), only the target's dist tag differing, and a
/// package mostly for one flavor's use has that flavor's branch
/// (`c9s-hs+fb`). So the flavor's or variant's own branch is tried
/// first, then `c9s-hs`, then its old spelling `c9s-sig-hyperscale`.
pub fn branches_for(release: u32, suffix: &str) -> Vec<String> {
    let mut branches = Vec::new();
    if suffix != "hs" {
        branches.push(format!("c{release}s-{suffix}"));
    }
    branches.push(format!("c{release}s-hs"));
    branches.push(format!("c{release}s-sig-hyperscale"));
    branches
}

/// `package`'s newest build per Hyperscale branch among `builds`
/// (the highest build id, wherever it is tagged), as targets to check.
pub fn targets(package: &str, builds: impl IntoIterator<Item = Build>) -> Vec<Target> {
    let mut newest: BTreeMap<(u32, String), Build> = BTreeMap::new();
    for b in builds.into_iter().filter(|b| b.name == package) {
        let Some(key) = build_branch_tag(&b.release) else {
            continue;
        };
        match newest.get(&key) {
            Some(kept) if kept.build_id >= b.build_id => {}
            _ => {
                newest.insert(key, b);
            }
        }
    }
    newest
        .into_iter()
        .map(|((release, suffix), b)| Target {
            package: package.to_string(),
            branches: branches_for(release, &suffix),
            nvr: b.nvr,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(id: i64, name: &str, release: &str) -> Build {
        Build {
            build_id: id,
            name: name.into(),
            version: "9.9p1".into(),
            release: release.into(),
            nvr: format!("{name}-9.9p1-{release}"),
        }
    }

    #[test]
    fn a_dist_tag_names_its_branch() {
        assert_eq!(
            branches_for(9, "hs+fb"),
            ["c9s-hs+fb", "c9s-hs", "c9s-sig-hyperscale"]
        );
        assert_eq!(
            branches_for(10, "hsx"),
            ["c10s-hsx", "c10s-hs", "c10s-sig-hyperscale"]
        );
        assert_eq!(branches_for(10, "hs"), ["c10s-hs", "c10s-sig-hyperscale"]);
    }

    #[test]
    fn the_newest_build_per_branch_is_checked() {
        let builds = [
            build(10, "openssh", "16.1.hs+fb.el9"),
            // In testing, newer: counts as built.
            build(12, "openssh", "17.1.hs+fb.el9"),
            build(11, "openssh", "30.1.hs+fb.el10"),
            build(5, "openssh", "16.1.hs.el9"),
            build(13, "other", "1.hs.el9"),
            // Not a Hyperscale dist tag.
            build(14, "openssh", "1.el9"),
        ];
        let got: Vec<(String, Vec<String>)> = targets("openssh", builds)
            .into_iter()
            .map(|t| (t.nvr, t.branches))
            .collect();
        assert_eq!(
            got,
            [
                (
                    "openssh-9.9p1-16.1.hs.el9".to_string(),
                    vec!["c9s-hs".to_string(), "c9s-sig-hyperscale".to_string()]
                ),
                (
                    "openssh-9.9p1-17.1.hs+fb.el9".to_string(),
                    branches_for(9, "hs+fb")
                ),
                (
                    "openssh-9.9p1-30.1.hs+fb.el10".to_string(),
                    branches_for(10, "hs+fb")
                ),
            ]
        );
    }
}
