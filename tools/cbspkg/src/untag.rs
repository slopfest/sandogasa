// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `untag`: take a build out of its collection's `-testing` or
//! `-release` tag, and tell its tracking issue why (#144).
//!
//! By hand that is `cbs untag-build <tag> <nvr>`, once per tag the build
//! is in, with the tags spelled out; nothing tells the tracking issue,
//! so whoever follows it has to check CBS. Here the reason is the
//! command's own: a build rejected in review (the issue is labeled
//! `stage::rejected`, which the next build's stage replaces), a package
//! retired from the SIG (`stage::retired`), or a change that landed in
//! stock CentOS Stream. `-candidate` keeps the build history and is not
//! touched. Like every write to CBS, it is confirmed at a terminal.

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use sandogasa_cbs::notify::Event;
use sandogasa_cbs::packages::tags_of;
use sandogasa_cbs::tags::{CbsCli, TagChange, TagHub};
use sandogasa_cbs::tracking::UntagReason;
use sandogasa_cbs::{Collection, Stage};
use sandogasa_cli::explain::Ui;

use crate::add_pkg::resolve;

#[derive(clap::Args)]
pub struct UntagArgs {
    /// The build to untag
    pub nvr: String,

    /// The branch whose collections to untag in (default: the one
    /// checked out)
    #[arg(long, value_name = "BRANCH", help_heading = "Selection")]
    pub branch: Option<String>,

    /// Untag only from this stage (repeatable; default: -testing and
    /// -release, wherever the build is)
    #[arg(long, value_enum, help_heading = "Selection")]
    pub from: Vec<FromStage>,

    /// The package's dist-git checkout (default: the current
    /// directory)
    #[arg(short = 'C', long, value_name = "DIR", help_heading = "Selection")]
    pub repo: Option<PathBuf>,

    #[command(flatten, next_help_heading = "Selection")]
    pub variants: crate::add_pkg::VariantArgs,

    /// Why the build is untagged, as its tracking issue is told
    #[arg(long, value_enum, help_heading = "Reason")]
    pub reason: Reason,

    /// The stock build the change landed in (with --reason landed)
    #[arg(long, value_name = "NVR", help_heading = "Reason")]
    pub landed_in: Option<String>,

    /// Show each step and its command without running anything
    #[arg(long, help_heading = "Writing")]
    pub dry_run: bool,

    /// Narrate each step and show its command, pausing before each
    #[arg(long, help_heading = "Output")]
    pub explain: bool,
}

/// The stages a build can be untagged from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum FromStage {
    Testing,
    Release,
}

/// Why a build is untagged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Reason {
    /// Rejected in review: the update needs another build
    Rejected,
    /// The package is retired from the SIG
    Retired,
    /// The change landed in stock CentOS Stream (--landed-in)
    Landed,
}

/// The reason the tracking issue is told, from `reason` and the stock
/// build `landed_in`, which `landed` needs.
pub fn untag_reason(reason: Reason, landed_in: Option<&str>) -> Result<UntagReason, String> {
    match (reason, landed_in) {
        (Reason::Rejected | Reason::Retired, Some(_)) => {
            Err("--landed-in goes with --reason landed".into())
        }
        (Reason::Rejected, None) => Ok(UntagReason::Rejected),
        (Reason::Retired, None) => Ok(UntagReason::Retired),
        (Reason::Landed, Some(stock)) => Ok(UntagReason::Landed(stock.to_string())),
        (Reason::Landed, None) => Err("--reason landed needs --landed-in <stock NVR>".into()),
    }
}

/// The changes that untag `nvr` from those of `collection`'s `-testing`
/// and `-release` tags it is in (`tags`), only those of `from` when it
/// names any.
pub fn untag_changes(
    collection: &Collection,
    nvr: &str,
    tags: &[String],
    from: &[FromStage],
) -> Vec<TagChange> {
    [
        (FromStage::Testing, Stage::Testing),
        (FromStage::Release, Stage::Release),
    ]
    .into_iter()
    .filter(|(f, _)| from.is_empty() || from.contains(f))
    .map(|(_, stage)| collection.tag(stage))
    .filter(|tag| tags.contains(tag))
    .map(|tag| TagChange::untag(&tag, nvr))
    .collect()
}

