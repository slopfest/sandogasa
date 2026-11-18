// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `promote`: move a build to its collection's next stage, from
//! `-candidate` to `-testing`, from `-testing` to `-release`.
//!
//! By hand that is `cbs tag-build <tag> <nvr>` with the tag spelled out
//! and the NVR looked up, and for a release, a `cbs untag-build` from
//! `-testing` that tends to be forgotten, leaving the build in both. The
//! changes come from sandogasa-cbs (`packages::promote_changes`), where
//! hs-relmon's `review` can share them. Like every write to CBS, it is
//! confirmed at a terminal and cannot be scripted past.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use sandogasa_cbs::notify::Event;
use sandogasa_cbs::packages::{furthest_stage, latest, promote_changes, tags_of};
use sandogasa_cbs::tags::{CbsCli, TagChange, TagHub};
use sandogasa_cbs::{Collection, PROFILE, Stage};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::resolve;

#[derive(clap::Args)]
pub struct PromoteArgs {
    /// The build to promote (default: the package's newest in each of
    /// the branch's collections)
    pub nvr: Option<String>,

    /// The branch whose collections to promote in (default: the one
    /// checked out)
    #[arg(long, value_name = "BRANCH", help_heading = "Selection")]
    pub branch: Option<String>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Selection")]
    pub repo: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Selection")]
    pub variants: crate::add_pkg::VariantArgs,

    /// The stage to promote to (default: the one after where the
    /// build is)
    #[arg(long, value_enum, help_heading = "Stages")]
    pub to: Option<To>,

    /// Show each step and its command without running anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// The stages a build can be promoted to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum To {
    Testing,
    Release,
}

impl From<To> for Stage {
    fn from(t: To) -> Stage {
        match t {
            To::Testing => Stage::Testing,
            To::Release => Stage::Release,
        }
    }
}

/// Where to promote a build that has got as far as `at`: `to` when
/// given, else the next stage. A released build still in `-testing`
/// (`in_testing`) is promoted to `-release` once more, which finishes
/// the untag left undone. An error when there is nowhere to go.
pub fn destination(
    at: Stage,
    to: Option<To>,
    in_testing: bool,
    nvr: &str,
) -> Result<Stage, String> {
    if at == Stage::Release && in_testing && to.is_none_or(|t| t == To::Release) {
        return Ok(Stage::Release);
    }
    let to = match to {
        Some(t) => Stage::from(t),
        None => at
            .next()
            .ok_or_else(|| format!("{nvr} is released already"))?,
    };
    if to <= at {
        return Err(format!(
            "{nvr} is in -{} already, past or at -{}",
            at.as_str(),
            to.as_str()
        ));
    }
    Ok(to)
}

