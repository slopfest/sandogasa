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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use crate::git;
use crate::plan::{self, changelog_commit_message};
use crate::ui::{StageFailure, Ui};
use crate::{changelog, distroinfo, gbpconf, host, salsaci};

/// How often to re-poll `glab ci status` while a pipeline runs.
const POLL_INTERVAL: Duration = Duration::from_secs(8);
/// How long to wait for the just-pushed commit's pipeline to appear
/// (the post-push race) before giving up and reporting the latest.
const CREATE_TIMEOUT: Duration = Duration::from_secs(180);
/// Overall safety cap on a single watch so it can't poll forever.
const WATCH_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// In `ChrootRefresh::Auto`, refresh a pbuilder base chroot older than
/// this before building.
const CHROOT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether the build stage refreshes the pbuilder base chroot before
/// building.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChrootRefresh {
    /// Refresh only when the base tarball is older than [`CHROOT_MAX_AGE`].
    #[default]
    Auto,
    /// Always refresh, regardless of age (`--refresh-chroot`).
    Force,
    /// Never auto-refresh; build against the chroot as-is
    /// (`--no-refresh-chroot`).
    Never,
}

/// Which workflow stages to run. The head stage differs by command —
/// `rebuild` uses `merge`, `update` uses `import` — and both share the
/// `build`/`lint`/`push`/`upload`/`tag` tail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stages {
    pub merge: bool,
    pub import: bool,
    pub build: bool,
    pub lint: bool,
    pub push: bool,
    pub upload: bool,
    pub tag: bool,
}

impl Stages {
    fn any(&self) -> bool {
        self.merge || self.import || self.build || self.lint || self.push || self.upload || self.tag
    }

    /// The build/lint/push/upload/tag tail — true if any is selected.
    fn any_tail(&self) -> bool {
        self.build || self.lint || self.push || self.upload || self.tag
    }
}

/// Parse the `--stage` selector. Empty defaults to **merge** only
/// (the other stages are opt-in for now). `all` enables every stage.
pub fn parse_stages(tokens: &[String]) -> Result<Stages, String> {
    if tokens.is_empty() {
        return Ok(Stages {
            merge: true,
            ..Stages::default()
        });
    }
    let mut s = Stages::default();
    for token in tokens {
        match token.trim() {
            "merge" => s.merge = true,
            "build" => s.build = true,
            "lint" => s.lint = true,
            "push" => s.push = true,
            "upload" => s.upload = true,
            "tag" => s.tag = true,
            "all" => {
                // `all` is the build-and-verify flow; `upload` and `tag`
                // (deliberate publish/release steps) stay opt-in.
                s.merge = true;
                s.build = true;
                s.lint = true;
                s.push = true;
            }
            other => {
                return Err(format!(
                    "unknown stage '{other}' \
                     (valid: merge, build, lint, push, upload, tag, all)"
                ));
            }
        }
    }
    if !s.any() {
        return Err("no stages selected".to_string());
    }
    Ok(s)
}

