// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The DEP-3 commit template, and the route a patch takes.
//!
//! On a `3.0 (quilt)` package a fix to a file outside `debian/`
//! cannot be a commit on the packaging branch: the orig tarball
//! comes from upstream, so `dpkg-source -b` stops with "aborting due
//! to unexpected upstream changes" and the packager finds out at
//! build time, having already made the commit. It has to become a
//! patch, written on the `gbp pq` branch.
//!
//! What goes wrong next is the header. `gbp pq export` turns the
//! commit message into the patch header, so a message with no
//! `Forwarded:` produces a patch that does not say whether upstream
//! has seen the fix -- which is what a sponsor asks first. The
//! template exists to put those fields in front of whoever is
//! writing the commit, at the moment they write it.

use std::path::{Path, PathBuf};

/// Where the template is written, relative to the repository.
///
/// Under `.git`, which is neither committed nor shipped, so offering
/// it costs the package nothing and needs no cleaning up.
pub const TEMPLATE_PATH: &str = ".git/dbranch-dep3-template";

/// Whether this package has a patch system at all.
///
/// A native package has none, and the advice would be wrong there:
/// its changes *are* commits on the packaging branch.
pub fn is_quilt(repo: &Path) -> bool {
    std::fs::read_to_string(repo.join("debian/source/format"))
        .is_ok_and(|f| f.trim_start().starts_with("3.0 (quilt)"))
}

/// The sequence that writes a patch, with `template` naming the
/// commit template when there is one to offer.
///
/// Kept here rather than spelled out at each call site: the route is
/// printed from more than one place, and two copies would drift.
///
/// The sign-off is deferred to an amend rather than passed to the
/// commit. `-s` appends its trailer after the last *non-comment*
/// line, and this template has none but blanks, so at commit time
/// the sign-off lands above the commented fields and every field
/// uncommented below it ends up after it. By the amend those fields
/// are real text, so the trailer follows them, which is both the
/// readable order and git's own convention of trailers last.
/// Cosmetic rather than load-bearing -- lintian reads DEP-3 fields
/// wherever they sit -- but a sponsor skims these.
pub fn workflow(template: Option<&str>) -> Vec<String> {
    let commit = match template {
        Some(path) => format!("    git commit -t {path}"),
        None => "    git commit".to_string(),
    };
    vec![
        "    gbp pq import".to_string(),
        "    ...edit the file...".to_string(),
        commit,
        "    git commit --amend --no-edit -s".to_string(),
        "    gbp pq export".to_string(),
    ]
}

/// Write the template, returning the path to name in the advice.
///
/// Overwritten freely: the content is static, so there is no edit of
/// anyone's to preserve, and rewriting it is how it stays current.
pub fn write_template(repo: &Path) -> Result<PathBuf, String> {
    let path = repo.join(TEMPLATE_PATH);
    let parent = path
        .parent()
        .ok_or_else(|| format!("{TEMPLATE_PATH}: no parent directory"))?;
    if !parent.is_dir() {
        return Err(format!(
            "{} is not a git repository, so there is nowhere to put the template",
            repo.display()
        ));
    }
    std::fs::write(&path, crate::plan::dep3_commit_template())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// Say where a fix to a file outside `debian/` has to go, offering
/// the template to write it with.
///
/// Best effort, and silent on a native package: this is advice
/// alongside some other finding, not a check of its own, so a
/// repository that cannot take the template simply gets the route
/// without it.
pub fn note_route(repo: &Path) {
    if !is_quilt(repo) {
        return;
    }
    let written = write_template(repo).ok();
    eprintln!();
    eprintln!("note: a fix outside debian/ is a patch, not a commit on this branch.");
    eprintln!("  The commit message becomes the patch header, so its DEP-3 fields");
    eprintln!("  -- Forwarded: above all -- are what say the fix went upstream:");
    eprintln!();
    for line in workflow(written.as_ref().map(|_| TEMPLATE_PATH)) {
        eprintln!("{line}");
    }
    eprintln!();
}

/// `dep3-template` — write the template and show what it is for.
pub fn run(repo: &Path, print: bool, ui: &crate::ui::Ui) -> Result<(), String> {
    if print {
        print!("{}", crate::plan::dep3_commit_template());
        return Ok(());
    }
    if !is_quilt(repo) {
        return Err(format!(
            "{} is not a 3.0 (quilt) package, so it has no patches to write",
            repo.display()
        ));
    }
    ui.step("Write the DEP-3 commit template");
    if ui.dry_run {
        println!("would write {TEMPLATE_PATH}");
        return Ok(());
    }
    let path = write_template(repo)?;
    println!("wrote {}", path.display());
    println!();
    for line in workflow(Some(TEMPLATE_PATH)) {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quilt_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("a temp dir");
        std::fs::create_dir_all(dir.path().join("debian/source")).unwrap();
        std::fs::write(dir.path().join("debian/source/format"), "3.0 (quilt)\n").unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        dir
    }

    #[test]
    fn a_native_package_has_no_patches_to_write() {
        let dir = quilt_tree();
        std::fs::write(dir.path().join("debian/source/format"), "3.0 (native)\n").unwrap();
        assert!(!is_quilt(dir.path()));
        // And a tree with no format file at all says nothing either.
        let bare = tempfile::tempdir().unwrap();
        assert!(!is_quilt(bare.path()));
    }

    #[test]
    fn the_template_lands_under_git_where_nothing_ships_it() {
        let dir = quilt_tree();
        let path = write_template(dir.path()).expect("a git repository takes it");
        assert!(path.ends_with("dbranch-dep3-template"));
        let text = std::fs::read_to_string(&path).unwrap();
        // The fields a person forgets, and not the two gbp writes
        // from the commit itself.
        assert!(text.contains("Forwarded:"), "{text}");
        assert!(!text.contains("\nDescription:"), "{text}");
    }

    #[test]
    fn without_a_git_directory_there_is_nowhere_to_put_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("debian/source")).unwrap();
        std::fs::write(dir.path().join("debian/source/format"), "3.0 (quilt)\n").unwrap();
        let err = write_template(dir.path()).expect_err("no .git, no template");
        assert!(err.contains("not a git repository"), "{err}");
    }

    #[test]
    fn the_route_names_the_template_only_when_there_is_one() {
        let with = workflow(Some(TEMPLATE_PATH));
        assert!(with[2].contains("git commit -t .git/dbranch-dep3-template"));
        // Without one the sequence still stands; only the -t goes.
        let without = workflow(None);
        assert_eq!(without[2].trim(), "git commit");
        assert_eq!(with.len(), without.len());
        assert_eq!(with[0].trim(), "gbp pq import");
        assert_eq!(with[4].trim(), "gbp pq export");
        // The sign-off comes after the message is written, so it
        // lands below the DEP-3 fields rather than above them.
        assert_eq!(with[3].trim(), "git commit --amend --no-edit -s");
        assert!(!with[2].contains(" -s"));
    }
}