pub fn run(args: &PromoteArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &PromoteArgs) -> Result<(), Box<dyn std::error::Error>> {
    let ui = Ui {
        explain: args.explain,
        dry_run: args.dry_run,
        quiet: false,
    };
    let repo = args.repo.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut tools = vec![("koji", "sudo dnf install koji", Some("--help"))];
    if !args.dry_run {
        tools.push(("cbs", "sudo dnf install centos-packager", Some("--help")));
    }
    sandogasa_cli::require_tools(&tools)?;
    let checkout = resolve(&repo, args.branch.as_deref(), None, Some(&args.variants))?;
    let package = &checkout.package;

    // Which build, where it is, and what promoting it takes, per
    // collection. Reading changes nothing, so a dry run does it too.
    let mut steps: Vec<(String, Stage, Vec<TagChange>)> = Vec::new();
    // Where each build goes, to find the older builds it replaces there.
    let mut moves: Vec<(&Collection, String, Stage)> = Vec::new();
    // The collections a named build turned out not to be in: fine for
    // some, as a build is made for one variant, but not for all.
    let mut lacking: Vec<String> = Vec::new();
    for collection in &checkout.collections {
        let candidate = collection.tag(Stage::Candidate);
        ui.step(&format!(
            "Find {}'s build in {} and the tags it is in",
            args.nvr.as_deref().unwrap_or(package),
            collection.prefix()
        ));
        let nvr = match &args.nvr {
            Some(n) => n.clone(),
            None => {
                ui.show_command(
                    &[
                        "koji",
                        "--profile",
                        "cbs",
                        "list-tagged",
                        "--latest",
                        &candidate,
                        package,
                    ]
                    .map(String::from),
                );
                match latest(&candidate, package) {
                    Some(n) => n,
                    None => {
                        println!("{package} has no build in {candidate} yet.");
                        continue;
                    }
                }
            }
        };
        ui.show_command(
            &["koji", "--profile", "cbs", "list-tags", "--build", &nvr].map(String::from),
        );
        let tags = tags_of(&nvr)?;
        let Some(at) = furthest_stage(collection, &tags) else {
            if args.nvr.is_some() && checkout.collections.len() > 1 {
                lacking.push(collection.prefix());
                continue;
            }
            return Err(format!("{nvr} is in none of {}'s tags", collection.prefix()).into());
        };
        let in_testing = tags.contains(&collection.tag(Stage::Testing));
        let to = match destination(at, args.to, in_testing, &nvr) {
            Ok(to) => to,
            Err(e) if args.to.is_none() => {
                println!("{e}; nothing to promote.");
                continue;
            }
            Err(e) => return Err(e.into()),
        };
        steps.push((
            nvr.clone(),
            to,
            promote_changes(collection, &nvr, to, &tags),
        ));
        moves.push((collection, nvr, to));
    }
    if let Some(nvr) = &args.nvr
        && lacking.len() == checkout.collections.len()
    {
        return Err(format!("{nvr} is in none of {}'s tags", lacking.join(", ")).into());
    }
    if steps.is_empty() {
        return Ok(());
    }
    let replaced = superseded(&ui, package, &moves);
    // The cheap checks first: the CBS login and a terminal to confirm
    // at, before any GitLab request.
    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("promoting in CBS is confirmed at a terminal; run it in one".into());
        }
    }
    // Then what the builds going to -testing drop, which takes the hub
    // and fedrq some seconds.
    let (found, unchecked, skipped) = check_moves(
        &ui,
        package,
        &moves,
        sandogasa_kojihub::config::hub_url("cbs").as_deref(),
        &FedrqLookup::new(),
    );
    if args.dry_run {
        // What the check found, before the steps it would lead to, as a
        // real run's plan shows it first.
        print!(
            "{}",
            crate::plan::checks(
                &found,
                unchecked.as_deref(),
                sandogasa_cli::style::use_color(sandogasa_cli::style::ColorChoice::Auto)
            )
        );
    }
    // The tracking issue is read next, so what it will be told is shown
    // with the promotion and confirmed with it.
    let base = crate::config::gitlab_base();
    let token = crate::config::gitlab_token();
    if sandogasa_cbs::notify::covers(checkout.sig) {
        ui.step("Find the tracking issue to update, through GitLab's API");
    }
    let mut notices = sandogasa_cbs::notify::plan(
        &base,
        token.as_deref(),
        &sandogasa_cbs::notify::Branch {
            sig: checkout.sig,
            package,
            branch: &checkout.branch,
            checkout: &crate::checkout::GitCheckout { repo: &repo },
            dry_run: args.dry_run,
            tool: "cbspkg",
        },
        &steps,
        &sandogasa_cbs::packages::build_commits,
    );
    // What the check found goes on the issue too, with the build.
    add_checks(
        &mut notices,
        &found,
        &skipped,
        unchecked.as_deref(),
        &checked_by(&std::env::args().collect::<Vec<_>>()),
    );

    if !args.dry_run {
        // The plan the confirmation asks about, on standard error with
        // the question; set apart from the login check and the steps
        // above.
        let color = sandogasa_cli::style::stderr_color();
        eprint!(
            "\n{}",
            crate::plan::promote(
                &steps,
                &found,
                unchecked.as_deref(),
                &notices,
                &replaced,
                token.is_some(),
                color
            )
        );
        // Something elsewhere requiring what a build drops, or a dropped
        // capability whose users could not be looked up, makes "no" the
        // answer an Enter gives.
        let breaks = found.iter().any(|c| c.breaks() || c.uncertain());
        if !sandogasa_cli::confirm("Promote?", !breaks)? {
            eprintln!("Nothing changed.");
            return Ok(());
        }
    }
    // What got where, so a failure partway still tells the issue of the
    // builds that moved: a rerun would find them promoted, with nothing
    // left to do and so nothing to tell.
    let mut done: Vec<(String, Stage)> = Vec::new();
    let hub = CbsCli {
        ui: &ui,
        cwd: &repo,
    };
    let moved = promote_all(&hub, &steps, args.dry_run, &mut done);
    // Only once the builds replacing them are in place.
    if moved.is_ok() {
        untag_superseded(&hub, &replaced, args.dry_run)?;
    }
    if args.dry_run {
        // After the steps, where a real run tells the issue.
        for notice in &notices {
            ui.step("Comment on the tracking issue, through GitLab's API");
            print!(
                "{}",
                sandogasa_cbs::notify::describe_styled(
                    notice,
                    token.is_some(),
                    sandogasa_cli::style::use_color(sandogasa_cli::style::ColorChoice::Auto),
                )
            );
        }
    } else {
        let done: Vec<(String, Event)> = done
            .into_iter()
            .map(|(nvr, stage)| (nvr, Event::Reached(stage)))
            .collect();
        sandogasa_cbs::notify::post(
            &base,
            token.as_deref(),
            &notices,
            &done,
            &sandogasa_cbs::notify::cbs_latest,
        );
        if moved.is_ok()
            && let Some((what, command, by_hand)) = release_next(args, &steps)
        {
            eprintln!();
            crate::next::print_next_step(&what, &command, &by_hand);
        }
    }
    moved
}

/// What to run, once tested, to release the builds `steps` took to
/// `-testing`: one `cbspkg promote` as the build's own suggests, and by
/// hand the tag into `-release` and the untag from `-testing` per build.
/// `None` when no build went to `-testing`.
fn release_next(
    args: &PromoteArgs,
    steps: &[(String, Stage, Vec<TagChange>)],
) -> Option<(String, Vec<String>, Vec<Vec<String>>)> {
    let tested: Vec<(&str, &str)> = steps
        .iter()
        .filter(|(_, to, _)| *to == Stage::Testing)
        .filter_map(|(nvr, _, changes)| {
            changes.iter().find_map(|c| match c {
                TagChange::Tag { tag, .. } => Some((nvr.as_str(), tag.as_str())),
                TagChange::Untag { .. } => None,
            })
        })
        .collect();
    if tested.is_empty() {
        return None;
    }
    let nvrs: Vec<&str> = tested.iter().map(|(nvr, _)| *nvr).collect();
    let mut by_hand = Vec::new();
    for (nvr, testing) in &tested {
        let release = testing
            .strip_suffix("-testing")
            .map_or_else(|| testing.to_string(), |base| format!("{base}-release"));
        by_hand.push(TagChange::tag(&release, nvr).argv());
        by_hand.push(TagChange::untag(testing, nvr).argv());
    }
    // A build named on the command line is the one to release; without
    // one, the same selection finds each collection's newest in testing.
    let nvr_given: Vec<&str> = args.nvr.iter().map(String::as_str).collect();
    let command = crate::next::promote_command(
        if nvr_given.is_empty() {
            &nvrs
        } else {
            &nvr_given
        },
        args.repo.as_deref(),
        args.branch.as_deref(),
        &args.variants,
    );
    let verb = if nvrs.len() == 1 { "has" } else { "have" };
    let what = format!(
        "once {} {verb} been tested, to release {}",
        crate::next::listed(&nvrs),
        if nvrs.len() == 1 { "it" } else { "them" }
    );
    Some((what, command, by_hand))
}

/// The repos a check looks in, through fedrq: absent, every question is
/// an error saying so, and the check reports them as not looked in.
struct FedrqLookup {
    installed: bool,
}