/// Parse the `update --stage` selector. Like [`parse_stages`] but the
/// head stage is `import` (new upstream) instead of `merge`. Empty
/// defaults to **import** only; `all` is import + build + lint + push.
pub fn parse_update_stages(tokens: &[String]) -> Result<Stages, String> {
    if tokens.is_empty() {
        return Ok(Stages {
            import: true,
            ..Stages::default()
        });
    }
    let mut s = Stages::default();
    for token in tokens {
        match token.trim() {
            "import" => s.import = true,
            "build" => s.build = true,
            "lint" => s.lint = true,
            "push" => s.push = true,
            "upload" => s.upload = true,
            "tag" => s.tag = true,
            "all" => {
                s.import = true;
                s.build = true;
                s.lint = true;
                s.push = true;
            }
            other => {
                return Err(format!(
                    "unknown stage '{other}' \
                     (valid: import, build, lint, push, upload, tag, all)"
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
    /// In the push stage, push but don't wait for / watch CI.
    pub nowait: bool,
    /// dput target for the upload stage (e.g. `ppa:user/name` or a
    /// dput host); `None` when not uploading.
    pub upload_target: Option<String>,
    /// Explicit merge source branch; `None` uses the checked-out
    /// branch. Lets dbranch run without first checking out the Debian
    /// branch.
    pub source: Option<String>,
    /// Build stage: whether to refresh the pbuilder base chroot first.
    pub chroot_refresh: ChrootRefresh,
    /// Bulk (no-argument) run: skip the confirmation prompt.
    pub assume_yes: bool,
    /// Bulk (no-argument) run: include EOL Ubuntu releases (default
    /// skips them).
    pub include_eol: bool,
    /// Changelog urgency for the rebuild entry (default `medium`).
    pub urgency: String,
}

/// Inputs for an `update` run (new-upstream import of the Debian
/// branch).
pub struct UpdateOptions {
    /// Debian branch to update; `None` uses the checked-out branch.
    pub branch: Option<String>,
    /// Stages to run (`import` head, then the shared tail).
    pub stages: Stages,
    /// `pbuilder-dist` distribution to build against (default
    /// `testing`; `unstable` when testing is too broken).
    pub build_suite: String,
    /// In the push stage, push but don't wait for / watch CI.
    pub nowait: bool,
    /// dput target; `None` uploads to dput's default (the Debian
    /// archive), `Some("mentors")` etc. for a vetted upload.
    pub upload_target: Option<String>,
    /// Build stage: whether to refresh the pbuilder base chroot first.
    pub chroot_refresh: ChrootRefresh,
    /// Changelog urgency for the new-upstream entry (default `medium`,
    /// e.g. `high` for a security upload).
    pub urgency: String,
}

/// Run the rebuild workflow over the selected branches.
pub fn run(ui: &Ui, repo: &Path, opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    // Validate cheap preconditions before any work. A bulk run only
    // selects Ubuntu PPA targets, which need an explicit upload target;
    // check it before resolving (and prompting for) the bulk set.
    // Explicit branches are checked per-target below, where a
    // proposed-update (which uploads to the dput default) is exempt.
    if opts.branches.is_empty() && opts.stages.upload && opts.upload_target.is_none() {
        return Err(
            "the upload stage needs a target: pass --ppa <name> or --upload-target <host>".into(),
        );
    }
    // A bulk --include-eol run is for local rebuilds: Launchpad rejects
    // uploads to an EOL series' PPA, so the two can't combine.
    if opts.branches.is_empty() && opts.include_eol && opts.stages.upload {
        return Err(
            "--include-eol can't be combined with the upload stage: EOL Ubuntu \
             releases can't be uploaded to a PPA — rebuild them locally instead"
                .into(),
        );
    }
    if let Some(s) = &opts.source
        && git::rev_parse(repo, s).is_none()
    {
        return Err(format!("source branch {s} not found").into());
    }

    if !ui.dry_run {
        // glab is only needed when the push stage actually waits on CI.
        let need_glab = opts.stages.push && !opts.nowait;
        // gbp is used by both the merge stage and `gbp tag`.
        git::ensure_tools(
            opts.stages.merge || opts.stages.tag,
            opts.stages.build,
            opts.stages.lint,
            need_glab,
            opts.stages.upload,
            opts.stages.tag,
            false, // rebuild never imports
        )?;
        // glab keeps a token per host; check the one this repo lives on.
        if let Some(host) = need_glab
            .then(|| git::remote_host(repo, "origin"))
            .flatten()
        {
            git::ensure_glab_auth(repo, &host)?;
        }
    }

    // The merge source: an explicit --source, else the checked-out
    // branch. The override lets dbranch run without first checking out
    // the Debian branch.
    let source = match &opts.source {
        Some(s) => s.clone(),
        None => git::current_branch(repo)?,
    };
    let all = git::local_branches(repo)?;

    let targets = if opts.branches.is_empty() {
        resolve_bulk_targets(
            ui,
            &source,
            &all,
            opts.stages.merge,
            opts.include_eol,
            opts.assume_yes,
        )?
    } else {
        opts.branches.clone()
    };
    if targets.is_empty() {
        return Err("no target branches to rebuild".into());
    }

    // Per-target preconditions, by target type (cheap, fail fast).
    // `debian-distro-info` is consulted only for `debian/` branches.
    for target in &targets {
        let codename = target_codename(target);
        match classify_target_type(target, &codename)? {
            // A PPA upload needs an explicit target (PPA or dput host).
            TargetType::Ppa => {
                if opts.stages.upload && opts.upload_target.is_none() {
                    return Err("the upload stage needs a target: pass --ppa <name> \
                                or --upload-target <host>"
                        .into());
                }
            }
            // A proposed-update uploads to the dput default (no target
            // needed), but the whole flow needs a Debian host:
            // `gbp dch --stable` (a newer gbp), the stable build chroot,
            // and dput-to-stable all need it. A dry-run is a
            // no-execution tutorial, so it's exempt from the host check.
            TargetType::Proposed { .. } => {
                if !ui.dry_run && !host::is_debian() {
                    let host = host::os_release_id().unwrap_or_else(|| "unknown".to_string());
                    return Err(format!(
                        "{target} is a Debian proposed-update, which must be built on a \
                         Debian host (gbp dch --stable, the stable build chroot, and \
                         dput to the Debian archive need it); this host is '{host}'. \
                         Run it from a Debian environment."
                    )
                    .into());
                }
            }
            // A backport also uploads to the dput default and needs the
            // Debian build chroot + dput-to-Debian; kept symmetric with
            // proposed-updates (the merge stage alone would work
            // anywhere, but one simple rule beats a per-stage matrix).
            TargetType::Backports { .. } => {
                if !ui.dry_run && !host::is_debian() {
                    let host = host::os_release_id().unwrap_or_else(|| "unknown".to_string());
                    return Err(format!(
                        "{target} is a Debian backport, which must be built on a \
                         Debian host (the backports build chroot and dput to the \
                         Debian archive need it); this host is '{host}'. \
                         Run it from a Debian environment."
                    )
                    .into());
                }
            }
        }
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
        let location = classify_target(repo, &all, target);
        rebuild_one(
            ui,
            repo,
            &source,
            target,
            location,
            opts.stages,
            opts.nowait,
            opts.upload_target.as_deref(),
            opts.chroot_refresh,
            &opts.urgency,
            opts.assume_yes,
        )?;
    }
    Ok(())
}

/// Where a target branch lives, which decides how dbranch gets onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TargetLocation {
    /// A local branch — check it out directly.
    Local,
    /// Only on the remote (`origin/<branch>`) — check out a tracking
    /// branch from it; do NOT recreate it from the Debian branch.
    Remote,
    /// Doesn't exist anywhere — create it from the Debian branch.
    New,
}

/// Classify a target branch as local, remote-only, or new. A branch on
/// `origin` that was never checked out locally is `Remote`, not `New`,
/// so we track the existing PPA branch rather than clobbering it with a
/// fresh branch off the Debian branch.
fn classify_target(repo: &Path, local: &[String], target: &str) -> TargetLocation {
    if local.iter().any(|b| b == target) {
        TargetLocation::Local
    } else if git::remote_branch_exists(repo, "origin", target) {
        TargetLocation::Remote
    } else {
        TargetLocation::New
    }
}

/// Attach to a branch's CI pipeline via `glab` and wait for it —
/// standalone of a rebuild, e.g. after a `--nowait` push or a dropped
/// connection. Defaults to the current branch. Propagates glab's exit
/// code on pipeline failure.
pub fn watch_ci(
    ui: &Ui,
    repo: &Path,
    branch: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !ui.dry_run {
        git::ensure_tools(false, false, false, true, false, false, false)?;
        if let Some(host) = git::remote_host(repo, "origin") {
            git::ensure_glab_auth(repo, &host)?;
        }
    }
    let branch = match branch {
        Some(b) => b,
        None => git::current_branch(repo)?,
    };
    // Watch the pipeline for the commit at the branch tip (what was
    // pushed), not just "the branch", to target the right pipeline.
    let sha = git::rev_parse(repo, &branch)
        .ok_or_else(|| format!("could not resolve branch {branch} to a commit"))?;
    ui.step(&format!(
        "Watch the CI pipeline for {branch} ({})",
        sha.get(..8).unwrap_or(&sha)
    ));
    watch_pipeline(ui, repo, &sha)
}

/// Apply the PPA-branch packaging adjustments (gbp.conf `debian-branch`
/// / `debian-tag`, the salsa-ci.yml preset) to existing branches —
/// the same fixups the merge stage does when creating a branch,
/// exposed for repairing branches set up before (or outside) dbranch.
/// Defaults to the current branch; checks each out first.
pub fn fixup(
    ui: &Ui,
    repo: &Path,
    branches: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Guard against being run in the wrong repo: a Debian package
    // working tree has a debian/ directory.
    if !repo.join("debian").is_dir() {
        return Err(format!(
            "no debian/ directory in {} — not a Debian package working tree?",
            repo.display()
        )
        .into());
    }
    let targets = if branches.is_empty() {
        vec![git::current_branch(repo)?]
    } else {
        branches
    };
    let all = git::local_branches(repo)?;
    for target in &targets {
        match classify_target(repo, &all, target) {
            TargetLocation::New => {
                return Err(format!("branch {target} does not exist").into());
            }
            location => {
                ui.step(&format!("Fix up {target}"));
                checkout_existing(ui, repo, target, location)?;
                let target_type = classify_target_type(target, &target_codename(target))?;
                adjust_branch_packaging(ui, repo, target, target_type)?;
            }
        }
    }
    Ok(())
}

/// Update the Debian branch to a new upstream release: import it
/// (`gbp import-orig --uscan --pristine-tar`) and write the new-version
/// changelog entry (`gbp dch -c -R`), then run the shared
/// build/lint/push/upload/tag tail. Unlike `rebuild`, the version is a
/// plain new-upstream Debian version (no `~codename+N` suffix) and the
/// build suite (testing/unstable) is decoupled from the changelog
/// distribution.
pub fn update(
    ui: &Ui,
    repo: &Path,
    opts: &UpdateOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Uploading to the dput default (the Debian archive) needs a Debian
    // host — Ubuntu's dput doesn't understand the unstable/archive
    // target. The import/build/lint stages are fine on Ubuntu, so gate
    // only the default-target upload; an explicit `--upload-target`
    // (e.g. mentors) is the user's call and exempt. A dry-run executes
    // nothing, so it's exempt too.
    if !ui.dry_run && opts.stages.upload && opts.upload_target.is_none() && !host::is_debian() {
        let host = host::os_release_id().unwrap_or_else(|| "unknown".to_string());
        return Err(format!(
            "uploading to the default dput target (the Debian archive) needs a \
             Debian host — Ubuntu's dput doesn't understand it; this host is \
             '{host}'. Run the upload stage from a Debian environment, or pass \
             --upload-target <host> for a different destination."
        )
        .into());
    }

    if !ui.dry_run {
        // gbp drives import-orig, dch, and tag; uscan + pristine-tar are
        // the import-orig backends.
        let need_glab = opts.stages.push && !opts.nowait;
        git::ensure_tools(
            opts.stages.import || opts.stages.tag,
            opts.stages.build,
            opts.stages.lint,
            need_glab,
            opts.stages.upload,
            opts.stages.tag,
            opts.stages.import,
        )?;
        if let Some(host) = need_glab
            .then(|| git::remote_host(repo, "origin"))
            .flatten()
        {
            git::ensure_glab_auth(repo, &host)?;
        }
    }

    let branch = match &opts.branch {
        Some(b) => b.clone(),
        None => git::current_branch(repo)?,
    };

    if opts.stages.import {
        import_stage(ui, repo, &branch, &opts.urgency)?;
    } else if opts.stages.any_tail() {
        ensure_on_branch(ui, repo, &branch)?;
    }

    build_pipeline(
        ui,
        repo,
        &branch,
        &opts.build_suite,
        None,
        opts.stages,
        opts.nowait,
        opts.upload_target.as_deref(),
        opts.chroot_refresh,
        // `update` never uploads to a PPA, so the PPA pre-check (the only
        // consumer of this flag) can't fire; value is irrelevant.
        false,
    )
}

/// The import stage: get onto the Debian branch, pull and import the
/// new upstream (`gbp import-orig --uscan --pristine-tar`), and write
/// the new-version changelog entry (`gbp dch -c -R`).
fn import_stage(
    ui: &Ui,
    repo: &Path,
    branch: &str,
    urgency: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    ensure_on_branch(ui, repo, branch)?;
    ui.step(&format!("Import the new upstream release onto {branch}"));
    import_orig(ui, repo)?;
    ui.step("Generate the new-version changelog entry");
    ui.run_required(&plan::gbp_dch_release_argv(urgency), repo)
}

/// Run `gbp import-orig --uscan --pristine-tar`, self-healing the
/// "upstream already imported" case. If an earlier `update` run imported
/// the new upstream but failed before the changelog was written (e.g. a
/// bad `gbp dch` that got reverted), gbp refuses to re-import. Rather
/// than dead-end, treat that one refusal as success and let the run fall
/// through to the changelog step. Any other failure still propagates.
fn import_orig(ui: &Ui, repo: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let argv = plan::gbp_import_orig_argv();
    let (code, output) = ui.run_capture(&argv, repo)?;
    if code == 0 {
        // run_capture buffers the tool's chatter; replay it so the
        // import stays transparent (nothing to show under --dry-run).
        if !ui.quiet {
            print!("{output}");
        }
        return Ok(());
    }
    // A real failure: always surface what gbp said before deciding.
    print!("{output}");
    if plan::import_already_done(&output) {
        eprintln!(
            "note: upstream already imported — keeping the existing \
             import and regenerating the changelog"
        );
        return Ok(());
    }
    Err(Box::new(StageFailure {
        command: argv.join(" "),
        code,
    }))
}

/// Check out `branch` unless it's already current (a no-op checkout
/// just prints "Already on …" and pauses pointlessly under --explain).
fn ensure_on_branch(ui: &Ui, repo: &Path, branch: &str) -> Result<(), Box<dyn std::error::Error>> {
    if git::current_branch(repo)? == branch {
        ui.step(&format!("Already on {branch}"));
        Ok(())
    } else {
        ui.run_required(&plan::checkout_argv(branch), repo)
    }
}

/// Resolve the branch set for a no-argument (bulk) run: the local PPA
/// branches (codename is a real Ubuntu release), with EOL releases
/// skipped unless `include_eol`. Prints the resolved list (marking
/// EOL) and, on an interactive real run, asks for confirmation before
/// returning. `--dry-run` and `--yes` skip the prompt; a non-terminal
/// stdin without `--yes` is refused with a remedy rather than run
/// unconfirmed.
fn resolve_bulk_targets(
    ui: &Ui,
    source: &str,
    all: &[String],
    merge: bool,
    include_eol: bool,
    assume_yes: bool,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let release_order = distroinfo::all_codenames()?;
    let supported = distroinfo::supported_codenames()?;

    // The merge stage merges the source into each PPA branch, so the
    // source must be the Debian branch — not a PPA branch (which would
    // merge a PPA into its siblings, and the checked-out one can't be
    // merged into itself). If checked out on a PPA branch, refuse with a
    // remedy rather than silently using it. (Non-merge stages don't use
    // the source, so they're fine from any branch.)
    if merge
        && release_order
            .iter()
            .any(|c| c == plan::codename_from_branch(source))
    {
        return Err(format!(
            "the merge source is {source}, which is a PPA branch — a bulk merge needs \
             the Debian branch as its source. Check out the Debian branch, or pass \
             --source <branch>."
        )
        .into());
    }

    // (branch, is_eol), newest release first.
    let selected = select_ppa_branches(all, &release_order, &supported);

    let eol_count = selected.iter().filter(|(_, eol)| *eol).count();
    let targets: Vec<String> = selected
        .iter()
        .filter(|(_, eol)| include_eol || !*eol)
        .map(|(b, _)| b.clone())
        .collect();

    if targets.is_empty() {
        if eol_count > 0 && !include_eol {
            return Err(format!(
                "all {eol_count} candidate PPA branch(es) are EOL; pass --include-eol to rebuild them"
            )
            .into());
        }
        return Err("no PPA branches found to rebuild".into());
    }

    // Show the resolved set, newest release first, marking EOL ones.
    eprintln!("Bulk rebuild — {} branch(es), newest first:", targets.len());
    for (b, eol) in &selected {
        if !include_eol && *eol {
            continue;
        }
        eprintln!("  {b}{}", if *eol { "  (EOL)" } else { "" });
    }
    if !include_eol && eol_count > 0 {
        let skipped: Vec<&str> = selected
            .iter()
            .filter(|(_, eol)| *eol)
            .map(|(b, _)| b.as_str())
            .collect();
        eprintln!(
            "Skipping {eol_count} EOL release(s): {} (use --include-eol)",
            skipped.join(", ")
        );
    }

    // Confirm before doing real work. Dry-run and --yes proceed
    // silently; a non-terminal stdin is refused rather than hung on.
    if ui.dry_run || assume_yes {
        return Ok(targets);
    }
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(
            "refusing to bulk-rebuild without confirmation; pass --yes (stdin is not a terminal)"
                .into(),
        );
    }
    if !ui.confirm(&format!("Rebuild these {} branch(es)?", targets.len())) {
        return Err("aborted".into());
    }
    Ok(targets)
}

/// The Ubuntu PPA targets among the local branches — those whose
/// codename ([`plan::codename_from_branch`]) is a real Ubuntu release —
/// as `(branch, is_eol)` ordered **newest release first** (by position
/// in `release_order`, which is oldest-first). Non-Ubuntu branches
/// (Debian suites, `master`/`main`, plumbing) are excluded by the
/// codename filter; `is_eol` is true when the codename is no longer
/// supported. The merge source is **not** filtered out here — the Debian
/// branch isn't a Ubuntu codename so it never appears anyway, and a PPA
/// branch that happens to be checked out must still be rebuilt (it was
/// previously dropped from the set). `resolve_bulk_targets` separately
/// refuses a PPA branch as the merge source.
fn select_ppa_branches(
    all: &[String],
    release_order: &[String],
    supported: &HashSet<String>,
) -> Vec<(String, bool)> {
    let position: HashMap<&str, usize> = release_order
        .iter()
        .enumerate()
        .map(|(i, c)| (c.as_str(), i))
        .collect();
    let mut selected: Vec<(usize, String, bool)> = all
        .iter()
        .filter_map(|b| {
            let codename = plan::codename_from_branch(b);
            position
                .get(codename)
                .map(|&pos| (pos, b.clone(), !supported.contains(codename)))
        })
        .collect();
    // Newest release first; ties (two branches, same codename) by name.
    selected.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    selected.into_iter().map(|(_, b, eol)| (b, eol)).collect()
}

#[allow(clippy::too_many_arguments)]
fn rebuild_one(
    ui: &Ui,
    repo: &Path,
    source: &str,
    target: &str,
    location: TargetLocation,
    stages: Stages,
    nowait: bool,
    upload_target: Option<&str>,
    chroot_refresh: ChrootRefresh,
    urgency: &str,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let codename = target_codename(target);
    let target_type = classify_target_type(target, &codename)?;
    let kind = match target_type {
        TargetType::Ppa => "PPA".to_string(),
        TargetType::Proposed { major } => format!("Debian {major} proposed-update"),
        TargetType::Backports { major } => format!("Debian {major} backport"),
    };
    ui.step(&format!("{target} (codename: {codename}, {kind})"));

    // The version the merge stage produced, reused by build/lint/upload.
    let mut rebuilt_version: Option<String> = None;

    if stages.merge {
        merge_stage(
            ui,
            repo,
            source,
            target,
            location,
            &codename,
            target_type,
            urgency,
            &mut rebuilt_version,
        )?;
    } else if stages.any_tail() {
        // Not merging: still need to be on the target.
        if location == TargetLocation::New {
            return Err(format!(
                "branch {target} does not exist; run the merge stage to create it"
            )
            .into());
        }
        checkout_existing(ui, repo, target, location)?;
    }

    // Ubuntu PPA and proposed-update builds run in the codename's own
    // chroot; a backport scratch-builds in the base release's chroot
    // (`trixie`, not `trixie-backports` — the suffix is a changelog
    // distribution, not a pbuilder dist).
    let build_suite = build_suite_for(target_type, &codename);
    build_pipeline(
        ui,
        repo,
        target,
        &build_suite,
        rebuilt_version,
        stages,
        nowait,
        upload_target,
        chroot_refresh,
        assume_yes,
    )
}

/// The `pbuilder-dist` distribution for a target: the codename itself,
/// except a backport drops the `-backports` suffix and builds against
/// the base release.
fn build_suite_for(target_type: TargetType, codename: &str) -> String {
    match target_type {
        TargetType::Backports { .. } => backports_base(codename).unwrap_or(codename).to_string(),
        _ => codename.to_string(),
    }
}

/// The shared `build → lint → push → upload → tag` tail, driven by both
/// `rebuild` and `update`. The caller has already done the head stage
/// (merge or import) and is on `target`. `build_suite` is the
/// `pbuilder-dist` distribution (the codename for Ubuntu, testing/
/// unstable for Debian); `rebuilt_version` is the version the merge
/// stage produced, else the changelog top is read.
#[allow(clippy::too_many_arguments)]
fn build_pipeline(
    ui: &Ui,
    repo: &Path,
    target: &str,
    build_suite: &str,
    rebuilt_version: Option<String>,
    stages: Stages,
    nowait: bool,
    upload_target: Option<&str>,
    chroot_refresh: ChrootRefresh,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // The package/version are needed by build, lint, and upload; compute
    // them once (preferring the version the head stage just produced).
    let pkg_ver = if stages.build || stages.lint || stages.upload {
        let (package, top_version) = top_package_version(repo)?;
        Some((package, rebuilt_version.unwrap_or(top_version)))
    } else {
        None
    };

    if stages.build {
        let (package, version) = pkg_ver.as_ref().unwrap();
        build_stage(ui, repo, build_suite, package, version, chroot_refresh)?;
    }
    if stages.lint {
        let (_, version) = pkg_ver.as_ref().unwrap();
        lint_stage(ui, repo, build_suite, version)?;
    }
    if stages.push {
        push_stage(ui, repo, target, nowait)?;
    }
    if stages.upload {
        let (package, version) = pkg_ver.as_ref().unwrap();
        upload_stage(ui, repo, package, version, upload_target, assume_yes)?;
    }
    if stages.tag {
        tag_stage(ui, repo)?;
    }
    Ok(())
}

/// The tag stage: clean the work tree (`gbp tag` refuses a dirty one,
/// and `debuild -S` leaves a generated `debian/files`) and tag the
/// release with `gbp tag`.
fn tag_stage(ui: &Ui, repo: &Path) -> Result<(), Box<dyn std::error::Error>> {
    ui.step("Clean the work tree (gbp tag needs it clean)");
    ui.run_required(&plan::dh_clean_argv(), repo)?;
    ui.step("Tag the release");
    ui.run_required(&plan::gbp_tag_argv(), repo)
}

/// The upload stage: `dput` the source `.changes` to its archive.
/// `target` is the PPA / dput host, or `None` for dput's configured
/// default (the Debian archive — used by `update`).
fn upload_stage(
    ui: &Ui,
    repo: &Path,
    package: &str,
    version: &str,
    target: Option<&str>,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // For a PPA target, pre-flight that the package is already there;
    // if not, the --ppa was likely wrong, so confirm before uploading.
    if let Some((owner, ppa)) = target.and_then(plan::ppa_owner_name) {
        ppa_preflight(ui, repo, owner, ppa, package, assume_yes)?;
    }
    let dest = target.unwrap_or("the default dput target");
    ui.step(&format!("Upload {package} {version} to {dest}"));
    let changes = format!("../{}", plan::changes_filename(package, version));
    ui.run_required(&plan::dput_argv(target, &changes), repo)
}

/// Pre-flight a PPA upload: query Launchpad for `package` in
/// `ppa:<owner>/<ppa>`. If it isn't already published there (or the PPA
/// can't be verified — e.g. a typo'd name 404s), the `--ppa` was likely
/// wrong, so confirm before uploading (like trusting a new SSH host the
/// first time). A genuine first upload legitimately hits this and is
/// confirmed once. The prompt fires only on an interactive real run;
/// `--yes` or a non-tty warns and proceeds (don't block automation),
/// and `--dry-run`/`--explain` just narrate the `curl`. A missing `curl`
/// skips the check rather than blocking the upload.
fn ppa_preflight(
    ui: &Ui,
    repo: &Path,
    owner: &str,
    ppa: &str,
    package: &str,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !ui.dry_run && !sandogasa_cli::tool_exists("curl") {
        eprintln!("note: curl not found; skipping the ppa:{owner}/{ppa} membership check");
        return Ok(());
    }
    ui.step(&format!(
        "Check whether {package} is already in ppa:{owner}/{ppa}"
    ));
    let (code, out) = ui.run_capture(&plan::launchpad_sources_argv(owner, ppa, package), repo)?;
    if ui.dry_run {
        return Ok(()); // narrated only; nothing executed
    }

    // total_size > 0 → already published there, proceed silently.
    let count = (code == 0)
        .then(|| plan::published_source_count(&out))
        .flatten();
    if count.is_some_and(|n| n > 0) {
        return Ok(());
    }
    let reason = match count {
        Some(_) => format!("{package} is not published in ppa:{owner}/{ppa} yet"),
        None => format!("could not verify ppa:{owner}/{ppa} exists (is the name right?)"),
    };

    // Non-interactive / --yes: warn and proceed rather than block.
    if assume_yes || !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprintln!("warning: {reason}; uploading anyway");
        return Ok(());
    }
    if ui.confirm_default_no(&format!("{reason} — upload anyway?")) {
        Ok(())
    } else {
        Err("aborted".into())
    }
}

/// The push stage: publish the branch, then (unless `nowait`) attach
/// to its CI pipeline via `glab` and wait for the result.
fn push_stage(
    ui: &Ui,
    repo: &Path,
    branch: &str,
    nowait: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    ui.step(&format!("Push {branch} to origin"));
    // Once the branch tracks origin/<branch> a plain `git push` (of the
    // checked-out branch) suffices; the first push sets the upstream.
    let push = if git::has_upstream(repo, branch) {
        plan::push_argv()
    } else {
        plan::push_set_upstream_argv("origin", branch)
    };
    ui.run_required(&push, repo)?;

    if nowait {
        eprintln!("--nowait: pushed; not watching CI (use `dbranch watch-ci {branch}` later)");
        return Ok(());
    }
    ui.step("Watch the CI pipeline");
    match git::rev_parse(repo, branch) {
        Some(sha) => watch_pipeline(ui, repo, &sha),
        None => {
            eprintln!("could not resolve {branch} to a commit; skipping CI watch");
            Ok(())
        }
    }
}

/// Watch the CI pipeline for commit `sha` to completion by polling
/// `glab ci list --sha <sha>` until it reaches a terminal state.
/// Targeting the commit — not the branch — means we never latch onto
/// the *previous* commit's pipeline that GitLab hasn't replaced yet
/// (the post-push race). A `failed`/`canceled` pipeline becomes a
/// [`StageFailure`] so the run exits non-zero; `success`/`skipped`/
/// `manual` pass; if no pipeline appears within [`CREATE_TIMEOUT`] it
/// is reported as benign (nothing to watch).
fn watch_pipeline(ui: &Ui, repo: &Path, sha: &str) -> Result<(), Box<dyn std::error::Error>> {
    let argv = plan::glab_ci_list_sha_argv(sha);
    ui.show_command(&argv);
    if ui.dry_run {
        return Ok(());
    }
    // In --explain, pause once before starting the poll loop.
    ui.pause_if_explain();
    let short = sha.get(..8).unwrap_or(sha);
    let start = Instant::now();
    let mut last_status = String::new();
    let mut reported_jobs: HashSet<String> = HashSet::new();
    loop {
        let (code, out, err) = ui.run_query(&argv, repo)?;
        if code != 0 {
            // A real error (auth / host / network), not "no pipeline".
            return Err(format!(
                "`glab ci list --sha {short}` failed: {}",
                first_nonempty(&err, &out)
            )
            .into());
        }
        let Some(p) = plan::latest_pipeline(&out) else {
            // No pipeline for this commit yet — wait for it to appear.
            if start.elapsed() < CREATE_TIMEOUT {
                eprintln!("waiting for the CI pipeline for {short} to be created…");
                sleep(POLL_INTERVAL);
                continue;
            }
            eprintln!("no CI pipeline found for {short} (nothing to watch)");
            return Ok(());
        };
        if p.status != last_status {
            if last_status.is_empty() && !p.web_url.is_empty() {
                eprintln!("pipeline {} — {}", p.id, p.web_url);
            }
            eprintln!("pipeline {} for {short}: {}", p.id, p.status);
            last_status = p.status.clone();
        }
        // Report each job as it finishes (best-effort; the pipeline
        // poll above is the source of truth for pass/fail).
        report_finished_jobs(ui, repo, p.id, &mut reported_jobs);
        if plan::is_terminal_status(&p.status) {
            return match p.status.as_str() {
                "failed" | "canceled" => Err(Box::new(crate::ui::StageFailure {
                    command: format!("CI pipeline {} ({})", p.id, p.status),
                    code: 1,
                })),
                _ => Ok(()),
            };
        }
        if start.elapsed() > WATCH_TIMEOUT {
            eprintln!(
                "giving up watching pipeline {} after the time cap (still {})",
                p.id, p.status
            );
            return Ok(());
        }
        sleep(POLL_INTERVAL);
    }
}

/// The first of two strings that is non-empty after trimming.
fn first_nonempty<'a>(a: &'a str, b: &'a str) -> &'a str {
    let a = a.trim();
    if a.is_empty() { b.trim() } else { a }
}

/// Poll a pipeline's jobs and print each one as it first reaches a
/// terminal state, tracking which have already been reported.
/// Best-effort: a failed jobs query is ignored (the pipeline-level
/// poll drives pass/fail), so transient hiccups don't abort the watch.
fn report_finished_jobs(ui: &Ui, repo: &Path, pipeline_id: i64, reported: &mut HashSet<String>) {
    let Ok((code, out, _err)) = ui.run_query(&plan::glab_pipeline_jobs_argv(pipeline_id), repo)
    else {
        return;
    };
    if code != 0 {
        return;
    }
    for job in plan::parse_jobs(&out) {
        if plan::is_terminal_status(&job.status) && reported.insert(job.name.clone()) {
            eprintln!("{}", format_job_line(&job));
        }
    }
}

/// One-line progress marker for a finished job: `✓` for success, `✗`
/// for failed/canceled (with the status), `•` for skipped/manual etc.
fn format_job_line(job: &plan::JobInfo) -> String {
    match job.status.as_str() {
        "success" => format!("  ✓ {} ({})", job.name, job.stage),
        "failed" | "canceled" => format!("  ✗ {} ({}) — {}", job.name, job.stage, job.status),
        other => format!("  • {} ({}) — {}", job.name, job.stage, other),
    }
}

/// Check out an existing target branch: a local one directly, or a
/// fresh tracking branch from `origin/<branch>` when it only exists on
/// the remote (so we build on the real PPA branch, not a new one).
fn checkout_existing(
    ui: &Ui,
    repo: &Path,
    target: &str,
    location: TargetLocation,
) -> Result<(), Box<dyn std::error::Error>> {
    match location {
        TargetLocation::Local => {
            // Skip a no-op checkout when we're already on the branch —
            // it just prints "Already on '<branch>'" and, under
            // --explain, adds a pointless pause on a do-nothing command.
            if git::current_branch(repo)? == target {
                ui.step(&format!("Already on {target}"));
                Ok(())
            } else {
                ui.run_required(&plan::checkout_argv(target), repo)
            }
        }
        TargetLocation::Remote => {
            ui.step(&format!("Check out {target} tracking origin/{target}"));
            ui.run_required(
                &plan::checkout_new_argv(target, &format!("origin/{target}")),
                repo,
            )
        }
        TargetLocation::New => {
            unreachable!("checkout_existing must not be called for a new branch")
        }
    }
}

/// The merge stage: get onto the target branch (create if needed),
/// merge the Debian branch, resolve the changelog conflict, and write
/// the normalized rebuild entry.
#[allow(clippy::too_many_arguments)]
fn merge_stage(
    ui: &Ui,
    repo: &Path,
    source: &str,
    target: &str,
    location: TargetLocation,
    codename: &str,
    target_type: TargetType,
    urgency: &str,
    out_version: &mut Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    if location == TargetLocation::New {
        // A new PPA branch starts from the Debian branch (already has
        // the new packaging) — no merge needed.
        ui.step(&format!("Create {target} from {source}"));
        ui.run_required(&plan::checkout_new_argv(target, source), repo)?;
    } else {
        checkout_existing(ui, repo, target, location)?;
        // The changelog conflict is expected and resolved
        // deterministically; in dry-run we always narrate it.
        let merged_ok = ui.run(&plan::merge_argv(source), repo)?;
        if ui.dry_run || !merged_ok {
            ui.step("Resolve the debian/changelog conflict");
            if !ui.dry_run {
                resolve_changelog(repo)?;
            }
            ui.explain_diff(repo, &["debian/changelog"]);
            ui.run_required(&plan::add_changelog_argv(), repo)?;
            ui.run_required(&plan::commit_merge_argv(), repo)?;
        }
    }

    // Ensure the branch's packaging is adjusted before `gbp dch` runs:
    // gbp.conf's debian-branch must point at this branch (not the
    // Debian branch), or gbp dch refuses ("not on branch <x>"). This
    // is a no-op on an already-adjusted branch, and self-heals one
    // created outside dbranch. The files it changed are listed in the
    // rebuild changelog entry.
    let changes = adjust_branch_packaging(ui, repo, target, target_type)?;

    let version = {
        let text = std::fs::read_to_string(repo.join("debian/changelog"))?;
        target_type
            .version(&text, codename)
            .ok_or("could not determine the Debian version from debian/changelog")?
    };

    ui.step("Generate the rebuild changelog entry");
    // PPA and backports use `--bpo` (for a backport the codename is
    // already `<release>-backports`, the real distribution); a
    // proposed-update uses `--stable` (the honest command). Either way
    // the entry is normalized afterward, so gbp's exact version/body —
    // including `--bpo`'s trailing period and any merged-delta lines —
    // is provisional.
    let dch = match target_type {
        TargetType::Ppa | TargetType::Backports { .. } => plan::gbp_dch_argv(codename, urgency),
        TargetType::Proposed { .. } => plan::gbp_dch_stable_argv(urgency),
    };
    ui.run_required(&dch, repo)?;

    ui.step(&format!(
        "Normalize the entry to {version} / \"Rebuild for {codename}\""
    ));
    if !ui.dry_run {
        let text = std::fs::read_to_string(repo.join("debian/changelog"))?;
        let normalized = changelog::normalize_top_stanza(
            &text,
            &version,
            codename,
            &changes.created,
            &changes.adjusted,
        )?;
        std::fs::write(repo.join("debian/changelog"), normalized)?;
    }
    ui.explain_diff(repo, &["debian/changelog"]);
    ui.run_required(
        &plan::commit_changelog_argv(&changelog_commit_message(&version)),
        repo,
    )?;

    *out_version = Some(version);
    Ok(())
}

/// Apply the PPA-branch packaging adjustments and commit each that
/// changed: point gbp.conf's `debian-branch` at the branch and set its
/// `debian-tag` to the `ubuntu/%(version)s` format (so `gbp tag` tags
/// under `ubuntu/` instead of the default `debian/`), and inject the
/// salsa-ci.yml PPA-rebuild preset. Idempotent and skipped when a file
/// is absent — run both when creating a new branch and to fix up an
/// existing one. Returns the display names of the files actually
/// changed (e.g. `["gbp.conf", "salsa-ci.yml"]`), which the merge stage
/// lists in the rebuild changelog entry.
/// Which packaging files [`adjust_branch_packaging`] touched, split by
/// whether each was created from scratch or edited — the changelog
/// entry words the two differently, matching the per-file commits.
#[derive(Debug, Default, PartialEq, Eq)]
struct PackagingChanges {
    created: Vec<String>,
    adjusted: Vec<String>,
}

fn adjust_branch_packaging(
    ui: &Ui,
    repo: &Path,
    target: &str,
    target_type: TargetType,
) -> Result<PackagingChanges, Box<dyn std::error::Error>> {
    // salsa-ci RELEASE + whether the backports-style relaxations apply
    // depend on the target type: a PPA builds against unstable with
    // relaxations; a proposed-update against its stable codename and a
    // backport against `<codename>-backports` (a supported salsa-ci
    // RELEASE, whose image also enables the backports apt repo), both
    // with no relaxations — they're legitimate Debian builds. Without
    // the RELEASE pin salsa-ci defaults to building against sid.
    let codename = target_codename(target);
    let (release, add_backports) = match target_type {
        TargetType::Ppa => ("unstable".to_string(), true),
        // For a backport the codename is already `<release>-backports`.
        TargetType::Proposed { .. } | TargetType::Backports { .. } => (codename, false),
    };
    // A backports branch only needs `debian-branch`: it lives in the
    // `debian/` namespace, so gbp's default `debian/%(version)s` tag is
    // already right. Everything else also gets `debian-tag`.
    let tag_format = match target_type {
        TargetType::Backports { .. } => None,
        _ => Some(plan::debian_tag_format(target)),
    };
    let keys_label = match &tag_format {
        Some(_) => "debian-branch, debian-tag",
        None => "debian-branch",
    };
    let mut changes = PackagingChanges::default();
    if repo.join("debian/gbp.conf").exists() {
        ui.step(&format!("Adjust gbp.conf ({keys_label}) for {target}"));
        let changed = edit_file(ui, repo, "debian/gbp.conf", |text| {
            let text = gbpconf::set_key(text, "debian-branch", target, None);
            match &tag_format {
                // Keep debian-tag right under debian-branch.
                Some(tag) => Some(gbpconf::set_key(
                    &text,
                    "debian-tag",
                    tag,
                    Some("debian-branch"),
                )),
                None => Some(text),
            }
        })?;
        if changed {
            ui.explain_diff(repo, &["debian/gbp.conf"]);
            ui.run_required(
                &plan::commit_file_argv(
                    &format!("Adjust gbp.conf for {target}"),
                    "debian/gbp.conf",
                ),
                repo,
            )?;
            changes.adjusted.push("gbp.conf".to_string());
        }
    } else {
        // No gbp.conf on the source branch — common when the rebuilder
        // isn't the maintainer and keeps the Debian branch clean. Create
        // one on *this* branch so `gbp dch` / `gbp tag` target it (they'd
        // otherwise default `debian-branch` to the Debian branch and
        // refuse: "not on branch <x>").
        ui.step(&format!("Create gbp.conf ({keys_label}) for {target}"));
        if !ui.dry_run {
            std::fs::write(
                repo.join("debian/gbp.conf"),
                gbpconf::new_config(target, tag_format.as_deref()),
            )?;
        }
        // A new file isn't picked up by `git commit <file>`; stage it,
        // show the staged diff, then commit.
        ui.run_required(&plan::git_add_argv("debian/gbp.conf"), repo)?;
        ui.explain_diff_cached(repo, &["debian/gbp.conf"]);
        ui.run_required(
            &plan::commit_file_argv(&format!("Create gbp.conf for {target}"), "debian/gbp.conf"),
            repo,
        )?;
        changes.created.push("gbp.conf".to_string());
    }
    if repo.join("debian/salsa-ci.yml").exists() {
        ui.step(&format!("Adjust salsa-ci.yml for {target}"));
        let changed = edit_file(ui, repo, "debian/salsa-ci.yml", |text| {
            salsaci::adjust_salsa_ci(text, &release, add_backports)
        })?;
        if changed {
            ui.explain_diff(repo, &["debian/salsa-ci.yml"]);
            ui.run_required(
                &plan::commit_file_argv(
                    &format!("Adjust salsa-ci.yml for {target}"),
                    "debian/salsa-ci.yml",
                ),
                repo,
            )?;
            changes.adjusted.push("salsa-ci.yml".to_string());
        }
    }
    Ok(changes)
}

/// Apply an in-place text transform to a repo file, returning whether
/// the file changed. In `--dry-run` nothing is read or written and it
/// returns `true`, so the follow-up commit is still narrated. A
/// transform returning `None` (unexpected format) is left unchanged
/// with a warning.
fn edit_file(
    ui: &Ui,
    repo: &Path,
    rel: &str,
    transform: impl FnOnce(&str) -> Option<String>,
) -> Result<bool, Box<dyn std::error::Error>> {
    if ui.dry_run {
        return Ok(true);
    }
    let path = repo.join(rel);
    let text = std::fs::read_to_string(&path)?;
    match transform(&text) {
        Some(new) if new != text => {
            std::fs::write(&path, new)?;
            Ok(true)
        }
        Some(_) => Ok(false),
        None => {
            eprintln!("{rel}: unexpected format; left unchanged");
            Ok(false)
        }
    }
}

/// The build stage: build the source package and scratch-build it in
/// the codename's pbuilder chroot.
fn build_stage(
    ui: &Ui,
    repo: &Path,
    codename: &str,
    package: &str,
    version: &str,
    chroot_refresh: ChrootRefresh,
) -> Result<(), Box<dyn std::error::Error>> {
    ui.step("Build the source package");
    ui.run_required(&plan::debuild_argv(), repo)?;

    // The chroot's base tarball: create it the first time, otherwise
    // refresh it (per the policy) so the build isn't against stale
    // packages. A missing/locatable check first ($HOME may be unset).
    let base = plan::pbuilder_base_tgz(codename);
    if base.as_ref().is_none_or(|p| !p.exists()) {
        ui.step(&format!(
            "Create the {codename} pbuilder chroot (no base tarball yet)"
        ));
        ui.run_required(&plan::pbuilder_create_argv(codename), repo)?;
    } else {
        let stale = base
            .as_deref()
            .is_some_and(|p| chroot_is_stale(p, CHROOT_MAX_AGE));
        let refresh = match chroot_refresh {
            ChrootRefresh::Force => true,
            ChrootRefresh::Never => false,
            ChrootRefresh::Auto => stale,
        };
        if refresh {
            ui.step(&format!("Refresh the {codename} pbuilder chroot"));
            ui.run_required(&plan::pbuilder_update_argv(codename), repo)?;
        }
    }

    let dsc = format!("../{}", plan::dsc_filename(package, version));
    ui.step(&format!("Scratch-build in the {codename} chroot"));
    ui.run_required(&plan::pbuilder_argv(codename, &dsc), repo)?;
    Ok(())
}

/// Whether a file's mtime is older than `max_age`. `false` when the
/// mtime can't be read (don't force a refresh on uncertainty).
fn chroot_is_stale(path: &Path, max_age: Duration) -> bool {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|mtime| SystemTime::now().duration_since(mtime).ok())
        .is_some_and(|age| age > max_age)
}

/// The lint stage: run `lintian` on the **`.deb`s** the build
/// produced in `~/pbuilder/<codename>_result/`. Linting the binaries
/// directly (rather than the `.changes`) avoids lintian re-unpacking
/// the source — the `.orig.tar.gz` isn't in the result dir, and
/// `debuild -S` already linted the source anyway. A non-zero exit is
/// reported but does not fail the run; rebuild lint tags are mostly
/// inherited from Debian.
fn lint_stage(
    ui: &Ui,
    repo: &Path,
    codename: &str,
    version: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    ui.step("Lint the built .deb packages");
    let Some(dir) = plan::pbuilder_result_dir(codename) else {
        eprintln!("could not locate the pbuilder result dir ($HOME unset); skipping lint");
        return Ok(());
    };
    let ver = plan::version_no_epoch(version);

    if ui.dry_run {
        // Can't enumerate (the dir may not exist yet); narrate a
        // shell-expandable glob of this build's .debs.
        let pattern = format!("{}/*_{ver}_*.deb", dir.display());
        ui.show_command(&plan::lintian_argv(&[pattern]));
        return Ok(());
    }

    let debs = matching_debs(&dir, ver);
    if debs.is_empty() {
        eprintln!(
            "no .deb files for {version} in {}; did the build stage run?",
            dir.display()
        );
        return Ok(());
    }
    let n = debs.len();
    let (code, output) = ui.run_capture(&plan::lintian_argv(&debs), repo)?;
    // lintian is silent when clean — echo its tags (unless --quiet),
    // then always print a summary so a clean run is visibly confirmed.
    if !ui.quiet {
        print!("{output}");
    }
    println!("lintian: {} ({n} .deb(s))", summarize_lintian(&output));
    // Use lintian's own exit convention (non-zero on error-level tags)
    // and propagate it.
    if code != 0 {
        return Err(Box::new(crate::ui::StageFailure {
            command: format!("lintian -I ({n} .deb(s))"),
            code,
        }));
    }
    Ok(())
}

/// One-line tally of lintian tags by severity from its output.
fn summarize_lintian(output: &str) -> String {
    let (mut e, mut w, mut i, mut p) = (0u32, 0u32, 0u32, 0u32);
    for line in output.lines() {
        match line.get(..3) {
            Some("E: ") => e += 1,
            Some("W: ") => w += 1,
            Some("I: ") => i += 1,
            Some("P: ") => p += 1,
            _ => {}
        }
    }
    if e + w + i + p == 0 {
        return "clean — no tags".to_string();
    }
    let mut parts = vec![format!("{e} error(s)"), format!("{w} warning(s)")];
    if i > 0 {
        parts.push(format!("{i} info"));
    }
    if p > 0 {
        parts.push(format!("{p} pedantic"));
    }
    parts.join(", ")
}

/// The built `.deb` paths for this version in `dir` (both arch and
/// `_all` packages, matched by the `_<version>_` infix), sorted.
fn matching_debs(dir: &Path, version_no_epoch: &str) -> Vec<String> {
    let infix = format!("_{version_no_epoch}_");
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "deb")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(&infix))
        })
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    out.sort();
    out
}

