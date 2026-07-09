// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::process::ExitCode;

use clap::{Parser, Subcommand};

mod branch_request;
mod check_crate;
mod check_update;
mod config;
mod dag;
mod karma;
mod resolve;
mod review_deps;
mod submit;

use resolve::{
    FedrqResolver, ResolveOptions, resolve_closure_with_options, resolve_with_installability,
};

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"))
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Args, Clone)]
struct ResolveArgs {
    /// Source RPM names to analyze.
    packages: Vec<String>,

    /// Branch to take packages from (e.g. rawhide).
    #[arg(short, long)]
    source: Option<String>,

    /// Repository class for the source branch (fedrq -r).
    #[arg(long, value_name = "REPO")]
    source_repo: Option<String>,

    /// Branch to port packages to (e.g. epel10).
    #[arg(short, long)]
    target: Option<String>,

    /// Repository class for the target branch (fedrq -r).
    #[arg(long, value_name = "REPO")]
    target_repo: Option<String>,

    /// Output as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,

    /// Write a TOML report (package list + dependency edges) for
    /// the branch-request subcommands to consume.
    #[arg(long, value_name = "FILE")]
    report: Option<String>,

    /// Output build-order as a Koji chain build string.
    #[arg(long)]
    koji: bool,

    /// Generate a shell script for Copr batch builds.
    #[arg(
        long,
        long_help = "\
Generate a shell script for Copr batch builds.

The script accepts the Copr repo as its first
argument, followed by any extra flags to pass
to copr build-package."
    )]
    copr: bool,

    /// Check that subpackages are installable.
    #[arg(
        long,
        long_help = "\
Check that subpackages are installable.

Verifies that the Requires of every
subpackage in the closure can be satisfied
by the target repo or by other packages
in the closure."
    )]
    check_install: bool,

    /// Exclude packages from the closure.
    #[arg(
        long,
        value_name = "PKG,...",
        value_delimiter = ',',
        long_help = "\
Exclude source packages from the closure.

Comma-separated list of source packages to
treat as already available on the target. Their
BuildRequires will not be resolved and they will
not appear in the closure. Useful for packages
you plan to excise from the build requirements.

May be passed multiple times."
    )]
    exclude: Vec<String>,

    /// Exclude packages from installability checks.
    #[arg(
        long,
        value_name = "PKG,...",
        value_delimiter = ',',
        long_help = "\
Exclude packages from installability checks.

Comma-separated list of source packages.
Deps provided by these packages are treated
as satisfied and they will not be pulled into
the closure. Useful for packages like glibc
whose version mismatch between Rawhide and
older releases is expected.

May be passed multiple times."
    )]
    exclude_install: Vec<String>,

    /// Base-distro packages to override with alternate packages.
    #[arg(
        long = "override",
        value_name = "PKG,...",
        value_delimiter = ',',
        long_help = "\
Base-distro packages to treat as deliberate
overrides (alternate, non-conflicting EPEL
packages).

Normally a dependency whose provider exists in
the base distro (RHEL / CentOS Stream) at a
too-old version is *blocked*: EPEL packages
must not replace base-distro packages, so the
closure is pruned there and the report explains
the options. Listing a package here confirms
you intend to introduce an alternate package
instead — the analysis then descends into it.
Note an alternate package needs a NEW package
review, not a branch request.

May be passed multiple times."
    )]
    overrides: Vec<String>,

    /// Base-distro branch to probe (e.g. c10s).
    #[arg(
        long,
        value_name = "BRANCH",
        long_help = "\
Base-distro branch behind the target, probed to
detect deps whose provider exists in the base
at a too-old version (EPEL must not replace
base packages).

Inferred for EPEL targets: epel10 uses c10s,
epel9 uses al9 (fedrq's c9s layers epel9 +
epel9-next, and UBI is incomplete, so AlmaLinux
stands in for RHEL 9). Pass this to override
the mapping or to enable the guard for targets
it can't infer (e.g. epel8)."
    )]
    base_branch: Option<String>,

    /// Disable auto-exclusion from installability checks.
    #[arg(
        long,
        long_help = "\
Disable auto-exclusion of default packages
(e.g. glibc) from installability checks.

By default, packages whose version mismatch
between branches is expected and harmless
are excluded automatically."
    )]
    no_auto_exclude_install: bool,

    /// Max recursion depth (0 = unlimited).
    #[arg(long, default_value = "0")]
    max_depth: usize,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Number of parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0")]
    jobs: usize,

    /// Clear fedrq + libdnf5 repo metadata caches before querying.
    #[arg(long)]
    refresh: bool,
}

#[derive(clap::Args, Clone)]
struct CheckUpdateArgs {
    /// Koji side tag, Bodhi update alias/URL, or COPR project.
    #[arg(long_help = "\
The update to check, one of:
- a Koji side tag (f45-build-side-143123)
- a Bodhi update alias or URL
  (FEDORA-EPEL-2026-f9eaa11e18)
- a COPR project: owner/project spec
  (@rust/uutils-and-nushell) or its URL. COPR
  input requires -b, and --testing-branch when
  -b is a base branch like al9 (it picks the
  COPR chroot, e.g. epel9 → epel-9-*).")]
    input: String,