impl FedrqLookup {
    fn new() -> Self {
        let installed =
            sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])
                .is_ok();
        Self { installed }
    }

    fn fedrq(&self, branch: &str) -> Result<sandogasa_fedrq::Fedrq, String> {
        if !self.installed {
            return Err("fedrq is not installed (sudo dnf install fedrq)".into());
        }
        Ok(sandogasa_fedrq::Fedrq {
            branch: Some(branch.to_string()),
            repo: None,
        })
    }
}

impl sandogasa_cbs::check::Lookup for FedrqLookup {
    fn providers(
        &self,
        branch: &str,
        capabilities: &[String],
    ) -> Result<Vec<(String, Vec<String>)>, String> {
        Ok(self
            .fedrq(branch)?
            .providers_info(capabilities)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|p| (p.source_name.unwrap_or(p.name), p.provides))
            .collect())
    }

    fn users(&self, branch: &str, capabilities: &[String]) -> Result<Vec<String>, String> {
        Ok(self
            .fedrq(branch)?
            .whatrequires_notsrc(capabilities)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(_, source)| source)
            .collect())
    }
}

/// Each build's checks, in full, as part of its tracking-issue comment,
/// with `by` (the tool, its version and the command) at the end, as
/// ebranch signs the check it posts to Bodhi; a build moving into
/// several collections (Stream and RHEL) has one check per collection.
/// A build `skipped` says why it was not checked, and when nothing
/// could be checked, `unchecked` says why, so a comment without a check
/// does not read as one that found nothing.
fn add_checks(
    notices: &mut [sandogasa_cbs::notify::Notice],
    found: &[sandogasa_cbs::check::Check],
    skipped: &[(String, String)],
    unchecked: Option<&str>,
    by: &str,
) {
    use sandogasa_cbs::check::one_line;
    for notice in notices {
        let mut parts: Vec<String> = found
            .iter()
            .filter(|c| c.nvr == notice.nvr)
            .map(|c| c.markdown())
            .collect();
        parts.extend(
            skipped
                .iter()
                .filter(|(nvr, _)| *nvr == notice.nvr)
                .map(|(_, why)| format!("Not checked for what it drops: `{}`.", one_line(why))),
        );
        if parts.is_empty() {
            if let Some(why) = unchecked {
                notice.add_paragraph(&format!(
                    "Not checked for what it drops: `{}`.",
                    one_line(why)
                ));
            }
            continue;
        }
        parts.push(by.to_string());
        notice.add_paragraph(&parts.join("\n\n"));
    }
}

/// Who checked: the tool, its version and the command, for the end of
/// a posted check. The comment is public, so the checkout's path
/// (`-C`, `--repo`) is left out and any other path cut to its last
/// part; each argument is quoted as a shell would need it.
fn checked_by(args: &[String]) -> String {
    let mut command = vec!["cbspkg".to_string()];
    let mut rest = args.iter().skip(1);
    while let Some(a) = rest.next() {
        if a == "-C" || a == "--repo" {
            rest.next();
            continue;
        }
        if a.starts_with("--repo=") || (a.starts_with("-C") && a.len() > 2) {
            continue;
        }
        let a = if a.contains('/') {
            a.rsplit('/').find(|p| !p.is_empty()).unwrap_or(a)
        } else {
            a
        };
        command.push(shell_quote(a));
    }
    format!(
        "<sub>Checked by cbspkg {}: `{}`</sub>",
        env!("CARGO_PKG_VERSION"),
        sandogasa_cbs::check::one_line(&command.join(" "))
    )
}

/// `arg` as a POSIX shell would need it: as is when it has only safe
/// characters, otherwise in single quotes.
fn shell_quote(arg: &str) -> String {
    let safe = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.,:=+@%^~".contains(c));
    if safe {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// What [`check_moves`] found: the checks; why nothing could be
/// checked, when so; and each build left unchecked, with why.
type Checked = (
    Vec<sandogasa_cbs::check::Check>,
    Option<String>,
    Vec<(String, String)>,
);

/// The checks of the builds that `moves` take to `-testing` or
/// `-release` (#157), each against the newest build of `package` in its
/// collection's `-release`, which is what everyone using the SIG's
/// repos has; read from the CBS hub at `hub_url`
/// (every collection's `-release`, and then every build's Provides, in
/// one `multiCall` each), with the users of what a build drops looked
/// up through `lookup` in stock CentOS Stream and EPEL. Also, when
/// nothing could be checked, why: no hub configured, or one that cannot
/// be read. A collection whose tag cannot be read is not checked,
/// rather than compared with an older build.
fn check_moves(
    ui: &Ui,
    package: &str,
    moves: &[(&Collection, String, Stage)],
    hub_url: Option<&str>,
    lookup: &dyn sandogasa_cbs::check::Lookup,
) -> Checked {
    use sandogasa_cbs::check::{compare, look_up, provides_of, user_repos};
    let to_testing: Vec<&(&Collection, String, Stage)> = moves
        .iter()
        .filter(|(_, _, to)| matches!(to, Stage::Testing | Stage::Release))
        .collect();
    if to_testing.is_empty() {
        return (Vec::new(), None, Vec::new());
    }
    let Some(hub_url) = hub_url else {
        return (
            Vec::new(),
            Some("no CBS hub in koji's configuration (profile cbs)".into()),
            Vec::new(),
        );
    };
    // One line says what is going on: the step heading when steps are
    // narrated, a plain status line otherwise.
    if !(ui.explain || ui.dry_run) {
        eprintln!("Checking what the builds of {package} drop that other packages require…");
    }
    ui.step(&format!(
        "Check what the builds of {package} drop that other packages require"
    ));
    let hub = sandogasa_kojihub::HubClient::new(hub_url);
    let tags: Vec<String> = to_testing
        .iter()
        .map(|(c, _, _)| c.tag(Stage::Release))
        .collect();
    let latest = match hub.latest_in_tags(&tags, package) {
        Ok(l) => l,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("cannot read {package}'s builds from CBS ({e})")),
                Vec::new(),
            );
        }
    };
    // Per move, the build it is compared with: the newest in the
    // collection's -release, which is what everyone using the SIG's
    // repos has, whichever stage the build goes to. Err, with the
    // tag, when it could not be read.
    let against: Vec<Result<Option<String>, String>> = latest
        .iter()
        .zip(&to_testing)
        .map(|(latest, (c, _, _))| match latest {
            Err(e) => Err(format!("{}: {e}", c.tag(Stage::Release))),
            Ok(b) => Ok(b.as_ref().map(|b| b.nvr.clone())),
        })
        .collect();
    let mut nvrs: Vec<String> = to_testing.iter().map(|(_, nvr, _)| nvr.clone()).collect();
    nvrs.extend(against.iter().filter_map(|a| a.clone().ok().flatten()));
    let provides = match provides_of(&hub, &nvrs) {
        Ok(p) => p,
        Err(e) => {
            return (
                Vec::new(),
                Some(format!("cannot read the builds' Provides from CBS ({e})")),
                Vec::new(),
            );
        }
    };
    let by_nvr: std::collections::BTreeMap<
        &str,
        &Result<std::collections::BTreeSet<String>, String>,
    > = nvrs
        .iter()
        .map(String::as_str)
        .zip(provides.iter())
        .collect();
    let mut found = Vec::new();
    let mut repos = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut skip = |nvr: &str, why: String| {
        eprintln!("warning: {why}; {nvr} not checked");
        skipped.push((nvr.to_string(), why));
    };
    for ((collection, nvr, _), against) in to_testing.iter().zip(&against) {
        let new = match by_nvr.get(nvr.as_str()) {
            Some(Ok(p)) => p,
            Some(Err(e)) => {
                skip(nvr, format!("cannot read {nvr}'s Provides ({e})"));
                continue;
            }
            None => continue,
        };
        let old = match against {
            Err(e) => {
                skip(nvr, format!("cannot read {e}"));
                continue;
            }
            Ok(None) => None,
            // Already released (a re-promotion finishing its untag from
            // -testing): nothing to compare it with but itself.
            Ok(Some(a)) if a == nvr => continue,
            Ok(Some(a)) => match by_nvr.get(a.as_str()) {
                Some(Ok(p)) => Some((a.as_str(), p)),
                _ => {
                    skip(nvr, format!("cannot read {a}'s Provides"));
                    continue;
                }
            },
        };
        found.push(compare(nvr, old, new));
        repos.push(user_repos(collection.major));
    }
    look_up(&mut found, &repos, &[package], lookup);
    (found, None, skipped)
}