pub fn run(args: &UntagArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &UntagArgs) -> Result<(), Box<dyn std::error::Error>> {
    let reason = untag_reason(args.reason, args.landed_in.as_deref())?;
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

    // The tags the build is in, read once: reading changes nothing, so
    // a dry run does it too.
    ui.step(&format!("Find the tags {} is in", args.nvr));
    ui.show_command(
        &[
            "koji",
            "--profile",
            "cbs",
            "list-tags",
            "--build",
            &args.nvr,
        ]
        .map(String::from),
    );
    let tags = tags_of(&args.nvr)?;
    let changes: Vec<TagChange> = checkout
        .collections
        .iter()
        .flat_map(|c| untag_changes(c, &args.nvr, &tags, &args.from))
        .collect();
    if changes.is_empty() {
        let prefixes: Vec<String> = checkout.collections.iter().map(|c| c.prefix()).collect();
        return Err(format!(
            "{} is in none of {}'s -testing or -release tags{}",
            args.nvr,
            prefixes.join(", "),
            if args.from.is_empty() {
                ""
            } else {
                " that --from names"
            }
        )
        .into());
    }

    // The cheap checks first: the CBS login and a terminal to confirm
    // at, before any GitLab request.
    if !args.dry_run {
        sandogasa_cbs::login::ensure_login("cbspkg", true, args.explain)?;
        if !std::io::stdin().is_terminal() {
            return Err("untagging in CBS is confirmed at a terminal; run it in one".into());
        }
    }
    let base = crate::config::gitlab_base();
    let token = crate::config::gitlab_token();
    ui.step("Find the tracking issue to tell, through GitLab's API");
    let untags: Vec<(String, String)> = changes
        .iter()
        .map(|c| (c.nvr().to_string(), c.tag_name().to_string()))
        .collect();
    let notices = sandogasa_cbs::notify::plan_untags(
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
        &untags,
        &reason,
    );

    if !args.dry_run {
        // Set apart from the login check and the steps above.
        println!();
        for change in &changes {
            println!("  {}", sandogasa_cli::explain::command_line(&change.argv()));
        }
        for notice in &notices {
            print!(
                "{}",
                sandogasa_cbs::notify::describe(notice, token.is_some())
            );
        }
        if !sandogasa_cli::confirm("Untag?", true)? {
            println!("Nothing changed.");
            return Ok(());
        }
    }
    let hub = CbsCli {
        ui: &ui,
        cwd: &repo,
    };
    // What got untagged, so a failure partway still tells the issue of
    // the tags the build left.
    let mut done: Vec<(String, Event)> = Vec::new();
    let untagged = untag_all(&hub, &changes, &mut done);
    if args.dry_run {
        // After the steps, where a real run tells the issue.
        for notice in &notices {
            ui.step("Comment on the tracking issue, through GitLab's API");
            print!(
                "{}",
                sandogasa_cbs::notify::describe(notice, token.is_some())
            );
        }
    } else {
        sandogasa_cbs::notify::post(
            &base,
            token.as_deref(),
            &notices,
            &done,
            &sandogasa_cbs::notify::cbs_latest,
        );
    }
    untagged
}

