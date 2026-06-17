// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The `rebuild` workflow, run in selectable stages (like
//! `rpmbuild`'s `-bp`/`-bc`/…):
//!
//! - **merge** — switch to the target PPA branch (creating it from
//!   the Debian branch if absent), merge the Debian branch in,
//!   resolve the `debian/changelog` conflict, and write the
//!   `~<codename>+<N>` "Rebuild for <codename>" entry.
//! - **build** — `debuild -S` then `pbuilder-dist` for the codename.
//!
//! dbranch is run from the Debian branch (whatever is checked out —
//! `master`, `debian/unstable`, …); that branch is the merge source.

use std::path::Path;

use crate::git;
use crate::plan::{self, changelog_commit_message};
use crate::ui::Ui;
use crate::{changelog, gbpconf};

/// Which workflow stages to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stages {
    pub merge: bool,
    pub build: bool,
}

impl Stages {
    fn any(&self) -> bool {
        self.merge || self.build
    }
}

/// Parse the `--stage` selector. Empty defaults to **merge** only
/// (the build stage is opt-in for now). `all` enables every stage.
pub fn parse_stages(tokens: &[String]) -> Result<Stages, String> {
    if tokens.is_empty() {
        return Ok(Stages {
            merge: true,
            build: false,
        });
    }
    let mut s = Stages {
        merge: false,
        build: false,
    };
    for token in tokens {
        match token.trim() {
            "merge" => s.merge = true,
            "build" => s.build = true,
            "all" => {
                s.merge = true;
                s.build = true;
            }
            other => {
                return Err(format!(
                    "unknown stage '{other}' (valid: merge, build, all)"
                ));
            }
        }
    }
    if !s.any() {
        return Err("no stages selected".to_string());
    }
    Ok(s)
}

/// Inputs for a `rebuild` run.
pub struct Options {
    /// Explicit target branches; empty means all existing PPA
    /// branches (every local branch except the current Debian branch
    /// and gbp's plumbing branches).
    pub branches: Vec<String>,
    /// Stages to run.
    pub stages: Stages,
}

/// Run the rebuild workflow over the selected branches.
pub fn run(ui: &Ui, repo: &Path, opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    if !ui.dry_run {
        git::ensure_tools(opts.stages.build)?;
    }

    // The Debian branch is wherever we start.
    let source = git::current_branch(repo)?;
    let all = git::local_branches(repo)?;

    let targets = if opts.branches.is_empty() {
        // Repo config overrides home config.
        let cfg = gbpconf::read_repo(repo).or(gbpconf::read_home());
        bulk_targets(&source, &all, &cfg)
    } else {
        opts.branches.clone()
    };
    if targets.is_empty() {
        return Err("no target branches to rebuild".into());
    }

    for target in &targets {
        // Only the merge stage can't target the current branch (a
        // branch can't be merged into itself); building the branch
        // you're already on is fine.
        if opts.stages.merge && target == &source {
            return Err(format!(
                "target branch {target} is the current Debian branch; \
                 name a different branch (or use --stage build)"
            )
            .into());
        }
        let exists = all.iter().any(|b| b == target);
        rebuild_one(ui, repo, &source, target, exists, opts.stages)?;
    }
    Ok(())
}

/// All existing PPA branches: everything except the Debian branch and
/// gbp's plumbing branches (`upstream-branch` and the pristine-tar
/// branch). Pure — `cfg` is the effective gbp config.
fn bulk_targets(source: &str, all: &[String], cfg: &gbpconf::GbpConfig) -> Vec<String> {
    let upstream = cfg
        .upstream_branch
        .clone()
        .unwrap_or_else(|| "upstream".to_string());
    let mut exclude = vec![source.to_string(), upstream];
    if cfg.pristine_tar.unwrap_or(false) {
        exclude.push("pristine-tar".to_string());
    }
    plan::ppa_branches(all, &exclude)
}