    /// Branch to check against (e.g. epel9).
    #[arg(
        short = 'b',
        long,
        long_help = "\
Branch to check against (e.g. epel9).

Auto-detected from the input: the Bodhi
release for an update alias, or the name of a
Fedora side tag (f43-build-side-* uses f43).
EPEL side tags are not inferred: pass a base
branch plus the EPEL repo, e.g. -b al9 -r
@epel (epel9) or -b c10s -r @epel (epel10)."
    )]
    branch: Option<String>,

    /// Repository class for the branch (fedrq -r).
    #[arg(
        short = 'r',
        long,
        value_name = "REPO",
        long_help = "\
Repository class for the branch (fedrq -r).

Defaults to the branch's stable base repos,
which is the correct comparison baseline.
Override only for special cases, e.g. -r @epel
for EPEL side tags (paired with a base branch
like -b al9)."
    )]
    repo: Option<String>,

    /// Override branch for @testing / COPR-chroot queries.
    #[arg(
        long,
        long_help = "\
Override branch for the new-provides queries:
@testing for Bodhi updates, and the chroot
selection for COPR input (epel9 → epel-9-*).

Auto-detected for EPEL side tags
(e.g. epel9-build-side-* uses epel9).
Otherwise defaults to --branch."
    )]
    testing_branch: Option<String>,

    /// Koji CLI profile (e.g. cbs for CentOS).
    #[arg(long)]
    koji_profile: Option<String>,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Show full lists instead of counts.
    #[arg(
        long,
        long_help = "\
Show full lists (every package, Provide, and
reverse dep) instead of counts plus the
actionable problems."
    )]
    detailed: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Clear fedrq + libdnf5 repo metadata caches before querying.
    #[arg(long)]
    refresh: bool,

    /// Parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0", hide_default_value = true)]
    jobs: usize,

    /// Cast karma on the update based on the check result.
    #[arg(
        long = "give-karma",
        conflicts_with = "json",
        long_help = "\
Cast karma on the Bodhi update. The check
result suggests the value (+1 when no issues
are found, -1 when reverse deps break or the
updated packages have unsatisfied deps, 0 when
the analysis was incomplete); you are prompted
with that suggestion as the default. Requires a
Bodhi update alias or URL as input. Reuses the
bodhi CLI's login session, starting an
interactive login first if there is none.

Listed bugs get per-bug feedback: update-request
bugs (\"<pkg>-<version> is available\") are
auto-voted +1 when the update delivers at least
the requested version and -1 otherwise; for
other bugs you are prompted. The full plan is
shown for confirmation before posting."
    )]
    give_karma: bool,

    /// Reviewer notes added near the top of the report.
    #[arg(
        long,
        long_help = "\
Reviewer notes added as a section near the top
of the posted report (with --give-karma, or the
review comment --submit posts after creating
the update). Prompted for interactively when
omitted; --yes skips the prompt."
    )]
    comment: Option<String>,

    /// Skip vote/submit confirmations; non-update bugs get 0.
    #[arg(short = 'y', long)]
    yes: bool,

    /// Submit the side tag to Bodhi if the check passes.
    #[arg(
        long,
        conflicts_with_all = ["give_karma", "json"],
        long_help = "\
Submit the side tag as a Bodhi update once the
check passes: creates the update from the tag
(the API behind `bodhi updates new --from-tag`)
after showing the plan — packages, type, bugs,
karma thresholds, notes — for confirmation, so
an accidentally missing package is caught
before anything is published. Requires a Koji
side tag as input and update notes via --notes
or --notes-file. Reuses the bodhi CLI's login
session, starting an interactive login first if
there is none.

After submitting, the check report is posted on
the new update as a review comment with per-bug
feedback (whether each listed bug is addressed
by the delivered versions) — the --give-karma
flow; Bodhi zeroes the submitter's own overall
karma, but per-bug feedback still counts.

When the check does NOT pass cleanly you can
curate the findings (keep/explain/remove) like
--give-karma; if findings are kept you are
asked whether to submit anyway (default no).
Non-interactive runs and --yes never submit a
failing update."
    )]
    submit: bool,

    /// Update notes/description (inline).
    #[arg(long, requires = "submit", conflicts_with = "notes_file")]
    notes: Option<String>,

    /// Read the update notes from a file.
    #[arg(long, value_name = "PATH", requires = "submit")]
    notes_file: Option<std::path::PathBuf>,

    /// Type: bugfix, enhancement, security, newpackage.
    #[arg(
        long = "type",
        value_name = "TYPE",
        default_value = "bugfix",
        requires = "submit",
        hide_possible_values = true,
        value_parser = ["bugfix", "enhancement", "security", "newpackage"]
    )]
    update_type: String,

    /// Severity: low, medium, high, urgent.
    #[arg(
        long,
        value_name = "LEVEL",
        default_value = "unspecified",
        requires = "submit",
        hide_possible_values = true,
        hide_default_value = true,
        long_help = "\
Update severity: unspecified (default), low,
medium, high, or urgent. Bodhi requires a real
severity for --type security.",
        value_parser = ["unspecified", "low", "medium", "high", "urgent"]
    )]
    severity: String,

    /// Bug ID(s) to associate and close (repeated or CSV).
    #[arg(
        long = "bug",
        value_name = "ID",
        value_delimiter = ',',
        requires = "submit"
    )]
    bug: Vec<u64>,

    /// Karma needed to push stable (default 3).
    #[arg(long, value_name = "N", default_value = "3", requires = "submit")]
    stable_karma: i32,

    /// Negative karma that unpushes (default -3).
    #[arg(
        long,
        value_name = "N",
        default_value = "-3",
        allow_hyphen_values = true,
        requires = "submit"
    )]
    unstable_karma: i32,

    /// Don't auto-push at the karma thresholds.
    #[arg(long, requires = "submit")]
    disable_autokarma: bool,
}