/// Make each untag, recording in `done` each tag the build left; the
/// first failure stops there.
fn untag_all(
    hub: &dyn TagHub,
    changes: &[TagChange],
    done: &mut Vec<(String, Event)>,
) -> Result<(), Box<dyn std::error::Error>> {
    for change in changes {
        let why = format!("Untag {} from {}", change.nvr(), change.tag_name());
        hub.apply(change, &why)?;
        done.push((
            change.nvr().to_string(),
            Event::Untagged(change.tag_name().to_string()),
        ));
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

    fn dry_run(f: &Fixture, from: &[FromStage], reason: Reason) -> UntagArgs {
        UntagArgs {
            nvr: NVR.to_string(),
            branch: None,
            from: from.to_vec(),
            repo: Some(f.repo.clone()),
            variants: Default::default(),
            reason,
            landed_in: None,
            dry_run: true,
            explain: false,
        }
    }

    /// koji, with the build in `tags`.
    fn koji(f: &Fixture, tags: &str) {
        f.fake("koji", &[("--help", ""), ("list-tags", tags)]);
    }

    #[test]
    fn the_reason_needs_the_stock_build_when_it_landed() {
        assert_eq!(
            untag_reason(Reason::Rejected, None),
            Ok(UntagReason::Rejected)
        );
        assert_eq!(
            untag_reason(Reason::Retired, None),
            Ok(UntagReason::Retired)
        );
        assert_eq!(
            untag_reason(Reason::Landed, Some("rsync-3.2.7-6.el10")),
            Ok(UntagReason::Landed("rsync-3.2.7-6.el10".into()))
        );
        assert!(
            untag_reason(Reason::Landed, None)
                .unwrap_err()
                .contains("--landed-in")
        );
    }

    #[test]
    fn only_the_testing_and_release_tags_the_build_is_in_are_untagged() {
        let c = sandogasa_cbs::Sig::ProposedUpdates
            .collection_for_branch("c10s-proposed-RHEL-1")
            .unwrap();
        let tags = |ts: &[&str]| ts.iter().map(|t| t.to_string()).collect::<Vec<_>>();
        let lines = |changes: Vec<TagChange>| {
            changes
                .iter()
                .map(|c| c.argv().join(" "))
                .collect::<Vec<_>>()
        };
        // -candidate keeps the history.
        assert_eq!(
            lines(untag_changes(
                &c,
                NVR,
                &tags(&[CANDIDATE, TESTING, RELEASE]),
                &[]
            )),
            [
                format!("cbs untag-build {TESTING} {NVR}"),
                format!("cbs untag-build {RELEASE} {NVR}")
            ]
        );
        assert_eq!(
            lines(untag_changes(
                &c,
                NVR,
                &tags(&[TESTING, RELEASE]),
                &[FromStage::Release]
            )),
            [format!("cbs untag-build {RELEASE} {NVR}")]
        );
        assert!(untag_changes(&c, NVR, &tags(&[CANDIDATE]), &[]).is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn a_dry_run_shows_the_untag_without_making_it() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, &format!("{CANDIDATE}\n{TESTING}"));
        run_inner(&dry_run(&f, &[], Reason::Rejected)).unwrap();
        assert_eq!(run(&dry_run(&f, &[], Reason::Rejected)), ExitCode::SUCCESS);
    }

    #[test]
    #[serial_test::serial]
    fn a_build_in_no_tag_to_untag_from_is_an_error() {
        let f = Fixture::proposed("c10s-proposed-RHEL-1", &[]);
        koji(&f, CANDIDATE);
        let err = run_inner(&dry_run(&f, &[], Reason::Rejected))
            .unwrap_err()
            .to_string();
        assert!(err.contains("is in none of"), "{err}");
        koji(&f, TESTING);
        let err = run_inner(&dry_run(&f, &[FromStage::Release], Reason::Retired))
            .unwrap_err()
            .to_string();
        assert!(err.ends_with("that --from names"), "{err}");
        // A landing without its stock build stops before anything.
        assert_eq!(run(&dry_run(&f, &[], Reason::Landed)), ExitCode::FAILURE);
    }

    #[derive(Default)]
    struct FakeHub {
        refuse: Vec<&'static str>,
    }

    impl TagHub for FakeHub {
        fn apply(&self, change: &TagChange, _why: &str) -> Result<(), Box<dyn std::error::Error>> {
            if self.refuse.contains(&change.tag_name()) {
                return Err("refused".into());
            }
            Ok(())
        }
    }

    #[test]
    fn each_tag_left_is_recorded_until_one_fails() {
        let changes = [
            TagChange::untag(TESTING, NVR),
            TagChange::untag(RELEASE, NVR),
        ];
        let mut done = Vec::new();
        untag_all(&FakeHub::default(), &changes, &mut done).unwrap();
        assert_eq!(done.len(), 2);
        let mut done = Vec::new();
        let hub = FakeHub {
            refuse: vec![RELEASE],
        };
        assert!(untag_all(&hub, &changes, &mut done).is_err());
        assert_eq!(
            done,
            [(NVR.to_string(), Event::Untagged(TESTING.to_string()))]
        );
    }
}