fn rebuild_one(
    ui: &Ui,
    repo: &Path,
    source: &str,
    target: &str,
    exists: bool,
    stages: Stages,
) -> Result<(), Box<dyn std::error::Error>> {
    let codename = target_codename(repo, target, exists);
    ui.step(&format!("{target} (codename: {codename})"));

    // The version produced by the merge stage, reused by the build
    // stage's `.dsc` path when both run.
    let mut rebuilt_version: Option<String> = None;

    if stages.merge {
        merge_stage(
            ui,
            repo,
            source,
            target,
            exists,
            &codename,
            &mut rebuilt_version,
        )?;
    } else {
        // Not merging: we still need to be on the target to build it.
        if !exists {
            return Err(format!(
                "branch {target} does not exist; run the merge stage to create it"
            )
            .into());
        }
        ui.run_required(&plan::checkout_argv(target), repo)?;
    }

    if stages.build {
        build_stage(ui, repo, &codename, rebuilt_version.as_deref())?;
    }

    Ok(())
}

/// The merge stage: get onto the target branch (create if needed),
/// merge the Debian branch, resolve the changelog conflict, and write
/// the normalized rebuild entry.
fn merge_stage(
    ui: &Ui,
    repo: &Path,
    source: &str,
    target: &str,
    exists: bool,
    codename: &str,
    out_version: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if exists {
        ui.run_required(&plan::checkout_argv(target), repo)?;
        // The changelog conflict is expected and resolved
        // deterministically; in dry-run we always narrate it.
        let merged_ok = ui.run(&plan::merge_argv(source), repo)?;
        if ui.dry_run || !merged_ok {
            ui.step("Resolve the debian/changelog conflict");
            if !ui.dry_run {
                resolve_changelog(repo)?;
            }
            ui.run_required(&plan::add_changelog_argv(), repo)?;
            ui.run_required(&plan::commit_merge_argv(), repo)?;
        }
    } else {
        // A new PPA branch starts from the Debian branch (already has
        // the new packaging) — no merge needed.
        ui.step(&format!("Create {target} from {source}"));
        ui.run_required(&plan::checkout_new_argv(target, source), repo)?;
    }

    let version = {
        let text = std::fs::read_to_string(repo.join("debian/changelog"))?;
        changelog::rebuild_version(&text, codename)
            .ok_or("could not determine the Debian version from debian/changelog")?
    };

    ui.step("Generate the rebuild changelog entry");
    ui.run_required(&plan::gbp_dch_argv(codename), repo)?;

    ui.step(&format!(
        "Normalize the entry to {version} / \"Rebuild for {codename}\""
    ));
    if !ui.dry_run {
        let text = std::fs::read_to_string(repo.join("debian/changelog"))?;
        let normalized = changelog::normalize_top_stanza(&text, &version, codename)?;
        std::fs::write(repo.join("debian/changelog"), normalized)?;
    }
    ui.run_required(
        &plan::commit_changelog_argv(&changelog_commit_message(&version)),
        repo,
    )?;

    *out_version = Some(version);
    Ok(())
}

/// The build stage: build the source package and scratch-build it in
/// the codename's pbuilder chroot.
fn build_stage(
    ui: &Ui,
    repo: &Path,
    codename: &str,
    merged_version: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prefer the version the merge stage just produced; otherwise read
    // the current top changelog entry (build-only run).
    let (package, version) = top_package_version(repo)?;
    let version = merged_version.map(str::to_string).unwrap_or(version);

    ui.step("Build the source package");
    ui.run_required(&plan::debuild_argv(), repo)?;

    // First-time chroot setup: pbuilder-dist needs the base tarball.
    let needs_create = plan::pbuilder_base_tgz(codename).is_none_or(|p| !p.exists());
    if needs_create {
        ui.step(&format!(
            "Create the {codename} pbuilder chroot (no base tarball yet)"
        ));
        ui.run_required(&plan::pbuilder_create_argv(codename), repo)?;
    }

    let dsc = format!("../{}", plan::dsc_filename(&package, &version));
    ui.step(&format!("Scratch-build in the {codename} chroot"));
    ui.run_required(&plan::pbuilder_argv(codename, &dsc), repo)?;
    Ok(())
}