/// The commands untagging the builds of `package` that each move in
/// `moves` (collection, build, stage) replaces: the older builds in the
/// stage it moves into, and for a release in `-testing` too, so that
/// superseded builds do not pile up there. `-candidate` keeps the build
/// history. The tags are read side by side; one that cannot be read is
/// a warning, and its builds are left alone.
fn superseded(ui: &Ui, package: &str, moves: &[(&Collection, String, Stage)]) -> Vec<TagChange> {
    let mut wanted: Vec<(String, &str)> = Vec::new();
    for (collection, nvr, to) in moves {
        let stages: &[Stage] = match to {
            Stage::Release => &[Stage::Release, Stage::Testing],
            _ => &[Stage::Testing],
        };
        for stage in stages {
            wanted.push((collection.tag(*stage), nvr.as_str()));
        }
    }
    ui.step(&format!(
        "Find older builds of {package} the promoted ones replace"
    ));
    for (tag, _) in &wanted {
        ui.show_command(
            &["koji", "--profile", "cbs", "list-tagged", tag, package].map(String::from),
        );
    }
    let listed: Vec<Result<Vec<String>, String>> = std::thread::scope(|s| {
        let handles: Vec<_> = wanted
            .iter()
            .map(|(tag, _)| s.spawn(move || tagged(tag, package)))
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or_else(|_| Err("koji panicked".into())))
            .collect()
    });
    let mut changes = Vec::new();
    for ((tag, nvr), builds) in wanted.iter().zip(listed) {
        let builds = match builds {
            Ok(b) => b,
            Err(e) => {
                eprintln!("warning: {e}; older builds there are left tagged");
                continue;
            }
        };
        for old in older_than(&builds, nvr, package) {
            changes.push(TagChange::untag(tag, &old));
        }
    }
    changes
}

/// The builds among `builds` of `package` older than `nvr`: the name
/// is the NVR less its last two fields, so `blktrace-tools` is not
/// `blktrace`.
pub fn older_than(builds: &[String], nvr: &str, package: &str) -> Vec<String> {
    let split = |nvr: &str| -> Option<(String, String)> {
        let mut parts = nvr.rsplitn(3, '-');
        let (release, version, name) = (parts.next()?, parts.next()?, parts.next()?);
        Some((name.to_string(), format!("{version}-{release}")))
    };
    let Some((name, vr)) = split(nvr).filter(|(n, _)| n == package) else {
        return Vec::new();
    };
    builds
        .iter()
        .filter(|b| {
            split(b).is_some_and(|(n, other)| {
                n == name
                    && sandogasa_rpmvercmp::compare_evr(&other, &vr) == std::cmp::Ordering::Less
            })
        })
        .cloned()
        .collect()
}