#[derive(clap::Args, Clone)]
struct CheckCrateArgs {
    /// Crate name on crates.io.
    name: String,

    /// Crate version (default: latest).
    version: Option<String>,

    /// Target branch (e.g. epel9, rawhide).
    #[arg(short = 'b', long)]
    branch: Option<String>,

    /// Repository class for the branch (fedrq -r).
    #[arg(short = 'r', long, value_name = "REPO")]
    repo: Option<String>,

    /// Expand missing deps transitively.
    #[arg(short = 't', long)]
    transitive: bool,

    /// Exclude dev dependencies from transitive expansion.
    #[arg(long, requires = "transitive")]
    exclude_dev: bool,

    /// Include optional dependencies in transitive expansion.
    #[arg(long, requires = "transitive")]
    include_optional: bool,

    /// Exclude unmet-version deps from transitive expansion.
    #[arg(
        long,
        requires = "transitive",
        long_help = "\
Exclude unmet-version dependencies (packaged,
but too old for the requirement) from
transitive expansion. They are included by
default: omitting them silently under-reports
what needs (re)building."
    )]
    exclude_unmet: bool,

    /// Exclude crates from transitive expansion.
    #[arg(
        long,
        requires = "transitive",
        value_delimiter = ',',
        value_name = "CRATE,..."
    )]
    exclude: Vec<String>,

    /// Generate a shell script for Copr batch builds.
    #[arg(
        long,
        requires = "transitive",
        long_help = "\
Generate a shell script for Copr batch builds.

The script accepts the Copr repo as its first
argument, followed by any extra flags to pass
to copr build-package."
    )]
    copr: bool,

    /// Output dependency graph in Graphviz DOT format.
    #[arg(long, requires = "transitive")]
    dot: bool,

    /// Machine-readable JSON output.
    #[arg(long)]
    json: bool,

    /// Output build-order as a Koji chain build string.
    #[arg(long, requires = "transitive")]
    koji: bool,

    /// Write analysis to a TOML file.
    #[arg(long, value_name = "PATH", requires = "transitive")]
    toml: Option<String>,

    /// Clear fedrq + libdnf5 repo metadata caches before querying.
    #[arg(long)]
    refresh: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,

    /// Parallel fedrq queries (0 = CPUs).
    #[arg(short = 'j', long, default_value = "0", hide_default_value = true)]
    jobs: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Analyze a crates.io crate's dependencies.
    CheckCrate(CheckCrateArgs),
    /// Find and link Bugzilla package review requests.
    CheckPkgReviews(CheckPkgReviewsArgs),
    /// Check if an update would break reverse dependencies.
    CheckUpdate(CheckUpdateArgs),
    /// Set up Bugzilla API key and other settings.
    Config,
    /// Escalate (needinfo) stale branch requests in a report.
    Escalate(EscalateArgs),
    /// File a branch request for one package.
    FileRequest(FileRequestArgs),
    /// File branch requests for all missing packages in a report.
    FileRequests(FileRequestsArgs),
    /// Detect dependency cycles in the build graph.
    FindCycles(ResolveArgs),
    /// Resolve the full dependency closure for porting.
    Resolve(ResolveArgs),
}

/// Bugzilla connection + co-maintainer offer flags shared by the
/// branch-request subcommands.
#[derive(clap::Args, Clone)]
struct BranchRequestCommon {
    /// EPEL branch to request (e.g. epel9, epel10).
    branch: String,

    /// Bugzilla base URL.
    #[arg(long, default_value = "https://bugzilla.redhat.com")]
    bugzilla_url: String,

    /// Bugzilla API key (defaults to BUGZILLA_API_KEY env var or
    /// the key from `ebranch config`).
    #[arg(long, env = "BUGZILLA_API_KEY")]
    api_key: Option<String>,

    /// FAS of the reporter, if willing to co-maintain.
    #[arg(long)]
    fas: Option<String>,

    /// Packaging SIG to offer as co-maintainer (requires --fas).
    #[arg(long)]
    sig: Option<String>,

    /// Base-distro branch to pre-flight against (e.g. c10s).
    #[arg(
        long,
        value_name = "BRANCH",
        long_help = "\
Base-distro branch behind the EPEL branch,
checked before filing: a package present in
the base distro is skipped (EPEL must not
replace it; the request would be CANTFIX).

Inferred from the branch: epel10 uses c10s,
epel9 uses al9. Pass this to override the
mapping or to enable the check for branches
it can't infer (e.g. epel8)."
    )]
    base_branch: Option<String>,

    /// Show what would happen without contacting Bugzilla.
    #[arg(long)]
    dry_run: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args, Clone)]
struct FileRequestArgs {
    /// Source package to request a branch for.
    package: String,

    #[command(flatten)]
    common: BranchRequestCommon,

    /// CSV of bugs/aliases this request blocks.
    #[arg(long, value_delimiter = ',')]
    blocked: Vec<String>,

    /// CSV of bugs/aliases this request depends on.
    #[arg(long, value_delimiter = ',')]
    dependson: Vec<String>,

    /// check-crate report TOML to record the new bug ID in.
    #[arg(long)]
    toml: Option<String>,
}

#[derive(clap::Args, Clone)]
struct FileRequestsArgs {
    /// check-crate report TOML listing the missing packages.
    toml: String,

