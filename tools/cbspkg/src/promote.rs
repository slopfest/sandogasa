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
    #[arg(long, value_name = "BRANCH")]
    pub branch: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long)]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long)]
    pub explain: bool,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR")]
    pub repo: Option<PathBuf>,

    /// The stage to promote to (default: the one after where the
    /// build is)
    #[arg(long, value_enum)]
    pub to: Option<To>,
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
    let checkout = resolve(&repo, args.branch.as_deref(), None)?;
    let package = &checkout.package;

    // Which build, where it is, and what promoting it takes, per
    // collection. Reading changes nothing, so a dry run does it too.
    let mut steps: Vec<(String, Stage, Vec<Vec<String>>)> = Vec::new();
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
    if steps.is_empty() {
        return Ok(());
    }

    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("promoting in CBS is confirmed at a terminal; run it in one".into());
        }
        for (nvr, to, argvs) in &steps {
            println!("{nvr} to -{}:", to.as_str());
            for argv in argvs {
                println!("  {}", sandogasa_cli::explain::command_line(argv));
            }
        }
        if !sandogasa_cli::confirm("Promote?", true)? {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    for (nvr, to, argvs) in &steps {
        for argv in argvs {
            let what = if argv[1] == "untag-build" {
                format!("Untag {nvr} from {}, as it is released", argv[2])
            } else {
                format!("Tag {nvr} into {}", argv[2])
            };
            ui.step(&what);
            ui.run_required(argv, &repo)?;
        }
        if !args.dry_run {
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