/// Every build of `package` in `tag`.
fn tagged(tag: &str, package: &str) -> Result<Vec<String>, String> {
    let out = std::process::Command::new("koji")
        .args(["--profile", PROFILE, "list-tagged", "--quiet", tag, package])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "koji list-tagged {tag} {package}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

/// Offer to untag the superseded builds (`changes`), after the
/// promotion, confirmed on their own at a terminal; a dry run shows the
/// commands. A failed untag is a warning: the promotion is done.
fn untag_superseded(
    hub: &dyn TagHub,
    changes: &[TagChange],
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if changes.is_empty() {
        return Ok(());
    }
    if !dry_run && !sandogasa_cli::confirm("Untag the older builds they replace?", true)? {
        eprintln!("Older builds left tagged.");
        return Ok(());
    }
    for change in changes {
        let why = format!(
            "Untag {} from {}, as a newer build replaces it",
            change.nvr(),
            change.tag_name()
        );
        if let Err(e) = hub.apply(change, &why) {
            eprintln!(
                "warning: could not untag {} from {} ({e}); run `{}` by hand",
                change.nvr(),
                change.tag_name(),
                sandogasa_cli::explain::command_line(&change.argv())
            );
        }
    }
    Ok(())
}

/// Make each step's tag changes, recording in `done` each build that
/// got through all of its own.
fn promote_all(
    hub: &dyn TagHub,
    steps: &[(String, Stage, Vec<TagChange>)],
    dry_run: bool,
    done: &mut Vec<(String, Stage)>,
) -> Result<(), Box<dyn std::error::Error>> {
    for (nvr, to, changes) in steps {
        for change in changes {
            let why = match change {
                TagChange::Untag { tag, .. } => {
                    format!("Untag {nvr} from {tag}, as it is released")
                }
                TagChange::Tag { tag, .. } => format!("Tag {nvr} into {tag}"),
            };
            hub.apply(change, &why)?;
        }
        done.push((nvr.clone(), *to));
        if !dry_run && *to == Stage::Release {
            println!("{nvr} is released.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_support::Fixture;

    const NVR: &str = "rsync-3.2.7-5~proposed.el10";
    const CANDIDATE: &str = "proposed_updates10s-packages-main-candidate";
    const TESTING: &str = "proposed_updates10s-packages-main-testing";
    const RELEASE: &str = "proposed_updates10s-packages-main-release";

    #[test]
    fn the_release_is_suggested_once_builds_reach_testing() {
        let step = |nvr: &str, tag: &str| {
            (
                nvr.to_string(),
                Stage::Testing,
                vec![TagChange::tag(tag, nvr)],
            )
        };
        let mut args = PromoteArgs {
            nvr: None,
            branch: None,
            repo: None,
            variants: Default::default(),
            to: None,
            dry_run: false,
            explain: false,
        };
        let one = [step("r-1-1.hs.el10", "hyperscale10s-packages-main-testing")];
        let (what, command, by_hand) = release_next(&args, &one).unwrap();
        assert_eq!(what, "once r-1-1.hs.el10 has been tested, to release it");
        assert_eq!(command.join(" "), "cbspkg promote r-1-1.hs.el10");
        assert_eq!(
            by_hand.iter().map(|a| a.join(" ")).collect::<Vec<_>>(),
            [
                "cbs tag-build hyperscale10s-packages-main-release r-1-1.hs.el10",
                "cbs untag-build hyperscale10s-packages-main-testing r-1-1.hs.el10",
            ]
        );
        args.variants.variant = vec!["main".into(), "facebook".into()];
        let two = [
            one[0].clone(),
            step(
                "r-1-1.hs+fb.el10",
                "hyperscale10s-packages-facebook-testing",
            ),
        ];
        let (what, command, by_hand) = release_next(&args, &two).unwrap();
        assert_eq!(
            what,
            "once r-1-1.hs.el10 and r-1-1.hs+fb.el10 have been tested, to release them"
        );
        assert_eq!(
            command.join(" "),
            "cbspkg promote --variant main --variant facebook"
        );
        assert_eq!(by_hand.len(), 4);
        // A release has no next step.
        let released = [(
            "r-1-1.hs.el10".to_string(),
            Stage::Release,
            vec![TagChange::tag("t-release", "r-1-1.hs.el10")],
        )];
        assert!(release_next(&args, &released).is_none());
    }

    #[test]
    fn each_builds_check_goes_into_its_own_comment() {
        use sandogasa_cbs::notify::{Event, Notice};
        let notice = |nvr: &str| Notice {
            nvr: nvr.into(),
            event: Event::Reached(Stage::Release),
            path: "p".into(),
            iid: 1,
            issue_url: "u".into(),
            note: format!("`{nvr}` is released.\n\n<!-- sandogasa: {nvr} reached release -->"),
            label: None,
            missing_label: None,
            unlabel: None,
            at_least: None,
            closing: None,
            older: false,
            tool: "cbspkg",
        };
        let mut notices = vec![notice("w-2-1.el9"), notice("w-2-1.el10")];
        let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        let checked = sandogasa_cbs::check::compare(
            "w-2-1.el9",
            Some(("w-1-1.el9", &set(&["w = 1-1"]))),
            &set(&["w = 2-1"]),
        );
        add_checks(
            &mut notices,
            std::slice::from_ref(&checked),
            &[],
            None,
            "BY",
        );
        assert_eq!(
            notices[0].note,
            format!(
                "`w-2-1.el9` is released.\n\n{}\n\nBY\n\n\
                 <!-- sandogasa: w-2-1.el9 reached release -->",
                checked.markdown()
            )
        );
        // A build without a check keeps its comment as it was.
        assert_eq!(
            notices[1].note,
            "`w-2-1.el10` is released.\n\n<!-- sandogasa: w-2-1.el10 reached release -->"
        );
        // Nothing could be checked: each comment says why.
        let mut notices = vec![notice("w-2-1.el9")];
        add_checks(&mut notices, &[], &[], Some("no CBS hub\nconfigured"), "BY");
        assert!(
            notices[0]
                .note
                .contains("\n\nNot checked for what it drops: `no CBS hub configured`.\n\n"),
            "{}",
            notices[0].note
        );
        // One build skipped, the other checked in two collections.
        let mut notices = vec![notice("w-2-1.el9"), notice("w-2-1.el10")];
        let mut rhel = checked.clone();
        rhel.against = Some("w-1-2.el9".into());
        add_checks(
            &mut notices,
            &[checked.clone(), rhel.clone()],
            &[(
                "w-2-1.el10".into(),
                "cannot read w-2-1.el10's Provides".into(),
            )],
            None,
            "BY",
        );
        assert!(notices[0].note.contains(&format!(
            "{}\n\n{}\n\nBY",
            checked.markdown(),
            rhel.markdown()
        )));
        assert!(
            notices[1].note.contains(
                "Not checked for what it drops: `cannot read w-2-1.el10's Provides`.\n\nBY"
            )
        );
        // The checkout's path stays private; arguments are quoted.
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let version = env!("CARGO_PKG_VERSION");
        assert_eq!(
            checked_by(&args(&[
                "/usr/bin/cbspkg",
                "promote",
                "-C",
                "/home/u/src/perf",
                "--repo=/home/u/x",
                "-C/home/u/y",
                "--branch",
                "c9s sig",
                "--to",
                "release"
            ])),
            format!(
                "<sub>Checked by cbspkg {version}: \
                 `cbspkg promote --branch 'c9s sig' --to release`</sub>"
            )
        );
        assert_eq!(
            checked_by(&args(&["cbspkg", "promote", "./it's/r-1.src.rpm"])),
            format!("<sub>Checked by cbspkg {version}: `cbspkg promote r-1.src.rpm`</sub>")
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote(""), "''");
    }

    #[test]
    fn builds_are_checked_against_what_they_replace() {
        let collections = sandogasa_cbs::hyperscale_collections(
            10,
            &["main".to_string(), "facebook".to_string()],
            &[],
        )
        .unwrap();
        let (main, fb) = (&collections[0], &collections[1]);
        let value = |v: &str| format!("<value><array><data>{v}</data></array></value>");
        let answer = |results: &[String]| {
            format!(
                "<?xml version='1.0'?><methodResponse><params><param>\
                 <value><array><data>{}</data></array></value>\
                 </param></params></methodResponse>",
                results.concat()
            )
        };
        let build = |nvr: &str| {
            value(&format!(
                "<value><array><data><value><struct><member><name>nvr</name>\
                 <value><string>{nvr}</string></value></member></struct></value>\
                 </data></array></value>"
            ))
        };
        let none = value("<value><array><data></data></array></value>");
        let rpm = |name: &str, vr: &str| {
            let (v, r) = vr.split_once('-').unwrap();
            format!(
                "<value><struct><member><name>name</name><value><string>{name}</string></value></member>\
                 <member><name>version</name><value><string>{v}</string></value></member>\
                 <member><name>release</name><value><string>{r}</string></value></member>\
                 <member><name>arch</name><value><string>x86_64</string></value></member></struct></value>"
            )
        };
        let provide = |name: &str| {
            value(&format!(
                "<value><array><data><value><struct><member><name>name</name>\
                 <value><string>{name}</string></value></member>\
                 <member><name>type</name><value><int>1</int></value></member></struct></value>\
                 </data></array></value>"
            ))
        };
        let mut server = mockito::Server::new();
        // main has 0-1 in -release (1-1 in -testing is not asked
        // about); facebook has nothing released. Asked per move: main,
        // facebook, main.
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("getLatestBuilds".into()))
            .with_body(answer(&[
                build("r-0-1.hs.el10"),
                none.clone(),
                build("r-0-1.hs.el10"),
            ]))
            .expect(1)
            .create();
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("listBuildRPMs".into()))
            .with_body(answer(&[
                value(&format!(
                    "<value><array><data>{}</data></array></value>",
                    rpm("r", "2-1.hs.el10")
                )),
                value(&format!(
                    "<value><array><data>{}</data></array></value>",
                    rpm("r", "2-1.hs+fb.el10")
                )),
                value(&format!(
                    "<value><array><data>{}</data></array></value>",
                    rpm("r", "1-1.hs.el10")
                )),
                value(&format!(
                    "<value><array><data>{}</data></array></value>",
                    rpm("r", "0-1.hs.el10")
                )),
                value(&format!(
                    "<value><array><data>{}</data></array></value>",
                    rpm("r", "0-1.hs.el10")
                )),
            ]))
            .expect(1)
            .create();
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("getRPMDeps".into()))
            .with_body(answer(&[
                provide("r"),
                provide("r"),
                provide("libr.so.1()(64bit)"),
                provide("libr.so.1()(64bit)"),
                provide("libr.so.1()(64bit)"),
            ]))
            .expect(1)
            .create();
        struct Repos;
        impl sandogasa_cbs::check::Lookup for Repos {
            fn providers(
                &self,
                _: &str,
                caps: &[String],
            ) -> Result<Vec<(String, Vec<String>)>, String> {
                assert_eq!(caps, ["libr.so.1()(64bit)"]);
                Ok(vec![])
            }
            fn users(&self, branch: &str, caps: &[String]) -> Result<Vec<String>, String> {
                assert_eq!(caps, ["libr.so.1()(64bit)"]);
                Ok(if branch == "c10s" {
                    vec!["user".into()]
                } else {
                    vec![]
                })
            }
        }
        let ui = Ui {
            explain: false,
            dry_run: true,
            quiet: true,
        };
        let moves = [
            (main, "r-2-1.hs.el10".to_string(), Stage::Testing),
            (fb, "r-2-1.hs+fb.el10".to_string(), Stage::Testing),
            (main, "r-1-1.hs.el10".to_string(), Stage::Release),
        ];
        let (found, unchecked, _) = check_moves(&ui, "r", &moves, Some(&server.url()), &Repos);
        assert_eq!(unchecked, None);
        assert_eq!(found.len(), 3);
        // Going to -testing, against -release too.
        assert_eq!(found[0].against.as_deref(), Some("r-0-1.hs.el10"));
        assert_eq!(found[0].dropped(), ["libr.so.1()(64bit)"]);
        assert!(found[0].breaks());
        assert_eq!(found[1].against, None);
        assert_eq!(found[2].nvr, "r-1-1.hs.el10");
        assert_eq!(found[2].against.as_deref(), Some("r-0-1.hs.el10"));
        assert!(found[2].dropped().is_empty());
        let shown = crate::plan::checks(&found, None, false);
        assert!(
            shown.contains("required in CentOS Stream 10 by user"),
            "{shown}"
        );
        assert!(
            shown.contains("nothing in -release to compare it with"),
            "{shown}"
        );
        // No hub configured, or one that cannot be read: nothing
        // checked, and the plan says why.
        let (found, why, _) = check_moves(&ui, "r", &moves, None, &Repos);
        assert!(found.is_empty());
        assert!(why.as_deref().unwrap().contains("no CBS hub"));
        let (found, why, _) = check_moves(&ui, "r", &moves, Some("http://127.0.0.1:1"), &Repos);
        assert!(found.is_empty() && why.is_some());
        assert!(crate::plan::checks(&found, why.as_deref(), false).starts_with("Not checked"));
        // Nothing to move: nothing asked.
        let (found, why, _) =
            check_moves(&ui, "r", &moves[..0], Some("http://127.0.0.1:1"), &Repos);
        assert!(found.is_empty() && why.is_none());
    }

    #[test]
    fn a_release_tag_that_cannot_be_read_leaves_its_build_unchecked() {
        let collections =
            sandogasa_cbs::hyperscale_collections(10, &["main".to_string()], &[]).unwrap();
        let fault = "<value><struct>\
             <member><name>faultCode</name><value><int>1000</int></value></member>\
             <member><name>faultString</name><value><string>No such tag</string></value></member>\
             </struct></value>";
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("getLatestBuilds".into()))
            .with_body(format!(
                "<?xml version='1.0'?><methodResponse><params><param>\
                 <value><array><data>{fault}</data></array></value>\
                 </param></params></methodResponse>"
            ))
            .create();
        server
            .mock("POST", "/")
            .match_body(mockito::Matcher::Regex("listBuildRPMs".into()))
            .with_body(
                "<?xml version='1.0'?><methodResponse><params><param>\
                 <value><array><data><value><array><data><value><array><data></data></array></value>\
                 </data></array></value></data></array></value></param></params></methodResponse>",
            )
            .create();
        struct Nothing;
        impl sandogasa_cbs::check::Lookup for Nothing {
            fn providers(
                &self,
                _: &str,
                _: &[String],
            ) -> Result<Vec<(String, Vec<String>)>, String> {
                Ok(vec![])
            }
            fn users(&self, _: &str, _: &[String]) -> Result<Vec<String>, String> {
                Ok(vec![])
            }
        }
        let ui = Ui {
            explain: false,
            dry_run: true,
            quiet: true,
        };
        let moves = [(&collections[0], "r-2-1.hs.el10".to_string(), Stage::Testing)];
        // Not checked, rather than taken as the first build released.
        let (found, why, _) = check_moves(&ui, "r", &moves, Some(&server.url()), &Nothing);
        assert!(found.is_empty(), "{found:?}");
        assert_eq!(why, None);
    }

    fn dry_run(nvr: Option<&str>, to: Option<To>, f: &Fixture) -> PromoteArgs {
        PromoteArgs {
            nvr: nvr.map(str::to_string),
            branch: None,
            repo: Some(f.repo.clone()),
            variants: Default::default(),
            to,
            dry_run: true,
            explain: false,
        }
    }

    /// koji, with the newest build in -candidate in `tags`.
    fn koji(f: &Fixture, tags: &str) {
        f.fake(
            "koji",
            &[
                ("--help", ""),
                ("list-tagged", &format!("{NVR}  {CANDIDATE}  salimma")),
                ("list-tags", tags),
            ],
        );
    }

    /// A hub recording the changes asked of it, failing those whose
    /// tag is in `refuse`.
    #[derive(Default)]
    struct FakeHub {
        made: std::cell::RefCell<Vec<String>>,
        refuse: Vec<&'static str>,
    }

    impl TagHub for FakeHub {
        fn apply(&self, change: &TagChange, _why: &str) -> Result<(), Box<dyn std::error::Error>> {
            if self.refuse.contains(&change.tag_name()) {
                return Err("refused".into());
            }
            self.made.borrow_mut().push(change.argv().join(" "));
            Ok(())
        }
    }

    #[test]
    fn each_promoted_build_is_recorded_once_its_changes_are_made() {
        let steps = vec![
            (
                "a-1-1".to_string(),
                Stage::Release,
                vec![
                    TagChange::tag("rel", "a-1-1"),
                    TagChange::untag("test", "a-1-1"),
                ],
            ),
            (
                "b-1-1".to_string(),
                Stage::Testing,
                vec![TagChange::tag("test", "b-1-1")],
            ),
        ];
        let hub = FakeHub::default();
        let mut done = Vec::new();
        promote_all(&hub, &steps, true, &mut done).unwrap();
        assert_eq!(
            *hub.made.borrow(),
            [
                "cbs tag-build rel a-1-1",
                "cbs untag-build test a-1-1",
                "cbs tag-build test b-1-1"
            ]
        );
        assert_eq!(
            done,
            [
                ("a-1-1".to_string(), Stage::Release),
                ("b-1-1".to_string(), Stage::Testing)
            ]
        );
        // A refused change stops there: the build before it is
        // recorded, the one it belongs to is not.
        let hub = FakeHub {
            refuse: vec!["test"],
            ..FakeHub::default()
        };
        let mut done = Vec::new();
        let steps = [steps[1].clone(), steps[0].clone()];
        assert!(promote_all(&hub, &steps, true, &mut done).is_err());
        assert!(done.is_empty());
    }

    #[test]
    fn a_failed_untag_of_a_superseded_build_is_only_a_warning() {
        let hub = FakeHub {
            refuse: vec!["test"],
            ..FakeHub::default()
        };
        let changes = [
            TagChange::untag("test", "a-0-1"),
            TagChange::untag("rel", "a-0-1"),
        ];
        // A dry run asks nothing; the refused untag does not stop the
        // next one.
        untag_superseded(&hub, &changes, true).unwrap();
        assert_eq!(*hub.made.borrow(), ["cbs untag-build rel a-0-1"]);
        untag_superseded(&hub, &[], false).unwrap();
    }

    #[test]
    fn only_older_builds_of_the_package_are_superseded() {
        let builds: Vec<String> = [
            "blktrace-1.3.0-13~proposed.el10",
            "blktrace-1.3.0-14~proposed.el10",
            "blktrace-1.3.0-15~proposed.el10",
            "blktrace-tools-1.0-1.el10",
        ]
        .map(String::from)
        .to_vec();
        assert_eq!(
            older_than(&builds, "blktrace-1.3.0-14~proposed.el10", "blktrace"),
            ["blktrace-1.3.0-13~proposed.el10"]
        );
        assert!(older_than(&builds, "other-1-1", "blktrace").is_empty());
    }

    /// blktrace c10s: -14 released while -13 is in -testing and
    /// -release. Both copies of -13 are untagged; -candidate keeps it.
    #[test]
    #[serial_test::serial]
    fn a_release_untags_the_builds_it_replaces_from_testing_and_release() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        let old = "rsync-3.2.7-4~proposed.el10";
        f.fake(
            "koji",
            &[
                ("--help", ""),
                ("list-tags", TESTING),
                (
                    &format!("list-tagged --quiet {TESTING}"),
                    &format!("{NVR}  {TESTING}  me\n{old}  {TESTING}  me"),
                ),
                (
                    &format!("list-tagged --quiet {RELEASE}"),
                    &format!("{old}  {RELEASE}  me"),
                ),
            ],
        );
        let moves_collections = crate::add_pkg::resolve(&f.repo, None, None, None)
            .unwrap()
            .collections;
        let moves = [(&moves_collections[0], NVR.to_string(), Stage::Release)];
        let ui = Ui {
            explain: false,
            dry_run: true,
            quiet: false,
        };
        let changes = superseded(&ui, "rsync", &moves);
        let lines: Vec<String> = changes
            .iter()
            .map(|c| sandogasa_cli::explain::command_line(&c.argv()))
            .collect();
        assert_eq!(
            lines,
            [
                format!("cbs untag-build {RELEASE} {old}"),
                format!("cbs untag-build {TESTING} {old}"),
            ]
        );
        // A tag that cannot be read leaves its builds alone.
        f.fake("koji", &[("--help", ""), ("list-tagged", "!no such tag")]);
        assert!(superseded(&ui, "rsync", &moves).is_empty());
        // And the dry run shows the whole promotion.
        f.fake(
            "koji",
            &[
                ("--help", ""),
                ("list-tags", TESTING),
                (
                    &format!("list-tagged --quiet {TESTING}"),
                    &format!("{NVR}  {TESTING}  me\n{old}  {TESTING}  me"),
                ),
                ("list-tagged --quiet", ""),
            ],
        );
        run_inner(&dry_run(Some(NVR), Some(To::Release), &f)).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_promotes_the_newest_candidate_to_testing() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, CANDIDATE);
        let args = dry_run(None, None, &f);
        run_inner(&args).unwrap();
        assert_eq!(run(&args), ExitCode::SUCCESS);
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_releases_a_tested_build_and_untags_it_from_testing() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, &format!("{CANDIDATE}\n{TESTING}"));
        run_inner(&dry_run(Some(NVR), None, &f)).unwrap();
        // Straight from -candidate when asked.
        koji(&f, CANDIDATE);
        run_inner(&dry_run(Some(NVR), Some(To::Release), &f)).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn a_package_without_a_build_or_with_nowhere_to_go_is_left_alone() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake("koji", &[("--help", ""), ("list-tagged", "")]);
        run_inner(&dry_run(None, None, &f)).unwrap();
        // Released already: said, and nothing done.
        koji(&f, &format!("{CANDIDATE}\n{RELEASE}"));
        run_inner(&dry_run(None, None, &f)).unwrap();
    }

    #[test]
    #[serial_test::serial]
    fn promoting_to_a_stage_the_build_is_past_is_an_error() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, &format!("{CANDIDATE}\n{RELEASE}"));
        let err = run_inner(&dry_run(None, Some(To::Testing), &f))
            .unwrap_err()
            .to_string();
        assert!(err.contains("in -release already"), "{err}");
        assert_eq!(
            run(&dry_run(None, Some(To::Testing), &f)),
            ExitCode::FAILURE
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_build_in_none_of_the_collections_tags_is_an_error() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, "some-other-tag");
        let err = run_inner(&dry_run(Some(NVR), None, &f))
            .unwrap_err()
            .to_string();
        assert!(err.contains("is in none of"), "{err}");
        // koji failing to answer is an error too.
        f.fake("koji", &[("--help", ""), ("list-tags", "!no such build")]);
        let err = run_inner(&dry_run(Some(NVR), None, &f))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no such build"), "{err}");
    }

    /// perf on c9s-hs+fb promotes in both of its collections; a build
    /// named for one of them is skipped in the other.
    #[test]
    #[serial_test::serial]
    fn a_named_build_is_promoted_in_whichever_collection_has_it() {
        let f = Fixture::hyperscale("perf", "c9s-hs+fb");
        f.fake(
            "koji",
            &[
                ("--help", ""),
                ("list-tags", "hyperscale9-packages-facebook-candidate"),
            ],
        );
        run_inner(&dry_run(Some("perf-6.1-1.hs.el9"), None, &f)).unwrap();
        // In none of them: an error naming each, not a silent success.
        f.fake("koji", &[("--help", ""), ("list-tags", "")]);
        let err = run_inner(&dry_run(Some("perf-6.1-1.hs.el9"), None, &f))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains(
                "perf-6.1-1.hs.el9 is in none of hyperscale9s-packages-facebook, \
                 hyperscale9-packages-facebook's tags"
            ),
            "{err}"
        );
    }

    #[test]
    #[serial_test::serial]
    fn a_promotion_needs_koji_and_for_real_cbs() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        f.fake("cbs", &[]);
        let args = PromoteArgs {
            dry_run: false,
            ..dry_run(None, None, &f)
        };
        let err = run_inner(&args).unwrap_err().to_string();
        assert!(err.contains("cbs (install"), "{err}");
    }

    #[test]
    fn a_build_goes_to_the_next_stage_unless_told_otherwise() {
        let n = "rsync-3.2.7-5~proposed.el9";
        assert_eq!(
            destination(Stage::Candidate, None, false, n),
            Ok(Stage::Testing)
        );
        assert_eq!(
            destination(Stage::Testing, None, true, n),
            Ok(Stage::Release)
        );
        assert_eq!(
            destination(Stage::Candidate, Some(To::Release), false, n),
            Ok(Stage::Release)
        );
        let err = destination(Stage::Release, None, false, n).unwrap_err();
        assert!(err.contains("released already"), "{err}");
        let err = destination(Stage::Release, Some(To::Testing), true, n).unwrap_err();
        assert!(err.contains("in -release already"), "{err}");
        assert!(destination(Stage::Testing, Some(To::Testing), true, n).is_err());
        // Released but left in testing: released again, to untag it.
        assert_eq!(
            destination(Stage::Release, None, true, n),
            Ok(Stage::Release)
        );
    }
}