    #[command(flatten)]
    common: BranchRequestCommon,

    /// CSV of bugs/aliases each filed request blocks.
    #[arg(long, value_delimiter = ',')]
    blocked: Vec<String>,
}

#[derive(clap::Args, Clone)]
struct EscalateArgs {
    /// check-crate report TOML with recorded branch requests.
    toml: String,

    #[command(flatten)]
    common: BranchRequestCommon,
}

#[derive(clap::Args, Clone)]
struct CheckPkgReviewsArgs {
    /// Path to TOML analysis file from check-crate --toml.
    toml: String,

    /// Bugzilla base URL.
    #[arg(long, default_value = "https://bugzilla.redhat.com")]
    bugzilla_url: String,

    /// Bugzilla API key (or set BUGZILLA_API_KEY env var).
    #[arg(long, env = "BUGZILLA_API_KEY")]
    api_key: Option<String>,

    /// Show changes without applying them.
    #[arg(long)]
    dry_run: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

enum Mode {
    Resolve,
    FindCycles,
}

/// Build branch-request `Options` from the shared flags,
/// resolving the API key (CLI flag → env → config file).
fn branch_request_options(c: &BranchRequestCommon) -> Result<branch_request::Options, String> {
    let api_key = config::resolve_api_key(c.api_key.as_deref())?;
    Ok(branch_request::Options {
        bugzilla_url: c.bugzilla_url.clone(),
        api_key,
        branch: c.branch.clone(),
        fas: c.fas.clone(),
        sig: c.sig.clone(),
        dry_run: c.dry_run,
        verbose: c.verbose,
        base_branch: resolve::base_branch_for(c.base_branch.as_deref(), Some(&c.branch), None),
    })
}

/// Dispatch the Bugzilla-backed branch-request subcommands.
/// Returns `Some(exit_code)` when `cmd` was one of them, `None`
/// otherwise so the caller proceeds to the fedrq commands.
fn handle_branch_request_command(cmd: &Command) -> Option<ExitCode> {
    let result = match cmd {
        Command::FileRequest(a) => branch_request_options(&a.common).and_then(|opts| {
            branch_request::run_file_request(
                &a.package,
                &a.blocked,
                &a.dependson,
                a.toml.as_deref(),
                &opts,
            )
        }),
        Command::FileRequests(a) => branch_request_options(&a.common)
            .and_then(|opts| branch_request::run_file_requests(&a.toml, &a.blocked, &opts)),
        Command::Escalate(a) => branch_request_options(&a.common)
            .and_then(|opts| branch_request::run_escalate(&a.toml, &opts)),
        _ => return None,
    };
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    })
}

