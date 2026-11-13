// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `promote`: move a build to its collection's next stage, from
//! `-candidate` to `-testing`, from `-testing` to `-release`.
//!
//! By hand that is `cbs tag-build <tag> <nvr>` with the tag spelled out
//! and the NVR looked up, and for a release, a `cbs untag-build` from
//! `-testing` that tends to be forgotten, leaving the build in both. The
//! commands come from sandogasa-cbs (`packages::promote_argvs`), where
//! hs-relmon's `review` can share them. Like every write to CBS, it is
//! confirmed at a terminal and cannot be scripted past.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use sandogasa_cbs::Stage;
use sandogasa_cbs::packages::{furthest_stage, latest, promote_argvs, tags_of};
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
    let mut steps: Vec<(String, Stage, Vec<Vec<String>>)> = Vec::new();
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
                    &["cbs", "list-tagged", "--latest", &candidate, package].map(String::from),
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
        ui.show_command(&["cbs", "list-tags", "--build", &nvr].map(String::from));
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
        steps.push((nvr.clone(), to, promote_argvs(collection, &nvr, to, &tags)));
    }
    if let Some(nvr) = &args.nvr
        && lacking.len() == checkout.collections.len()
    {
        return Err(format!("{nvr} is in none of {}'s tags", lacking.join(", ")).into());
    }
    if steps.is_empty() {
        return Ok(());
    }
    // The cheap checks first: the CBS login and a terminal to confirm
    // at, before any GitLab request.
    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("promoting in CBS is confirmed at a terminal; run it in one".into());
        }
    }
    // The tracking issue is read next, so what it will be told is shown
    // with the promotion and confirmed with it.
    let base = crate::config::gitlab_base();
    let token = crate::config::gitlab_token();
    if crate::notify::covers(checkout.sig) {
        ui.step("Find the tracking issue to tell, through GitLab's API");
    }
    let notices = crate::notify::plan(
        &base,
        token.as_deref(),
        &crate::notify::Branch {
            sig: checkout.sig,
            package,
            branch: &checkout.branch,
            repo: &repo,
            dry_run: args.dry_run,
        },
        &steps,
        &sandogasa_cbs::packages::build_commit,
    );

    if !args.dry_run {
        // Set apart from the login check and the steps above.
        println!();
        for (nvr, to, argvs) in &steps {
            println!("{nvr} to -{}:", to.as_str());
            for argv in argvs {
                println!("  {}", sandogasa_cli::explain::command_line(argv));
            }
        }
        for notice in &notices {
            print!("{}", crate::notify::describe(notice, token.is_some()));
        }
        if !sandogasa_cli::confirm("Promote?", true)? {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    // What got where, so a failure partway still tells the issue of the
    // builds that moved: a rerun would find them promoted, with nothing
    // left to do and so nothing to tell.
    let mut done: Vec<(String, Stage)> = Vec::new();
    let moved = promote_all(&ui, &repo, &steps, args.dry_run, &mut done);
    if args.dry_run {
        // After the steps, where a real run tells the issue.
        for notice in &notices {
            ui.step("Tell the tracking issue, through GitLab's API");
            print!("{}", crate::notify::describe(notice, token.is_some()));
        }
    } else {
        crate::notify::post(&base, token.as_deref(), &notices, &done);
    }
    moved
}

/// Run each step's `cbs` commands, recording in `done` each build that
/// got through all of its own.
fn promote_all(
    ui: &Ui,
    repo: &std::path::Path,
    steps: &[(String, Stage, Vec<Vec<String>>)],
    dry_run: bool,
    done: &mut Vec<(String, Stage)>,
) -> Result<(), Box<dyn std::error::Error>> {
    for (nvr, to, argvs) in steps {
        for argv in argvs {
            let what = if argv[1] == "untag-build" {
                format!("Untag {nvr} from {}, as it is released", argv[2])
            } else {
                format!("Tag {nvr} into {}", argv[2])
            };
            ui.step(&what);
            ui.run_required(argv, repo)?;
        }
        done.push((nvr.clone(), *to));
        if !dry_run {
            match to {
                Stage::Testing => {
                    println!("\n{nvr} is in -testing. Once it has been tested: cbspkg promote")
                }
                _ => println!("{nvr} is released."),
            }
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