/// The codename for a target: for an existing branch, the basename of
/// its `debian/gbp.conf` `debian-branch` (read without checking it
/// out); otherwise the branch name's basename (`ubuntu/<rel>` →
/// `<rel>`).
fn target_codename(repo: &Path, target: &str, exists: bool) -> String {
    if exists
        && let Some(text) = git::show_file(repo, target, "debian/gbp.conf")
        && let Some(db) = gbpconf::parse(&text).debian_branch
    {
        return plan::codename_from_branch(&db).to_string();
    }
    plan::codename_from_branch(target).to_string()
}

/// Resolve a conflicted `debian/changelog` in place, refusing to
/// proceed if anything other than the changelog is left unmerged.
fn resolve_changelog(repo: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let unmerged = git::unmerged_paths(repo)?;
    let others: Vec<&String> = unmerged
        .iter()
        .filter(|p| p.as_str() != "debian/changelog")
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "merge has non-changelog conflicts ({}); resolve them manually, \
             commit, then re-run",
            others
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }
    if unmerged.is_empty() {
        return Err("git merge failed but no paths are unmerged; aborting".into());
    }
    let path = repo.join("debian/changelog");
    let text = std::fs::read_to_string(&path)?;
    let resolved = changelog::resolve_conflict(&text)
        .ok_or("debian/changelog is unmerged but has no conflict markers")?;
    std::fs::write(&path, resolved)?;
    Ok(())
}

/// The source package name and version from the top changelog stanza.
fn top_package_version(repo: &Path) -> Result<(String, String), Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(repo.join("debian/changelog"))?;
    let header = changelog::stanza_headers(&text)
        .into_iter()
        .next()
        .ok_or("empty debian/changelog")?;
    Ok((header.package, header.version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Ui;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        // `-c commit.gpgsign=false` so a developer's global signing
        // config doesn't break the throwaway test commits.
        let ok = Command::new("git")
            .arg("-c")
            .arg("commit.gpgsign=false")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@x")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@x")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn setup() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        git(p, &["init", "-q", "-b", "debian/unstable"]);
        std::fs::create_dir_all(p.join("debian")).unwrap();
        std::fs::write(
            p.join("debian/changelog"),
            "damo (3.2.8-1) unstable; urgency=medium\n\n  * New upstream version 3.2.8\n\n \
             -- M <m@x>  Wed, 17 Jun 2026 17:28:43 +0100\n",
        )
        .unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-qm", "init"]);
        git(p, &["branch", "noble"]);
        dir
    }

    const MERGE: Stages = Stages {
        merge: true,
        build: false,
    };
    const BUILD: Stages = Stages {
        merge: false,
        build: true,
    };

    #[test]
    fn parse_stages_defaults_and_values() {
        assert_eq!(parse_stages(&[]).unwrap(), MERGE);
        assert_eq!(
            parse_stages(&["all".to_string()]).unwrap(),
            Stages {
                merge: true,
                build: true
            }
        );
        assert_eq!(parse_stages(&["build".to_string()]).unwrap(), BUILD);
        assert!(parse_stages(&["bogus".to_string()]).is_err());
    }

    fn ui_dry() -> Ui {
        Ui {
            explain: false,
            dry_run: true,
        }
    }

    #[test]
    fn dry_run_merge_existing_branch() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: MERGE,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn dry_run_merge_missing_branch_is_a_create() {
        let dir = setup();
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: MERGE,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn build_only_on_missing_branch_errors() {
        let dir = setup();
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: BUILD,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn build_only_on_existing_branch_dry_run() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: BUILD,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn bulk_targets_excludes_source_and_plumbing() {
        let all: Vec<String> = ["debian/unstable", "upstream", "pristine-tar", "noble"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cfg = gbpconf::GbpConfig {
            debian_branch: None,
            upstream_branch: Some("upstream".to_string()),
            pristine_tar: Some(true),
        };
        assert_eq!(bulk_targets("debian/unstable", &all, &cfg), vec!["noble"]);
    }

    #[test]
    fn merge_rejects_target_equal_to_source() {
        let dir = setup();
        let opts = Options {
            branches: vec!["debian/unstable".to_string()],
            stages: MERGE,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn build_only_allows_the_current_branch() {
        // Already on debian/unstable; build-only against it is fine
        // (no merge means no self-merge).
        let dir = setup();
        let opts = Options {
            branches: vec!["debian/unstable".to_string()],
            stages: BUILD,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }
}