/// Clear the fedrq + libdnf5 repo metadata caches if `--refresh` was passed.
fn handle_refresh(refresh: bool, verbose: bool) -> Result<(), ExitCode> {
    if refresh {
        // Drop both the smartcache and libdnf5: the smartcache clear
        // alone misses the host's *native* branch, which reuses
        // ~/.cache/libdnf5 and can otherwise serve stale metadata.
        if let Err(e) = sandogasa_fedrq::clear_all_caches() {
            eprintln!("error: failed to clear metadata caches: {e}");
            return Err(ExitCode::FAILURE);
        }
        if verbose {
            eprintln!("cleared fedrq + libdnf5 caches");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    let cli = Cli::parse();

    // config and check-pkg-reviews don't need fedrq.
    if matches!(cli.command, Command::Config) {
        let rt = tokio::runtime::Runtime::new().expect("failed to create async runtime");
        return match rt.block_on(config::cmd_config()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if let Command::CheckPkgReviews(a) = &cli.command {
        let api_key = match config::resolve_api_key(a.api_key.as_deref()) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        };
        let opts = review_deps::CheckPkgReviewsOptions {
            toml_path: a.toml.clone(),
            bugzilla_url: a.bugzilla_url.clone(),
            api_key,
            dry_run: a.dry_run,
            verbose: a.verbose,
        };
        return match review_deps::check_pkg_reviews(&opts) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    // Branch-request subcommands talk to Bugzilla, not fedrq.
    if let Some(code) = handle_branch_request_command(&cli.command) {
        return code;
    }

    // All other subcommands need fedrq.
    if let Err(e) =
        sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])
    {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    // CheckCrate and CheckUpdate have their own args; handle separately.
    if let Command::CheckCrate(a) = &cli.command {
        if a.branch.is_none() && a.repo.is_none() {
            eprintln!("error: at least one of --branch or --repo is required");
            return ExitCode::FAILURE;
        }
        if let Err(code) = handle_refresh(a.refresh, a.verbose) {
            return code;
        }
        if a.jobs > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(a.jobs)
                .build_global()
                .expect("failed to configure thread pool");
        }
        let label = match (&a.branch, &a.repo) {
            (Some(b), Some(r)) => format!("{b} ({r})"),
            (Some(b), None) => b.clone(),
            (None, Some(r)) => r.clone(),
            (None, None) => unreachable!(),
        };
        let opts = check_crate::CheckCrateOptions {
            branch: a.branch.clone(),
            repo: a.repo.clone(),
            label,
            verbose: a.verbose,
            transitive: a.transitive,
            exclude_dev: a.exclude_dev,
            include_optional: a.include_optional,
            include_too_old: !a.exclude_unmet,
            exclude: a.exclude.iter().cloned().collect(),
        };
        return match check_crate::check_crate(&a.name, a.version.as_deref(), &opts) {
            Ok(report) => {
                if let Some(ref path) = a.toml
                    && let Err(e) = check_crate::write_toml(&report, path)
                {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
                if a.koji || a.copr {
                    // Machine output on stdout (pipeable); the human report
                    // on stderr so you can still see what needs building.
                    check_crate::eprint_report(&report);
                    let rpm_phases = map_phase_packages(&report.full_build_phases(), |name| {
                        format!("rust-{name}")
                    });
                    if a.copr {
                        print_copr_script(&rpm_phases);
                    } else {
                        print_koji_chain(&rpm_phases);
                    }
                } else if a.dot {
                    check_crate::eprint_report(&report);
                    check_crate::print_dot(&report);
                } else if a.json {
                    print_json(&report);
                } else {
                    check_crate::print_report(&report);
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    if let Command::CheckUpdate(a) = &cli.command {
        let input_kind = check_update::detect_input_type(&a.input);
        // check-update needs koji for side tag queries — which Bodhi
        // input may also trigger (side-tag-backed updates). COPR input
        // never touches koji.
        if !matches!(input_kind, check_update::InputKind::Copr { .. })
            && let Err(e) =
                sandogasa_cli::require_tools(&[("koji", "sudo dnf install koji", Some("version"))])
        {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        // Voting needs a Bodhi update; submitting needs a side tag
        // (an alias means the update already exists). Fail fast on
        // the wrong input kind.
        let (vote_alias, side_tag) = match &input_kind {
            check_update::InputKind::BodhiAlias(alias) => (Some(alias.clone()), None),
            check_update::InputKind::SideTag(tag) => (None, Some(tag.clone())),
            // COPRs are published through their own repos: nothing to
            // vote on or submit.
            check_update::InputKind::Copr { .. } => (None, None),
        };
        if a.give_karma && vote_alias.is_none() {
            eprintln!("error: --give-karma requires a Bodhi update alias or URL as input");
            return ExitCode::FAILURE;
        }
        if a.submit && side_tag.is_none() {
            let why = match &input_kind {
                check_update::InputKind::BodhiAlias(_) => {
                    "this is a Bodhi update, so it has already been submitted"
                }
                _ => "a COPR is published through its own repos, not Bodhi",
            };
            eprintln!("error: --submit requires a Koji side tag as input; {why}");
            return ExitCode::FAILURE;
        }
        if a.yes && !a.give_karma && !a.submit {
            eprintln!("error: --yes requires --give-karma or --submit");
            return ExitCode::FAILURE;
        }
        if a.comment.is_some() && !a.give_karma && !a.submit {
            eprintln!("error: --comment requires --give-karma or --submit");
            return ExitCode::FAILURE;
        }
        // Bodhi rejects security updates without a real severity;
        // catch it before the analysis rather than at POST time.
        if a.submit && a.update_type == "security" && a.severity == "unspecified" {
            eprintln!("error: --type security requires --severity (low/medium/high/urgent)");
            return ExitCode::FAILURE;
        }
        // Notes are required for submission; resolve (and read the
        // file) up front so a typo'd path fails in seconds.
        let submit_notes = if a.submit {
            match submit::resolve_notes(a.notes.as_deref(), a.notes_file.as_deref()) {
                Ok(n) => Some(n),
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            None
        };
        // Validate the bodhi session up front (logging in if
        // needed) so a missing session doesn't surface only after
        // the analysis has run for minutes.
        if (a.give_karma || a.submit)
            && let Err(e) = karma::ensure_session()
        {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        if let Err(code) = handle_refresh(a.refresh, a.verbose) {
            return code;
        }
        if a.jobs > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(a.jobs)
                .build_global()
                .expect("failed to configure thread pool");
        }
        let opts = check_update::CheckUpdateOptions {
            branch: a.branch.clone(),
            repo: a.repo.clone(),
            testing_branch: a.testing_branch.clone(),
            koji_profile: a.koji_profile.clone(),
            verbose: a.verbose,
            interactive: {
                use std::io::IsTerminal;
                !a.json && std::io::stdin().is_terminal()
            },
        };
        return match check_update::check_update(&a.input, &opts) {
            Ok(report) => {
                if a.json {
                    print_json(&report);
                } else {
                    check_update::print_report(&report, a.detailed);
                }
                let mut report = report;
                // Let the reviewer curate the blocking findings
                // (keep/explain/remove) before karma is derived / the
                // pass gate is applied. Skipped under --yes or
                // non-interactively, where every finding is kept
                // (today's behavior). The curated report drives the
                // karma, the posted comment, and the submit gate.
                let mut addressed = Vec::new();
                if (a.give_karma || a.submit) && !a.yes && opts.interactive {
                    let findings = check_update::blocking_findings(&report);
                    if !findings.is_empty() {
                        eprintln!("── address findings ──");
                        eprintln!(
                            "(k)eep counts against the update / \
                             (e)xplain accepts it / (r)emove drops it\n"
                        );
                        match sandogasa_review::resolve_interactive(findings, |f| f.summary()) {
                            Ok(decisions) => {
                                let (curated, expl) =
                                    check_update::apply_resolutions(report, decisions);
                                report = curated;
                                addressed = expl;
                            }
                            Err(e) => {
                                eprintln!("error: {e}");
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                }
                if a.give_karma
                    && let Some(alias) = &vote_alias
                {
                    let (karma, reason) = karma::derive_karma(&report);
                    // The posted comment is the full Markdown report plus
                    // an "addressed by the reviewer" section; --comment
                    // adds reviewer notes near the top (prompted for
                    // interactively when absent).
                    let mut report_md = check_update::render_report(&report, a.detailed);
                    report_md.push_str(&check_update::render_addressed(&addressed));
                    if let Err(e) =
                        karma::run(alias, karma, &reason, &report_md, a.comment.clone(), a.yes)
                    {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
                if a.submit
                    && let Some(tag) = &side_tag
                {
                    // The pass gate reuses the karma derivation: +1 is
                    // a clean pass; 0 (incomplete/stale analysis) or -1
                    // (breakage) needs an explicit interactive override
                    // — never auto-submitted, not even under --yes.
                    let (karma, reason) = karma::derive_karma(&report);
                    if karma < 1 {
                        eprintln!("check did not pass cleanly: {reason}");
                        if a.yes || !opts.interactive {
                            eprintln!(
                                "not submitting — fix the update, or rerun \
                                 interactively to override"
                            );
                            return ExitCode::FAILURE;
                        }
                        match submit::confirm_default_no("submit anyway?") {
                            Ok(true) => {}
                            Ok(false) => {
                                eprintln!("aborted: update not submitted");
                                return ExitCode::FAILURE;
                            }
                            Err(e) => {
                                eprintln!("error: {e}");
                                return ExitCode::FAILURE;
                            }
                        }
                    }
                    let sopts = submit::SubmitOptions {
                        notes: submit_notes.clone().expect("resolved before the analysis"),
                        update_type: a.update_type.clone(),
                        severity: a.severity.clone(),
                        bugs: a.bug.clone(),
                        autokarma: !a.disable_autokarma,
                        stable_karma: a.stable_karma,
                        unstable_karma: a.unstable_karma,
                        assume_yes: a.yes,
                    };
                    let aliases = match submit::run(tag, &report.updated_packages, &sopts) {
                        Ok(aliases) => aliases,
                        Err(e) => {
                            eprintln!("error: {e}");
                            return ExitCode::FAILURE;
                        }
                    };
                    // Post the check report as a comment on the new
                    // update — the same review-checklist flow as
                    // --give-karma, including per-bug feedback on
                    // whether the listed bugs are addressed. Bodhi
                    // zeroes the submitter's overall karma on their
                    // own update (karma::run detects that); the
                    // per-bug feedback still counts.
                    let mut report_md = check_update::render_report(&report, a.detailed);
                    report_md.push_str(&check_update::render_addressed(&addressed));
                    for alias in &aliases {
                        if let Err(e) =
                            karma::run(alias, karma, &reason, &report_md, a.comment.clone(), a.yes)
                        {
                            eprintln!(
                                "error: the update was submitted, but posting the review \
                                 comment failed: {e}"
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                }
                let has_broken = report.reverse_deps.values().any(|r| r.status == "broken");
                if has_broken {
                    ExitCode::FAILURE
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::FAILURE
            }
        };
    }

    let (args, mode) = match &cli.command {
        Command::CheckCrate(_)
        | Command::CheckPkgReviews(_)
        | Command::CheckUpdate(_)
        | Command::Config
        | Command::Escalate(_)
        | Command::FileRequest(_)
        | Command::FileRequests(_) => unreachable!(),
        Command::FindCycles(a) => (a, Mode::FindCycles),
        Command::Resolve(a) => (a, Mode::Resolve),
    };

    if args.source.is_none() && args.source_repo.is_none() {
        eprintln!("error: at least one of --source or --source-repo is required");
        return ExitCode::FAILURE;
    }
    if args.target.is_none() && args.target_repo.is_none() {
        eprintln!("error: at least one of --target or --target-repo is required");
        return ExitCode::FAILURE;
    }

    if let Err(code) = handle_refresh(args.refresh, args.verbose) {
        return code;
    }

    if args.jobs > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.jobs)
            .build_global()
            .expect("failed to configure thread pool");
    }

    // When the source repo is a Koji repo, create a @koji-src:
    // companion for source RPM queries (BuildRequires, subpkg Requires).
    let source_src = args.source_repo.as_deref().and_then(|r| {
        r.strip_prefix("@koji:").map(|tag| sandogasa_fedrq::Fedrq {
            branch: args.source.clone(),
            repo: Some(format!("@koji-src:{tag}")),
        })
    });

    // Base-distro guard: probe the base behind an EPEL target so deps
    // whose provider exists there (too old) are blocked instead of
    // becoming branch requests.
    let base_branch = resolve::base_branch_for(
        args.base_branch.as_deref(),
        args.target.as_deref(),
        args.target_repo.as_deref(),
    );
    if base_branch.is_none()
        && args
            .target
            .as_deref()
            .is_some_and(|t| t.starts_with("epel"))
    {
        eprintln!(
            "warning: no base-distro mapping for this EPEL target; \
             base-distro guard disabled (pass --base-branch to enable)"
        );
    }
    if args.verbose
        && let Some(ref b) = base_branch
    {
        eprintln!("[resolve] base-distro guard active (probing {b})");
    }

    let resolver = FedrqResolver {
        source: sandogasa_fedrq::Fedrq {
            branch: args.source.clone(),
            repo: args.source_repo.clone(),
        },
        source_src,
        target: sandogasa_fedrq::Fedrq {
            branch: args.target.clone(),
            repo: args.target_repo.clone(),
        },
        base: base_branch.as_ref().map(|b| sandogasa_fedrq::Fedrq {
            branch: Some(b.clone()),
            repo: None,
        }),
    };
    let source_label = match (&args.source, &args.source_repo) {
        (Some(b), Some(r)) => format!("{b} ({r})"),
        (Some(b), None) => b.clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => unreachable!(),
    };
    let target_label = match (&args.target, &args.target_repo) {
        (Some(b), Some(r)) => format!("{b} ({r})"),
        (Some(b), None) => b.clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => unreachable!(),
    };
    let options = ResolveOptions {
        max_depth: args.max_depth,
        verbose: args.verbose,
        exclude: args.exclude.iter().cloned().collect(),
        exclude_install: args.exclude_install.iter().cloned().collect(),
        auto_exclude: !args.no_auto_exclude_install,
        base_branch,
        overrides: args.overrides.iter().cloned().collect(),
        interactive: {
            use std::io::IsTerminal;
            !args.json && std::io::stdin().is_terminal()
        },
    };
    let (closure, install_report) = if args.check_install {
        match resolve_with_installability(
            &resolver,
            &args.packages,
            &source_label,
            &target_label,
            &options,
        ) {
            Ok((c, r)) => (c, Some(r)),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match resolve_closure_with_options(
            &resolver,
            &args.packages,
            &source_label,
            &target_label,
            &options,
        ) {
            Ok(c) => (c, None),
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    for w in &closure.warnings {
        eprintln!("warning: {w}");
    }

    if let Some(report) = &install_report {
        for (pkg, entry) in &report.issues {
            for u in &entry.unsatisfied {
                match &u.provided_by {
                    Some(provider) => {
                        eprintln!("install: {pkg}: {dep} (needs {provider})", dep = u.dep)
                    }
                    None => eprintln!("install: {pkg}: {dep} (unresolvable)", dep = u.dep),
                }
            }
        }
    }

    // Persist a branch-request report when asked, regardless of
    // the stdout output format.
    if let Some(path) = &args.report {
        let report = resolve::ResolveReport::from_closure(&closure);
        if let Err(e) = resolve::write_report(&report, path) {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
        if args.verbose {
            eprintln!(
                "wrote report with {} package(s) to {path}",
                report.packages.len()
            );
        }
    }

    match mode {
        Mode::Resolve => {
            let edges = closure.to_edges();
            let phases = match dag::topological_layers(&edges) {
                Ok(p) => p,
                Err(_) => {
                    eprintln!(
                        "warning: dependency graph contains cycles; \
                         build order unavailable \
                         (run 'find-cycles' for details)"
                    );
                    vec![]
                }
            };

            if args.copr {
                // Machine output on stdout; the blocked section (a
                // human must act on it) goes to stderr.
                eprint!("{}", render_blocked(&closure));
                print_copr_script(&phases);
            } else if args.koji {
                eprint!("{}", render_blocked(&closure));
                print_koji_chain(&phases);
            } else if args.json {
                let mut json = serde_json::json!({
                    "source_branch": closure.source_branch,
                    "target_branch": closure.target_branch,
                    "requested": closure.requested,
                    "closure": closure.closure,
                    "blocked_by_base": closure.blocked_by_base,
                    "overrides": closure.overrides,
                    "warnings": closure.warnings,
                    "build_order": phases,
                });
                if let Some(report) = &install_report {
                    json["installability"] = serde_json::json!({
                        "issues": report.issues,
                        "additional_packages":
                            report.additional_packages,
                    });
                }
                print_json(&json);
            } else {
                if phases.is_empty() {
                    print_resolve(&closure);
                } else {
                    print_build_order(&phases, &closure);
                }
                if let Some(report) = &install_report {
                    print_installability(report);
                }
                print!("{}", render_blocked(&closure));
            }
            ExitCode::SUCCESS
        }
        Mode::FindCycles => {
            let edges = closure.to_edges();
            let cycles = dag::find_cycles(&edges);
            if args.json {
                print_json(&serde_json::json!({
                    "source_branch": closure.source_branch,
                    "target_branch": closure.target_branch,
                    "requested": closure.requested,
                    "cycles": cycles,
                }));
            } else {
                print_cycles(&cycles, &closure);
            }
            if cycles.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

/// Map package names in build phases through a transform function.
fn map_phase_packages(
    phases: &[dag::BuildPhase],
    f: impl Fn(&str) -> String,
) -> Vec<dag::BuildPhase> {
    phases
        .iter()
        .map(|p| dag::BuildPhase {
            phase: p.phase,
            packages: p.packages.iter().map(|pkg| f(pkg)).collect(),
        })
        .collect()
}

fn print_copr_script(phases: &[dag::BuildPhase]) {
    println!(
        r#"#!/bin/bash
# Generated by ebranch --copr
# Usage: ./script.sh <copr-repo> [extra copr build-package flags...]
set -euo pipefail

REPO="${{1:?Usage: $0 <copr-repo> [extra flags...]}}"
shift
EXTRA=("$@")

extract_build_id() {{
    # Parse "Created builds: <id>" from copr output
    grep -oP 'Created builds: \K[0-9]+' | head -1
}}"#
    );

    for (i, phase) in phases.iter().enumerate() {
        println!();
        println!("# Phase {}", phase.phase);

        for (j, pkg) in phase.packages.iter().enumerate() {
            if i == 0 && j == 0 {
                // Very first package: no dependency flags, capture batch ID.
                println!(
                    r#"PHASE_{phase}_ID=$(copr build-package --nowait --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}" 2>&1 | tee /dev/stderr | extract_build_id)"#,
                    phase = phase.phase,
                    pkg = pkg,
                );
            } else if j == 0 {
                // First package in a new phase: depends on previous phase.
                println!(
                    r#"PHASE_{phase}_ID=$(copr build-package --nowait --after-build-id "$PHASE_{prev}_ID" --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}" 2>&1 | tee /dev/stderr | extract_build_id)"#,
                    phase = phase.phase,
                    prev = phases[i - 1].phase,
                    pkg = pkg,
                );
            } else {
                // Subsequent package in same phase: same batch.
                println!(
                    r#"copr build-package --nowait --with-build-id "$PHASE_{phase}_ID" --name {pkg} "$REPO" "${{EXTRA[@]+"${{EXTRA[@]}}"}}""#,
                    phase = phase.phase,
                    pkg = pkg,
                );
            }
        }
    }
}

fn print_koji_chain(phases: &[dag::BuildPhase]) {
    let chain: Vec<String> = phases
        .iter()
        .map(|phase| phase.packages.join(" "))
        .collect();
    println!("{}", chain.join(" : "));
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).expect("JSON serialization failed")
    );
}

/// Render the base-distro-blocked section: what was pruned, why, and
/// the two real options. Empty when nothing is blocked.
fn render_blocked(closure: &resolve::Closure) -> String {
    use std::fmt::Write as _;
    if closure.blocked_by_base.is_empty() {
        return String::new();
    }
    let base = closure
        .blocked_by_base
        .values()
        .next()
        .map(|b| b.base_branch.as_str())
        .unwrap_or("base");
    let mut out = String::new();
    let _ = writeln!(
        out,
        "\nBlocked by base distro ({base}) — EPEL must not replace these packages:"
    );
    for (pkg, b) in &closure.blocked_by_base {
        let needed_by: Vec<&str> = b.required_by.iter().map(String::as_str).collect();
        let _ = writeln!(
            out,
            "  - {pkg}: needs {} ({}); {} has {}",
            b.dep,
            needed_by.join(", "),
            b.base_branch,
            b.base_version
        );
    }
    let _ = writeln!(
        out,
        "\nOptions for blocked packages: introduce an alternate,\n\
         non-conflicting package (rerun with --override <pkg>; an\n\
         alternate needs a NEW package review, not a branch request),\n\
         or lower the depending package's requirement to the\n\
         base-distro version."
    );
    out
}

/// Annotate an overridden package name in listings.
fn override_marker(closure: &resolve::Closure, pkg: &str) -> &'static str {
    if closure.overrides.contains(pkg) {
        " (override — needs new package review)"
    } else {
        ""
    }
}

fn print_resolve(closure: &resolve::Closure) {
    println!(
        "Dependency closure from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    let discovered = closure.closure.len() - closure.requested.len();
    for (pkg, entry) in &closure.closure {
        let marker = override_marker(closure, pkg);
        if entry.missing_deps.is_empty() {
            println!("  {pkg}{marker}: all BuildRequires satisfied");
        } else {
            println!("  {pkg}{marker}:");
            for dep in &entry.missing_deps {
                println!("    - {} (provided by {})", dep.dep, dep.provided_by);
            }
        }
    }

    println!(
        "\nTotal: {} package(s) in closure ({} requested, {} discovered).",
        closure.closure.len(),
        closure.requested.len(),
        discovered
    );
}

fn print_build_order(phases: &[dag::BuildPhase], closure: &resolve::Closure) {
    println!(
        "Build order from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    for phase in phases {
        println!("  Phase {}:", phase.phase);
        for pkg in &phase.packages {
            println!("    - {pkg}{}", override_marker(closure, pkg));
        }
    }

    println!(
        "\n{} package(s) in {} phase(s).",
        closure.closure.len(),
        phases.len()
    );
}

fn print_installability(report: &resolve::InstallabilityReport) {
    if report.issues.is_empty() {
        println!("\nInstallability: all subpackage Requires satisfied.");
        return;
    }

    println!("\nInstallability issues:\n");
    for (pkg, entry) in &report.issues {
        println!("  {pkg}:");
        for u in &entry.unsatisfied {
            match &u.provided_by {
                Some(provider) => {
                    println!("    - {} (needs {})", u.dep, provider);
                }
                None => {
                    println!("    - {} (unresolvable)", u.dep);
                }
            }
        }
    }

    if !report.additional_packages.is_empty() {
        println!("\nAdditional packages needed for installability:");
        for pkg in &report.additional_packages {
            println!("  - {pkg}");
        }
    }
}

fn print_cycles(cycles: &[dag::Cycle], closure: &resolve::Closure) {
    println!(
        "Cycle detection from {} to {}:\n",
        closure.source_branch, closure.target_branch
    );

    if cycles.is_empty() {
        println!("  No cycles detected. The dependency graph is a DAG.");
    } else {
        println!("  Found {} cycle(s):\n", cycles.len());
        for (i, cycle) in cycles.iter().enumerate() {
            let chain: Vec<&str> = cycle
                .packages
                .iter()
                .map(|s| s.as_str())
                .chain(std::iter::once(cycle.packages[0].as_str()))
                .collect();
            println!("  Cycle {} ({} packages):", i + 1, cycle.packages.len());
            println!("    {}", chain.join(" -> "));
        }
    }
}