/// The codename for a target branch — the branch name's basename
/// (`ubuntu/<rel>` → `<rel>`, `noble` → `noble`). For a properly set-up
/// PPA branch this also equals gbp.conf's `debian-branch`; deriving it
/// from the branch name avoids being misled by an unadjusted gbp.conf
/// still pointing at the Debian branch.
fn target_codename(target: &str) -> String {
    plan::codename_from_branch(target).to_string()
}

/// What kind of rebuild target a branch is — drives the version scheme
/// (the changelog distribution is the codename in both cases).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetType {
    /// Ubuntu PPA branch: version `<base>~<codename>+<N>`.
    Ppa,
    /// Debian stable proposed-update branch (`debian/<codename>`):
    /// version `<base>~deb<major>u<M>`.
    Proposed { major: u32 },
    /// Debian backports branch (`debian/<codename>-backports`):
    /// version `<base>~bpo<major>+<M>`.
    Backports { major: u32 },
}

impl TargetType {
    /// The fresh version for this target, given the merged changelog and
    /// the codename — `changelog::rebuild_version` for a PPA,
    /// `changelog::proposed_version` for a proposed-update, or
    /// `changelog::backports_version` for a backport.
    fn version(self, changelog: &str, codename: &str) -> Option<String> {
        match self {
            TargetType::Ppa => changelog::rebuild_version(changelog, codename),
            TargetType::Proposed { major } => changelog::proposed_version(changelog, major),
            TargetType::Backports { major } => changelog::backports_version(changelog, major),
        }
    }
}

