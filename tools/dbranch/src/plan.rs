// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Pure helpers that turn branch names and versions into the exact
//! commands dbranch runs (and shows under `--explain`). Kept separate
//! from execution so every command can be asserted in tests.

/// The Ubuntu codename for a branch: the segment after a `namespace/`
/// prefix, or the whole name when unprefixed. `ubuntu/questing` →
/// `questing`, `noble` → `noble`.
pub fn codename_from_branch(branch: &str) -> &str {
    branch.rsplit('/').next().unwrap_or(branch)
}

/// The PPA target branches for a no-argument bulk run: every local
/// branch except those in `exclude` (the current Debian branch and
/// gbp's plumbing branches — `upstream-branch` and the pristine-tar
/// branch).
pub fn ppa_branches(all: &[String], exclude: &[String]) -> Vec<String> {
    all.iter()
        .filter(|b| !exclude.iter().any(|e| e == *b))
        .cloned()
        .collect()
}

/// Strip a Debian epoch (`N:`) for filename use — `.dsc`/`.changes`
/// names never carry the epoch.
pub fn version_no_epoch(version: &str) -> &str {
    match version.split_once(':') {
        Some((_, rest)) => rest,
        None => version,
    }
}

/// The source `.dsc` filename for a package at a version.
pub fn dsc_filename(package: &str, version: &str) -> String {
    format!("{package}_{}.dsc", version_no_epoch(version))
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// `git checkout <branch>`.
pub fn checkout_argv(branch: &str) -> Vec<String> {
    argv(&["git", "checkout", branch])
}

/// `git checkout -b <branch> <start_point>` — create a new PPA branch
/// off the current Debian branch.
pub fn checkout_new_argv(branch: &str, start_point: &str) -> Vec<String> {
    argv(&["git", "checkout", "-b", branch, start_point])
}

/// `git merge --signoff --no-edit <source>` — merge the Debian branch
/// in; `--signoff` matches the merge commits in the damo history.
pub fn merge_argv(source: &str) -> Vec<String> {
    argv(&["git", "merge", "--signoff", "--no-edit", source])
}

/// `git add debian/changelog`.
pub fn add_changelog_argv() -> Vec<String> {
    argv(&["git", "add", "debian/changelog"])
}

/// `git commit -s --no-edit --cleanup=strip` — finalize a
/// conflict-resolved merge. `--cleanup=strip` drops the `# Conflicts:`
/// comment block git leaves in `MERGE_MSG` (with `--no-edit` the
/// default cleanup is `whitespace`, which would keep those `#` lines).
pub fn commit_merge_argv() -> Vec<String> {
    argv(&["git", "commit", "-s", "--no-edit", "--cleanup=strip"])
}

/// `git commit -s -m <message> debian/changelog`.
pub fn commit_changelog_argv(message: &str) -> Vec<String> {
    argv(&["git", "commit", "-s", "-m", message, "debian/changelog"])
}

/// `gbp dch --bpo -R -D <codename> --spawn-editor=never` — create the
/// finalized rebuild stanza (with the correct date/maintainer
/// footer). `-R`/`--release` would otherwise spawn an editor by
/// default; dbranch normalizes the entry afterward, so suppress it.
pub fn gbp_dch_argv(codename: &str) -> Vec<String> {
    argv(&[
        "gbp",
        "dch",
        "--bpo",
        "-R",
        "-D",
        codename,
        "--spawn-editor=never",
    ])
}

/// `debuild -S -sa -d` — build the source package.
pub fn debuild_argv() -> Vec<String> {
    argv(&["debuild", "-S", "-sa", "-d"])
}

/// `pbuilder-dist <codename> ../<pkg>_<version>.dsc` — scratch-build
/// the source package in the codename's chroot.
pub fn pbuilder_argv(codename: &str, dsc_relpath: &str) -> Vec<String> {
    argv(&["pbuilder-dist", codename, dsc_relpath])
}

/// `pbuilder-dist <codename> create` — build the codename's base
/// chroot the first time (no `~/pbuilder/<codename>-base.tgz` yet).
pub fn pbuilder_create_argv(codename: &str) -> Vec<String> {
    argv(&["pbuilder-dist", codename, "create"])
}

/// Path to a codename's pbuilder base tarball
/// (`~/pbuilder/<codename>-base.tgz`); `None` if `$HOME` is unset.
pub fn pbuilder_base_tgz(codename: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::Path::new(&home)
            .join("pbuilder")
            .join(format!("{codename}-base.tgz"))
    })
}

/// The gbp-style commit subject for a changelog release commit.
pub fn changelog_commit_message(version: &str) -> String {
    format!("Update changelog for {version} release")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codename_strips_namespace() {
        assert_eq!(codename_from_branch("ubuntu/questing"), "questing");
        assert_eq!(codename_from_branch("noble"), "noble");
        assert_eq!(codename_from_branch("ubuntu/resolute"), "resolute");
    }

    #[test]
    fn ppa_branches_excludes_listed() {
        let all: Vec<String> = [
            "master",
            "upstream",
            "pristine-tar",
            "noble",
            "ubuntu/questing",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let exclude: Vec<String> = ["master", "upstream", "pristine-tar"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            ppa_branches(&all, &exclude),
            vec!["noble", "ubuntu/questing"]
        );
    }

    #[test]
    fn dsc_filename_drops_epoch() {
        assert_eq!(
            dsc_filename("damo", "3.2.8-1~questing+1"),
            "damo_3.2.8-1~questing+1.dsc"
        );
        assert_eq!(
            dsc_filename("damo", "1:3.2.8-1~questing+1"),
            "damo_3.2.8-1~questing+1.dsc"
        );
    }

    #[test]
    fn command_builders_match_the_real_commands() {
        assert_eq!(checkout_argv("noble"), ["git", "checkout", "noble"]);
        assert_eq!(
            checkout_new_argv("ubuntu/plucky", "debian/unstable"),
            ["git", "checkout", "-b", "ubuntu/plucky", "debian/unstable"]
        );
        assert_eq!(
            merge_argv("master"),
            ["git", "merge", "--signoff", "--no-edit", "master"]
        );
        // The merge commit strips git's `# Conflicts:` comment block.
        assert_eq!(
            commit_merge_argv(),
            ["git", "commit", "-s", "--no-edit", "--cleanup=strip"]
        );
        assert_eq!(
            gbp_dch_argv("questing"),
            [
                "gbp",
                "dch",
                "--bpo",
                "-R",
                "-D",
                "questing",
                "--spawn-editor=never"
            ]
        );
        assert_eq!(debuild_argv(), ["debuild", "-S", "-sa", "-d"]);
        assert_eq!(
            pbuilder_argv("questing", "../damo_3.2.8-1~questing+1.dsc"),
            [
                "pbuilder-dist",
                "questing",
                "../damo_3.2.8-1~questing+1.dsc"
            ]
        );
        assert_eq!(
            pbuilder_create_argv("questing"),
            ["pbuilder-dist", "questing", "create"]
        );
        assert_eq!(
            changelog_commit_message("3.2.8-1~questing+1"),
            "Update changelog for 3.2.8-1~questing+1 release"
        );
    }
}