/// The base release of a backports codename (`trixie-backports` →
/// `trixie`); `None` when the codename has no `-backports` suffix.
fn backports_base(codename: &str) -> Option<&str> {
    codename
        .strip_suffix("-backports")
        .filter(|b| !b.is_empty())
}

/// Classify a target branch. A `debian/<codename>-backports` branch is a
/// backport; a `debian/<codename>` branch whose codename is a numbered
/// Debian release (via `debian-distro-info`) is a proposed-update;
/// everything else is a PPA. `debian-distro-info` is only consulted for
/// `debian/`-namespaced branches, so a plain Ubuntu PPA rebuild never
/// needs it.
fn classify_target_type(
    target: &str,
    codename: &str,
) -> Result<TargetType, Box<dyn std::error::Error>> {
    if target.starts_with("debian/")
        && let Some(base) = backports_base(codename)
    {
        return match distroinfo::debian_major(base)? {
            Some(major) => Ok(TargetType::Backports { major }),
            None => {
                Err(format!("unknown Debian release '{base}' in backports target {target}").into())
            }
        };
    }
    // The rolling suites are never proposed-update targets; skip the
    // tool call (so a plain run never needs debian-distro-info for them).
    if target.starts_with("debian/")
        && !matches!(codename, "unstable" | "sid" | "experimental" | "rc-buggy")
        && let Some(major) = distroinfo::debian_major(codename)?
    {
        return Ok(TargetType::Proposed { major });
    }
    Ok(TargetType::Ppa)
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
        import: false,
        build: false,
        lint: false,
        push: false,
        upload: false,
        tag: false,
    };
    const BUILD: Stages = Stages {
        merge: false,
        import: false,
        build: true,
        lint: false,
        push: false,
        upload: false,
        tag: false,
    };

    #[test]
    fn parse_stages_defaults_and_values() {
        assert_eq!(parse_stages(&[]).unwrap(), MERGE);
        assert_eq!(
            parse_stages(&["all".to_string()]).unwrap(),
            Stages {
                merge: true,
                import: false,
                build: true,
                lint: true,
                push: true,
                // `all` excludes upload/tag (deliberate, opt-in).
                upload: false,
                tag: false,
            }
        );
        assert_eq!(parse_stages(&["build".to_string()]).unwrap(), BUILD);
        assert_eq!(
            parse_stages(&["lint".to_string()]).unwrap(),
            Stages {
                lint: true,
                ..Stages::default()
            }
        );
        assert_eq!(
            parse_stages(&["push".to_string()]).unwrap(),
            Stages {
                push: true,
                ..Stages::default()
            }
        );
        assert_eq!(
            parse_stages(&["upload".to_string()]).unwrap(),
            Stages {
                upload: true,
                ..Stages::default()
            }
        );
        assert_eq!(
            parse_stages(&["tag".to_string()]).unwrap(),
            Stages {
                tag: true,
                ..Stages::default()
            }
        );
        assert!(parse_stages(&["bogus".to_string()]).is_err());
    }

    #[test]
    fn parse_update_stages_defaults_and_values() {
        // Defaults to import; `all` is import + build + lint + push.
        assert_eq!(
            parse_update_stages(&[]).unwrap(),
            Stages {
                import: true,
                ..Stages::default()
            }
        );
        assert_eq!(
            parse_update_stages(&["all".to_string()]).unwrap(),
            Stages {
                import: true,
                build: true,
                lint: true,
                push: true,
                ..Stages::default()
            }
        );
        assert_eq!(
            parse_update_stages(&["build".to_string()]).unwrap(),
            Stages {
                build: true,
                ..Stages::default()
            }
        );
        // `merge` is a rebuild-only stage; not valid for update.
        assert!(parse_update_stages(&["merge".to_string()]).is_err());
        assert!(parse_update_stages(&["bogus".to_string()]).is_err());
    }

    fn ui_dry() -> Ui {
        Ui {
            explain: false,
            dry_run: true,
            quiet: false,
        }
    }

    #[test]
    fn update_dry_run_imports_on_current_branch() {
        let dir = setup(); // on debian/unstable
        let opts = UpdateOptions {
            branch: None,
            stages: parse_update_stages(&[]).unwrap(), // import
            build_suite: "testing".to_string(),
            nowait: false,
            upload_target: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
        };
        update(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn dry_run_merge_existing_branch() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn dry_run_merge_missing_branch_is_a_create() {
        let dir = setup();
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn source_override_is_used_as_merge_source() {
        let dir = setup();
        // On debian/unstable, but merge from noble into a new branch.
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: Some("noble".to_string()),
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn source_override_unknown_branch_errors() {
        let dir = setup();
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: Some("does-not-exist".to_string()),
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn classify_target_distinguishes_local_remote_new() {
        let dir = setup();
        let p = dir.path();
        // A remote-only branch: a remote-tracking ref, no local branch.
        git(
            p,
            &["update-ref", "refs/remotes/origin/ubuntu/questing", "HEAD"],
        );
        let local = crate::git::local_branches(p).unwrap();
        assert_eq!(classify_target(p, &local, "noble"), TargetLocation::Local);
        assert_eq!(
            classify_target(p, &local, "ubuntu/questing"),
            TargetLocation::Remote
        );
        assert_eq!(
            classify_target(p, &local, "ubuntu/plucky"),
            TargetLocation::New
        );
    }

    #[test]
    fn dry_run_merge_remote_only_branch_tracks_origin() {
        let dir = setup();
        let p = dir.path();
        // origin has the branch but it was never checked out locally —
        // dbranch must track it, not recreate it from the Debian branch.
        git(
            p,
            &["update-ref", "refs/remotes/origin/ubuntu/questing", "HEAD"],
        );
        let opts = Options {
            branches: vec!["ubuntu/questing".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), p, &opts).unwrap();
    }

    #[test]
    fn dry_run_create_branch_adjusts_packaging() {
        let dir = setup();
        let p = dir.path();
        // A new branch with packaging files to adjust.
        std::fs::write(
            p.join("debian/gbp.conf"),
            "[DEFAULT]\npristine-tar = True\ndebian-branch = debian/unstable\nupstream-branch = upstream\n",
        )
        .unwrap();
        std::fs::write(
            p.join("debian/salsa-ci.yml"),
            "---\ninclude:\n  - x\nvariables:\n  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'\n",
        )
        .unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-qm", "add packaging"]);
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), p, &opts).unwrap();
    }

    #[test]
    fn build_suite_drops_backports_suffix() {
        // A backport scratch-builds against the base release chroot.
        assert_eq!(
            build_suite_for(TargetType::Backports { major: 13 }, "trixie-backports"),
            "trixie"
        );
        // PPA and proposed-update build in the codename's own chroot.
        assert_eq!(build_suite_for(TargetType::Ppa, "resolute"), "resolute");
        assert_eq!(
            build_suite_for(TargetType::Proposed { major: 13 }, "trixie"),
            "trixie"
        );
    }

    #[test]
    fn backports_base_strips_suffix() {
        assert_eq!(backports_base("trixie-backports"), Some("trixie"));
        assert_eq!(backports_base("bookworm-backports"), Some("bookworm"));
        assert_eq!(backports_base("trixie"), None);
        assert_eq!(backports_base("-backports"), None);
        assert_eq!(backports_base("noble"), None);
    }

    #[test]
    fn backports_gbp_conf_branch_only_and_salsa_release_pin() {
        // The iptstate debian/trixie-backports spec: a created gbp.conf
        // carries only debian-branch (gbp's default debian/%(version)s
        // tag is right for debian/* branches), and salsa-ci.yml gets
        // RELEASE pinned to `trixie-backports` (without the pin salsa-ci
        // builds against sid) with no PPA-style relaxations. Also
        // exercises the mixed created + adjusted split.
        let dir = setup();
        let p = dir.path();
        git(p, &["config", "user.email", "t@x"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "commit.gpgsign", "false"]);
        let salsa = "---\ninclude:\n  - https://salsa.debian.org/salsa-ci-team/pipeline/raw/master/recipes/debian.yml\n";
        std::fs::write(p.join("debian/salsa-ci.yml"), salsa).unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-qm", "add salsa ci"]);
        git(p, &["checkout", "-qb", "debian/trixie-backports"]);

        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        };
        let changes = adjust_branch_packaging(
            &ui,
            p,
            "debian/trixie-backports",
            TargetType::Backports { major: 13 },
        )
        .unwrap();

        assert_eq!(changes.created, vec!["gbp.conf".to_string()]);
        assert_eq!(changes.adjusted, vec!["salsa-ci.yml".to_string()]);
        let text = std::fs::read_to_string(p.join("debian/gbp.conf")).unwrap();
        assert_eq!(text, "[DEFAULT]\ndebian-branch = debian/trixie-backports\n");
        assert!(!text.contains("debian-tag"));
        let ci = std::fs::read_to_string(p.join("debian/salsa-ci.yml")).unwrap();
        assert!(ci.contains("RELEASE: \"trixie-backports\""), "{ci}");
        assert!(!ci.contains("adjust for backports"), "{ci}");
    }

    #[test]
    fn creates_gbp_conf_when_source_branch_has_none() {
        // The reported case: the maintainer keeps the Debian branch clean
        // (no debian/gbp.conf), so a rebuild branch must get one created —
        // otherwise `gbp dch` defaults debian-branch to the Debian branch
        // and refuses. setup() has no gbp.conf.
        let dir = setup();
        let p = dir.path();
        // Repo-local identity so the real commit works without depending
        // on the developer's / sandbox's global git config.
        git(p, &["config", "user.email", "t@x"]);
        git(p, &["config", "user.name", "T"]);
        git(p, &["config", "commit.gpgsign", "false"]);
        git(p, &["checkout", "-qb", "ubuntu/resolute"]);

        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: true,
        };
        let changes = adjust_branch_packaging(&ui, p, "ubuntu/resolute", TargetType::Ppa).unwrap();

        assert!(changes.created.contains(&"gbp.conf".to_string()));
        let text = std::fs::read_to_string(p.join("debian/gbp.conf")).unwrap();
        assert!(text.contains("debian-branch = ubuntu/resolute"), "{text}");
        assert!(text.contains("debian-tag = ubuntu/%(version)s"), "{text}");
        // The created file was committed (nothing left uncommitted).
        let status = Command::new("git")
            .args(["status", "--porcelain", "debian/gbp.conf"])
            .current_dir(p)
            .output()
            .unwrap();
        assert!(status.stdout.is_empty(), "gbp.conf not committed");
    }

    #[test]
    fn fixup_dry_run_adjusts_existing_branch() {
        let dir = setup();
        let p = dir.path();
        std::fs::write(
            p.join("debian/gbp.conf"),
            "[DEFAULT]\npristine-tar = True\ndebian-branch = debian/unstable\nupstream-branch = upstream\n",
        )
        .unwrap();
        std::fs::write(
            p.join("debian/salsa-ci.yml"),
            "---\nvariables:\n  SALSA_CI_DISABLE_BUILD_PACKAGE_ANY: '1'\n",
        )
        .unwrap();
        git(p, &["add", "-A"]);
        git(p, &["commit", "-qm", "add packaging"]);
        // noble exists locally in setup().
        fixup(&ui_dry(), p, vec!["noble".to_string()]).unwrap();
    }

    #[test]
    fn chroot_is_stale_by_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("base.tgz");
        std::fs::write(&p, "x").unwrap();
        // A just-written file is fresh; a missing one is "not stale".
        assert!(!chroot_is_stale(&p, Duration::from_secs(3600)));
        assert!(!chroot_is_stale(
            &dir.path().join("nope"),
            Duration::from_secs(3600)
        ));
        // Backdate the mtime → stale.
        Command::new("touch")
            .args(["-d", "2000-01-01"])
            .arg(&p)
            .status()
            .unwrap();
        assert!(chroot_is_stale(&p, Duration::from_secs(3600)));
    }

    #[test]
    fn fixup_unknown_branch_errors() {
        let dir = setup();
        assert!(fixup(&ui_dry(), dir.path(), vec!["ubuntu/nope".to_string()]).is_err());
    }

    #[test]
    fn fixup_without_debian_dir_errors() {
        // A directory with no debian/ (wrong repo) is rejected up front.
        let dir = tempfile::tempdir().unwrap();
        assert!(fixup(&ui_dry(), dir.path(), vec!["noble".to_string()]).is_err());
    }

    #[test]
    fn edit_file_writes_when_changed_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("debian")).unwrap();
        std::fs::write(
            p.join("debian/gbp.conf"),
            "debian-branch = debian/unstable\n",
        )
        .unwrap();
        let ui = Ui {
            explain: false,
            dry_run: false,
            quiet: false,
        };
        let transform = |t: &str| Some(gbpconf::set_key(t, "debian-branch", "noble", None));
        assert!(edit_file(&ui, p, "debian/gbp.conf", transform).unwrap());
        let after = std::fs::read_to_string(p.join("debian/gbp.conf")).unwrap();
        assert!(after.contains("debian-branch = noble"));
        // Re-applying the same transform reports no change.
        let transform = |t: &str| Some(gbpconf::set_key(t, "debian-branch", "noble", None));
        assert!(!edit_file(&ui, p, "debian/gbp.conf", transform).unwrap());
    }

    #[test]
    fn format_job_line_marks_by_status() {
        let job = |status: &str| plan::JobInfo {
            id: 1,
            name: "build source".to_string(),
            stage: "build".to_string(),
            status: status.to_string(),
        };
        assert_eq!(format_job_line(&job("success")), "  ✓ build source (build)");
        assert_eq!(
            format_job_line(&job("failed")),
            "  ✗ build source (build) — failed"
        );
        assert_eq!(
            format_job_line(&job("skipped")),
            "  • build source (build) — skipped"
        );
    }

    #[test]
    fn build_only_on_missing_branch_errors() {
        let dir = setup();
        let opts = Options {
            branches: vec!["ubuntu/plucky".to_string()],
            stages: BUILD,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn build_only_on_existing_branch_dry_run() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: BUILD,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn build_only_when_already_on_target_skips_checkout() {
        let dir = setup();
        let p = dir.path();
        // Already on the target: checkout would be a no-op, so the
        // Local path takes the "Already on …" branch instead.
        git(p, &["checkout", "-q", "noble"]);
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: BUILD,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), p, &opts).unwrap();
    }

    #[test]
    fn push_stage_dry_run_pushes_and_watches() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: Stages {
                push: true,
                ..Stages::default()
            },
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn upload_without_target_errors() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: Stages {
                upload: true,
                ..Stages::default()
            },
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn upload_dry_run_with_ppa_target() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: Stages {
                upload: true,
                ..Stages::default()
            },
            nowait: false,
            upload_target: Some("ppa:michel/sugarjar".to_string()),
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn tag_dry_run_cleans_then_tags() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: Stages {
                tag: true,
                ..Stages::default()
            },
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn push_stage_nowait_dry_run_skips_watch() {
        let dir = setup();
        let opts = Options {
            branches: vec!["noble".to_string()],
            stages: Stages {
                push: true,
                ..Stages::default()
            },
            nowait: true,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }

    #[test]
    fn watch_ci_dry_run_with_explicit_branch() {
        let dir = setup();
        watch_ci(&ui_dry(), dir.path(), Some("noble".to_string())).unwrap();
    }

    #[test]
    fn watch_ci_dry_run_defaults_to_current_branch() {
        let dir = setup();
        // setup() leaves debian/unstable checked out.
        watch_ci(&ui_dry(), dir.path(), None).unwrap();
    }

    #[test]
    fn summarize_lintian_counts_or_reports_clean() {
        assert_eq!(summarize_lintian(""), "clean — no tags");
        assert_eq!(summarize_lintian("N: just a note\n"), "clean — no tags");
        let out = "\
W: damo: some-warning here
I: damo: an-info-tag
I: python3-damo: another-info
E: damo: an-error\n";
        assert_eq!(summarize_lintian(out), "1 error(s), 1 warning(s), 2 info");
    }

    #[test]
    fn matching_debs_filters_by_version_and_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        for f in [
            "damo_3.2.8-1~noble+1_arm64.deb",
            "python3-damo_3.2.8-1~noble+1_all.deb",
            "damo-dbgsym_3.2.8-1~noble+1_arm64.ddeb", // debug, excluded
            "damo_3.2.8-1~noble+1_arm64.changes",     // not a .deb
            "other_9.9-1~noble+1_arm64.deb",          // different version
        ] {
            std::fs::write(p.join(f), "").unwrap();
        }
        let names: Vec<String> = matching_debs(p, "3.2.8-1~noble+1")
            .iter()
            .map(|s| {
                Path::new(s)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                "damo_3.2.8-1~noble+1_arm64.deb",
                "python3-damo_3.2.8-1~noble+1_all.deb"
            ]
        );
    }

    #[test]
    fn bulk_include_eol_rejects_upload_stage() {
        let dir = setup();
        let opts = Options {
            branches: vec![], // bulk
            stages: Stages {
                upload: true,
                ..Stages::default()
            },
            nowait: false,
            upload_target: Some("ppa:me/x".to_string()),
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: true,
            include_eol: true,
        };
        assert!(run(&ui_dry(), dir.path(), &opts).is_err());
    }

    #[test]
    fn select_ppa_branches_picks_ubuntu_codenames_newest_first() {
        // A mix like archlinux-keyring: Ubuntu codenames (bare and
        // namespaced), Debian suites, plumbing, and the source.
        let all: Vec<String> = [
            "debian/unstable",
            "master",
            "upstream",
            "pristine-tar",
            "debian/trixie",
            "bookworm-backports",
            "jammy",           // Ubuntu LTS, supported
            "oracular",        // Ubuntu, EOL
            "ubuntu/questing", // Ubuntu, supported (namespaced)
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // release order, oldest first (as `ubuntu-distro-info --all` gives).
        let order: Vec<String> = ["jammy", "noble", "oracular", "questing"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let supported = ["jammy", "noble", "questing"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let selected = select_ppa_branches(&all, &order, &supported);
        // Only Ubuntu-codename branches, newest release first, with EOL
        // flags; Debian suites / plumbing excluded by the codename filter.
        // A checked-out PPA branch is NOT dropped here (that's the bug
        // fix): the merge-source refusal lives in resolve_bulk_targets.
        assert_eq!(
            selected,
            vec![
                ("ubuntu/questing".to_string(), false),
                ("oracular".to_string(), true),
                ("jammy".to_string(), false),
            ]
        );
    }

    #[test]
    fn merge_rejects_target_equal_to_source() {
        let dir = setup();
        let opts = Options {
            branches: vec!["debian/unstable".to_string()],
            stages: MERGE,
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
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
            nowait: false,
            upload_target: None,
            source: None,
            chroot_refresh: ChrootRefresh::Auto,
            urgency: "medium".to_string(),
            assume_yes: false,
            include_eol: false,
        };
        run(&ui_dry(), dir.path(), &opts).unwrap();
    }
}
