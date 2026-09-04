// SPDX-License-Identifier: Apache-2.0 OR MIT

mod act;
mod adopt;
mod config;
mod dependents;
mod deps;
mod derive;
mod gitlab_unshipped;
mod intersect;
mod kondo;
mod prune_retired;
mod reconcile;
mod semver_audit;
mod triage_retired;
mod triage_updates;
mod unkeep;
mod workspace;

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_distgit::DistGitClient;

#[derive(Parser)]
#[command(
    about,
    long_about = None,
    max_term_width = 80,
    version = sandogasa_cli::version!(),
    before_help = sandogasa_cli::banner!()
)]
struct Cli {
    /// Path(s) to inventory TOML file(s).
    #[arg(short, long, global = true)]
    inventory: Vec<String>,

    /// Directory to scan for *.toml inventory files.
    #[arg(short = 'I', long, value_name = "DIR", global = true)]
    inventory_dir: Vec<String>,

    /// Run as if started in DIR (like `git -C`): the workspace file,
    /// relative inventory paths and outputs are looked up there.
    #[arg(short = 'C', long = "directory", value_name = "DIR", global = true)]
    directory: Option<String>,

    /// Workspace file naming the inventories, graph and walk settings
    /// (default: ./kondo.toml when present). Supplies the flags the
    /// maintenance subcommands would otherwise need; the command line
    /// wins, --no-defaults ignores it.
    #[arg(short = 'w', long, value_name = "PATH", global = true)]
    workspace: Option<String>,

    /// Which [[closure]] of the workspace a walk subcommand works on
    /// (default: the first).
    #[arg(long, value_name = "NAME", global = true)]
    closure: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Enact the standing cull verdicts interactively: orphan,
    /// remove own ACL, or give each package away, adjusting the
    /// inventories as you go.
    Act(ActArgs),
    /// Add a package to the inventory.
    Add(AddArgs),
    /// Adopt orphaned inventory packages on dist-git.
    Adopt(AdoptArgs),
    /// Render the standing cull verdicts as the grouped,
    /// announcement-ready report with runnable ACL commands.
    Announce(AnnounceArgs),
    /// Configure poi-tracker (Bugzilla API key, etc.).
    Config,
    /// Classify inventory packages by who depends on them, over a
    /// saved dependency graph: leaves, packages other keeps carry,
    /// externally needed ones.
    Dependents(DependentsArgs),
    /// Inventory the dependencies pulled from other repos
    /// (e.g. EPEL).
    Deps(DepsArgs),
    /// Recompute a derived dependency inventory offline from a
    /// saved graph: owned packages the keeps reach.
    Derive(DeriveArgs),
    /// Export inventory to another format.
    Export(ExportArgs),
    /// Find which inventory file(s) contain a package.
    Find(FindArgs),
    /// Import from legacy JSON format.
    Import(ImportArgs),
    /// Keep only the inventory packages also present in other
    /// inventories, optionally merging them into a file.
    Intersect(IntersectArgs),
    /// Start keeping packages: walk them against the saved graph
    /// (fedrq only for what it lacks), merge, recompute the derived
    /// inventory.
    Keep(KeepArgs),
    /// Triage packages no essential inventory needs: keep as a
    /// cull candidate, file into an inventory, or drop.
    Kondo(KondoArgs),
    /// Mark (or remove) packages no longer carried on any
    /// active branch (dist-git project gone or retired
    /// everywhere).
    PruneRetired(PruneRetiredArgs),
    /// Bring the workspace's inventories up to date with its keeps:
    /// walk new keeps, recompute the derived inventories, ask the
    /// decisions a program cannot make, then triage what nothing
    /// justifies.
    Reconcile(ReconcileArgs),
    /// Remove a package from the inventory.
    Remove(RemoveArgs),
    /// Audit pending upstream updates by semver impact, flagging
    /// which are non-breaking, breaking, or need review.
    SemverAudit(SemverAuditArgs),
    /// Show inventory contents.
    Show(ShowArgs),
    /// Sync inventory from Fedora dist-git (Pagure) access.
    SyncDistgit(SyncDistgitArgs),
    /// Sync inventory from a GitLab RPM group.
    SyncGitlab(SyncGitlabArgs),
    /// Close open release-monitoring bugs as CANTFIX for any
    /// inventoried package that is retired on a dist-git branch.
    TriageRetired(TriageRetiredArgs),
    /// Triage open release-monitoring bugs for inventoried
    /// packages by bumping their Bugzilla priority to match the
    /// inventory.
    TriageUpdates(TriageUpdatesArgs),
    /// Stop keeping packages: over a saved dependency graph,
    /// report what the removal frees, and optionally edit the
    /// inventories.
    Unkeep(UnkeepArgs),
    /// Validate inventory consistency.
    Validate,
}

#[derive(clap::Args)]
struct PruneRetiredArgs {
    /// Active branch(es) to check against (CSV or repeated;
    /// e.g. `rawhide,f44,epel9`). Default: queried from Bodhi's
    /// active releases, plus rawhide.
    #[arg(long, value_delimiter = ',', value_name = "BRANCH,...")]
    branch: Vec<String>,

    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Parallel dist-git queries.
    #[arg(short = 'j', long, default_value = "8")]
    jobs: usize,

    /// Preview without modifying the inventory.
    #[arg(long)]
    dry_run: bool,

    /// Delete matched packages from the inventory instead of
    /// marking them `unshipped` (the default; marking survives
    /// re-syncs and lets triage-retired keep closing bugs).
    #[arg(long, conflicts_with = "dry_run")]
    remove: bool,

    /// Skip the confirmation prompt.
    #[arg(short, long)]
    yes: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct TriageRetiredArgs {
    /// Dist-git branch(es) to check retirement against (CSV or
    /// repeated; e.g. `rawhide`, `epel10`, `f43`). Each branch
    /// scopes its own Bugzilla search — a `rawhide` retirement
    /// closes the Fedora/rawhide bug, an `epel9` retirement closes
    /// the Fedora EPEL/epel9 bug. A package retired on one branch
    /// but live on another only has bugs closed where it's dead.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "BRANCH,...",
        default_value = "rawhide"
    )]
    branch: Vec<String>,

    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Batch mode: one Bugzilla query for bugs assigned to or
    /// CC'ing EMAIL (default: the configured email), matched
    /// against the inventory locally. Much faster on a large
    /// inventory, but misses bugs where EMAIL is neither
    /// assignee nor CC'd.
    #[arg(long, value_name = "EMAIL", num_args = 0..=1)]
    batch: Option<Option<String>>,

    /// Close ALL open bugs on retired branches, not just
    /// release-monitoring (Anitya) bugs. Use with care: this
    /// closes human-filed bugs (CVEs, FTBFS, etc.) too.
    #[arg(long)]
    all_reporters: bool,

    /// Record results in the inventory's `retired_on` markers
    /// (adds and removes; needs a single -i file).
    #[arg(long, conflicts_with = "dry_run")]
    mark: bool,

    /// Bugzilla API key (or set BUGZILLA_API_KEY env var, or
    /// run `poi-tracker config`).
    #[arg(long, env = "BUGZILLA_API_KEY")]
    api_key: Option<String>,

    /// Also set `assigned_to` on each closed bug to the
    /// Bugzilla email set via `poi-tracker config`. Interactive
    /// mode prompts; with `-y` this flag is the only way to
    /// claim.
    #[arg(long)]
    claim: bool,

    /// Preview closures without applying them.
    #[arg(long)]
    dry_run: bool,

    /// Skip the confirmation prompt.
    #[arg(short, long)]
    yes: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct TriageUpdatesArgs {
    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Batch mode: one Bugzilla query for bugs assigned to or
    /// CC'ing EMAIL (default: the configured email), matched
    /// against the inventory locally. Much faster on a large
    /// inventory, but misses bugs where EMAIL is neither
    /// assignee nor CC'd.
    #[arg(long, value_name = "EMAIL", num_args = 0..=1)]
    batch: Option<Option<String>>,

    /// Close partially-addressed bugs without asking.
    #[arg(long, conflicts_with = "skip_stale")]
    close_stale: bool,

    /// Skip the Bodhi check for already-built updates.
    #[arg(long)]
    skip_stale: bool,

    /// Bugzilla API key (or set BUGZILLA_API_KEY env var, or
    /// run `poi-tracker config`).
    #[arg(long, env = "BUGZILLA_API_KEY")]
    api_key: Option<String>,

    /// Also set `assigned_to` on each closed bug to the
    /// Bugzilla email set via `poi-tracker config`. Interactive
    /// mode prompts; with `-y` this flag is the only way to
    /// claim.
    #[arg(long)]
    claim: bool,

    /// Preview updates without applying them.
    #[arg(long)]
    dry_run: bool,

    /// Skip the confirmation prompt.
    #[arg(short, long)]
    yes: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

/// Package filters shared by the inventory-walking commands. The
/// filters compose: a package must match the pattern AND fall
/// inside the `[start-from, end-with]` range.
#[derive(clap::Args, Default)]
struct WalkFilterArgs {
    /// Only process packages matching this glob (e.g. `rust-*`;
    /// a bare name matches exactly). Comma-separated or
    /// repeated; default: all packages.
    #[arg(long, value_delimiter = ',', value_name = "GLOB,...")]
    pattern: Vec<String>,

    /// Resume from this package onwards (inclusive), in the
    /// inventory's iteration order.
    #[arg(long, value_name = "NAME")]
    start_from: Option<String>,

    /// Stop after this package (inclusive). Combine with
    /// `--start-from` to bound a sub-range.
    #[arg(long, value_name = "NAME")]
    end_with: Option<String>,
}

impl WalkFilterArgs {
    /// Whether `name` passes every configured filter.
    fn matches(&self, name: &str) -> bool {
        matches_any_pattern(name, &self.pattern)
            && self.start_from.as_deref().is_none_or(|s| name >= s)
            && self.end_with.as_deref().is_none_or(|e| name <= e)
    }
}

#[derive(clap::Args)]
struct SemverAuditArgs {
    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Batch mode: one Bugzilla query for bugs assigned to or
    /// CC'ing EMAIL (default: the configured email), matched
    /// against the inventory locally. Much faster on a large
    /// inventory, but misses bugs where EMAIL is neither
    /// assignee nor CC'd.
    #[arg(long, value_name = "EMAIL", num_args = 0..=1)]
    batch: Option<Option<String>>,

    /// Show only non-breaking updates.
    #[arg(long)]
    non_breaking: bool,

    /// Output as JSON instead of human-readable text.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct IntersectArgs {
    /// Inventory file(s) to intersect with: only packages of the
    /// main inventory also found in one of these survive
    /// (repeat the flag per file).
    #[arg(long, required = true)]
    with: Vec<String>,

    /// Merge the intersection into this inventory TOML file
    /// (accumulates; existing entries win).
    #[arg(short, long)]
    output: Option<String>,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct KondoArgs {
    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Essential inventory file(s): packages found in any of these
    /// are never cull candidates (repeat the flag per file).
    #[arg(long, required = true)]
    essential: Vec<String>,

    /// dist-git username whose access level routes each action.
    #[arg(long)]
    user: String,

    /// Default inventory for 'explain': Enter at the explanation
    /// prompt files the package here.
    #[arg(long, value_name = "PATH")]
    explain_into: Option<String>,

    /// Keep every candidate without prompting.
    #[arg(short, long)]
    yes: bool,

    /// Ignore the day-fresh ACL cache and look every level up again.
    #[arg(long)]
    refresh_acls: bool,

    /// Reason recorded in the -o cull file for packages culled this
    /// run (the access level is always appended). A note typed at
    /// the prompt as `c <note>` overrides it per package.
    #[arg(long)]
    reason: Option<String>,

    /// Merge the culled set into this inventory TOML file
    /// (accumulates across passes).
    #[arg(short, long)]
    output: Option<String>,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

/// The maintenance loop as one command. Needs a workspace file (`-w`,
/// or `./kondo.toml`). For every closure in it: keeps the graph has
/// never walked are walked (fedrq only for what the graph lacks) and
/// merged in; the derived inventory is recomputed; each package that
/// newly rides in it can be made essential in its own right instead;
/// each keep only devel-only edges carry can be demoted to the derived
/// inventory. Then the owned packages no essential inventory justifies
/// go through kondo's keep/explain/remove triage into the cull file.
/// Every answer is written as it is given, so an interrupted run
/// resumes where it stopped; Enter takes the default (ride, keep),
/// `a` takes it for the rest, `q` stops asking. `--yes` takes every
/// default without asking; `--json` reports and asks nothing.
#[derive(clap::Args)]
struct ReconcileArgs {
    /// Essential inventory that promoted packages are filed into
    /// (default: the closure's first keeps inventory).
    #[arg(long, value_name = "PATH")]
    into: Option<String>,

    /// Take every default without asking (ride, keep essential),
    /// and leave the triage candidates as cull candidates.
    #[arg(short, long)]
    yes: bool,

    /// Ignore the day-fresh ACL cache in the triage.
    #[arg(long)]
    refresh_acls: bool,

    /// Compute and report only: write no file, walk nothing, ask
    /// nothing (new keeps are listed instead of walked).
    #[arg(long)]
    dry_run: bool,

    /// Report as JSON; asks and triages nothing (files are still
    /// written unless --dry-run).
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

/// Where a dependency walk looks and what it collects — shared by
/// `deps` (a full walk) and `keep` (an incremental one).
#[derive(clap::Args)]
struct WalkRepoArgs {
    /// fedrq branch (e.g. hs.el9, epel9).
    #[arg(short, long)]
    branch: String,

    /// fedrq repo class (e.g. stack).
    #[arg(short, long)]
    repo: Option<String>,

    /// Collect providers from these repo ids (CSV or repeated).
    ///
    /// Exact match. The default suits an EPEL walk; for a Fedora
    /// walk pass the branch's own repo id, e.g. `--from rawhide`.
    #[arg(long, value_delimiter = ',', default_value = "epel")]
    from: Vec<String>,

    /// Base-distro repo id prefix(es); their providers end the
    /// walk.
    ///
    /// The default treats CentOS Stream as the given beneath an
    /// EPEL stack. A Fedora walk has no base distro — the default
    /// then never matches, and everything resolves within --from's
    /// repos, which is what you want.
    #[arg(long, value_delimiter = ',', default_value = "fedrq-centos-stream-")]
    base_repo: Vec<String>,

    /// Walk runtime dependencies only, without seeding the
    /// roots' BuildRequires.
    #[arg(long)]
    runtime_only: bool,
}

#[derive(clap::Args)]
struct KeepArgs {
    /// Packages to start keeping.
    #[arg(required = true, value_name = "PACKAGE")]
    names: Vec<String>,

    /// Keep inventory to add them to (default: the first -i).
    #[arg(long, value_name = "PATH")]
    into: Option<String>,

    /// Saved dependency graph to extend (from `deps`); the walk
    /// resolves offline whatever it already knows.
    #[arg(long, value_name = "PATH")]
    graph: String,

    /// Inventory of the packages you own — newly reached owned
    /// packages become fixpoint roots and derived entries.
    #[arg(long, value_name = "PATH")]
    owned: String,

    /// Derived inventory to recompute afterwards (optional).
    #[arg(long, value_name = "PATH")]
    deps: Option<String>,

    #[command(flatten)]
    walk: WalkRepoArgs,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct DepsArgs {
    #[command(flatten)]
    filter: WalkFilterArgs,

    #[command(flatten)]
    walk: WalkRepoArgs,

    /// Where to write the walk's full dependency graph (every
    /// requirer/provider edge, not just first attributions) as
    /// JSON. Default: beside -o, as <output>-graph.json.
    #[arg(long, value_name = "PATH")]
    graph: Option<String>,

    /// Iterate to a fixpoint: collected packages found in this
    /// inventory (yours) become roots whose BuildRequires seed
    /// further rounds.
    #[arg(long, value_name = "PATH", conflicts_with = "runtime_only")]
    fixpoint: Option<String>,

    /// Write the collected dependencies as an inventory TOML file.
    #[arg(short, long)]
    output: Option<String>,

    /// Inventory name for --output
    /// (default: "<input name>-deps-<branch>").
    #[arg(long)]
    name: Option<String>,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct ActArgs {
    /// FAS username whose ACL actions these are.
    #[arg(long)]
    user: String,

    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Personal inventory to drop enacted packages from.
    #[arg(long, value_name = "PATH")]
    personal: Option<String>,

    /// Branch(es) for the reverse-dependency probe before an orphan
    /// (CSV or repeated). The default `auto` derives them per package
    /// from its dist-git branches: rawhide plus its own EPEL
    /// branches — an EPEL-only package's dependents are invisible
    /// from rawhide.
    #[arg(long, value_delimiter = ',', default_value = "auto")]
    branch: Vec<String>,

    /// Dist-git API token (or PAGURE_API_TOKEN, or run
    /// `poi-tracker config`).
    #[arg(long, env = "PAGURE_API_TOKEN")]
    api_token: Option<String>,

    /// Ignore the day-fresh ACL cache and look every level up again.
    #[arg(long)]
    refresh_acls: bool,

    /// Log each access lookup.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct AnnounceArgs {
    /// FAS username whose access level routes each action.
    #[arg(long)]
    user: String,

    /// Ignore the day-fresh ACL cache and look every level up again.
    #[arg(long)]
    refresh_acls: bool,

    /// Log each access lookup.
    #[arg(short, long)]
    verbose: bool,

    /// Output machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct DependentsArgs {
    /// Dependency graph JSON from a `deps --graph` run.
    #[arg(long, value_name = "PATH")]
    graph: String,

    /// Output machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct DeriveArgs {
    /// Dependency graph JSON from a `deps --graph` run.
    #[arg(long, value_name = "PATH")]
    graph: String,

    /// Inventory of the packages you own (e.g. a sync-distgit
    /// output); only owned packages enter the derived inventory.
    #[arg(long, value_name = "PATH")]
    owned: String,

    /// The derived inventory to recompute.
    #[arg(short, long, value_name = "PATH")]
    output: String,

    /// Replace the output file's packages; without it, report only.
    #[arg(long)]
    apply: bool,

    /// Output machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct UnkeepArgs {
    /// Packages to stop keeping.
    #[arg(required = true, value_name = "PACKAGE")]
    names: Vec<String>,

    /// Dependency graph JSON from a `deps --graph` run.
    #[arg(long, value_name = "PATH")]
    graph: String,

    /// Derived dependency inventories (a `deps` output merged by
    /// `intersect`, say); freed packages leave these with --apply.
    #[arg(long, value_name = "PATH")]
    deps: Vec<String>,

    /// Edit the inventories; without it, report only.
    #[arg(long)]
    apply: bool,

    /// Output machine-readable JSON.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct AddArgs {
    /// Source RPM name.
    name: String,

    /// Point of contact ("Name <email>").
    #[arg(long)]
    poc: Option<String>,

    /// Reason for tracking.
    #[arg(long)]
    reason: Option<String>,

    /// Team responsible.
    #[arg(long)]
    team: Option<String>,

    /// Internal task/ticket reference.
    #[arg(long)]
    task: Option<String>,

    /// Binary RPM subpackage(s) to track (comma-separated or repeated).
    #[arg(long, value_delimiter = ',')]
    rpm: Vec<String>,

    /// Workload tag(s) (comma-separated or repeated).
    #[arg(long, value_delimiter = ',')]
    workload: Vec<String>,

    /// Track branch for hs-relmon (e.g. upstream, fedora-rawhide).
    #[arg(long)]
    track: Option<String>,
}

#[derive(clap::Args)]
struct AdoptArgs {
    #[command(flatten)]
    filter: WalkFilterArgs,

    /// Dist-git API token with the `modify_project` ACL (or set
    /// PAGURE_API_TOKEN, or run `poi-tracker config`).
    #[arg(long, env = "PAGURE_API_TOKEN")]
    api_token: Option<String>,

    /// Preview orphaned packages without adopting them.
    #[arg(long)]
    dry_run: bool,

    /// Adopt every orphaned match without per-package prompts.
    #[arg(short, long)]
    yes: bool,

    /// Print progress to stderr.
    #[arg(short, long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct FindArgs {
    /// Source RPM name to search for.
    name: String,
}

#[derive(clap::Args)]
struct RemoveArgs {
    /// Source RPM name to remove.
    name: String,

    /// Remove specific binary RPM(s) instead of the whole package.
    #[arg(long, value_delimiter = ',')]
    rpm: Vec<String>,
}

#[derive(clap::Args)]
struct ShowArgs {
    /// Filter by workload.
    #[arg(long)]
    workload: Option<String>,

    /// Output as JSON instead of human-readable.
    #[arg(long)]
    json: bool,
}

#[derive(clap::Args)]
struct ExportArgs {
    #[command(subcommand)]
    format: ExportFormat,
}

#[derive(Subcommand)]
enum ExportFormat {
    /// Export as content-resolver YAML.
    ContentResolver {
        /// Export only this workload.
        #[arg(long)]
        workload: Option<String>,
        /// Output file (default: {workload-name}.yaml).
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Export as hs-relmon manifest TOML.
    HsRelmon {
        /// Filter by workload.
        #[arg(long)]
        workload: Option<String>,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,

        /// Default distros list.
        #[arg(long, default_value = "upstream,fedora,centos,hyperscale")]
        distros: String,

        /// Default tracking branch.
        #[arg(long, default_value = "upstream")]
        track: String,

        /// Remove manifest entries not in the inventory.
        #[arg(long)]
        prune: bool,
    },
}

#[derive(clap::Args)]
struct ImportArgs {
    /// Path to legacy JSON inventory file.
    json_file: String,

    /// Output path for TOML inventory.
    #[arg(short, long, default_value = "inventory.toml")]
    output: String,

    /// Fields to mark as private (stripped on export).
    #[arg(long, value_delimiter = ',', value_name = "FIELD,...")]
    private_fields: Vec<String>,

    /// Workload tag(s) to apply to all imported packages.
    #[arg(long, value_delimiter = ',', value_name = "WORKLOAD,...")]
    workload: Vec<String>,
}

#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("source")
        .required(true)
        .args(["user", "group"])
))]
struct SyncDistgitArgs {
    /// Import packages for this dist-git user.
    #[arg(long)]
    user: Option<String>,

    /// Import packages for this dist-git group.
    #[arg(long)]
    group: Option<String>,

    /// Output TOML file.
    #[arg(short, long, default_value = "inventory.toml")]
    output: String,

    /// One owner-alias request instead of a prefix scan.
    /// Direct owner/admin/commit only: collaborator/ticket
    /// grants are missed (and removed under --prune). Implies
    /// --no-groups.
    #[arg(
        long,
        conflicts_with_all = ["group", "include_group", "exclude_group",
                              "auto_prefix", "no_auto_prefix",
                              "start_pattern", "end_pattern"]
    )]
    fast: bool,

    /// Exclude group-only access.
    #[arg(
        long,
        conflicts_with_all = ["include_group", "exclude_group"]
    )]
    no_groups: bool,

    /// Keep only these groups (CSV or repeated).
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "GROUP,...",
        conflicts_with = "exclude_group"
    )]
    include_group: Vec<String>,

    /// Drop these groups (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "GROUP,...")]
    exclude_group: Vec<String>,

    /// Exclude packages by glob (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "GLOB,...")]
    exclude: Vec<String>,

    /// Name pattern for a single patterned query.
    #[arg(
        long,
        conflicts_with_all = ["auto_prefix", "start_pattern", "end_pattern"]
    )]
    pattern: Option<String>,

    /// Start the prefix scan at this prefix.
    #[arg(long, value_name = "PREFIX")]
    start_pattern: Option<String>,

    /// Stop the prefix scan before this prefix.
    #[arg(long, value_name = "PREFIX")]
    end_pattern: Option<String>,

    /// Query by a-z/0-9 prefix (--user default).
    #[arg(long, overrides_with = "no_auto_prefix")]
    auto_prefix: bool,

    /// Single query; may 504 for a --user sync.
    #[arg(
        long,
        overrides_with = "auto_prefix",
        conflicts_with_all = ["start_pattern", "end_pattern"]
    )]
    no_auto_prefix: bool,

    /// Remove packages no longer in dist-git results.
    #[arg(long)]
    prune: bool,

    /// Mark packages added by this sync that are already
    /// retired everywhere (like prune-retired).
    #[arg(long)]
    mark_unshipped: bool,

    /// Parallel dist-git queries for --mark-unshipped.
    #[arg(short = 'j', long, default_value = "8")]
    jobs: usize,

    /// Pagure API page size.
    #[arg(long, default_value = "100")]
    per_page: u32,

    /// Workload tags (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "WORKLOAD,...")]
    workload: Vec<String>,

    /// Inventory name (default: user/group).
    #[arg(long)]
    name: Option<String>,
}

/// Well-known GitLab RPM group presets.
const GITLAB_PRESETS: &[(&str, &str)] = &[
    ("hyperscale", "https://gitlab.com/CentOS/Hyperscale/rpms"),
    (
        "proposed-updates",
        "https://gitlab.com/CentOS/proposed_updates/rpms",
    ),
    (
        "centos-stream",
        "https://gitlab.com/redhat/centos-stream/rpms",
    ),
];

#[derive(clap::Args)]
#[command(group(
    clap::ArgGroup::new("source")
        .required(true)
        .args(["url", "preset"])
))]
struct SyncGitlabArgs {
    /// GitLab group URL.
    #[arg(long)]
    url: Option<String>,

    /// Preset: hyperscale, proposed-updates, centos-stream.
    #[arg(long)]
    preset: Option<String>,

    /// Output TOML file.
    #[arg(short, long, default_value = "inventory.toml")]
    output: String,

    /// Exclude packages by glob (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "GLOB,...")]
    exclude: Vec<String>,

    /// Remove packages no longer in GitLab results.
    #[arg(long)]
    prune: bool,

    /// Workload tags (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "WORKLOAD,...")]
    workload: Vec<String>,

    /// Mark archived projects with no released CBS build as
    /// unshipped (Hyperscale: RHEL or Stream; Proposed Updates:
    /// Stream only).
    #[arg(long)]
    mark_unshipped: bool,

    /// CentOS releases to count as currently shipped (CSV).
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "REL,...",
        default_value = "9,10"
    )]
    centos_release: Vec<u32>,

    /// Inventory name (default: derived from group).
    #[arg(long)]
    name: Option<String>,
}

/// Command outcome: hard failures bubble up as an error (printed
/// as `error: {e}` with a FAILURE exit by `main`); soft outcomes
/// already reported inline (e.g. partial failures, "not found")
/// pick their own `ExitCode`.
type CmdResult = Result<ExitCode, Box<dyn std::error::Error>>;

/// Derive a YAML filename for a workload export.
fn workload_export_filename(
    inventory: &sandogasa_inventory::Inventory,
    workload_key: &str,
) -> String {
    let meta = inventory.inventory.workloads.get(workload_key);
    let name = meta
        .and_then(|m| m.name.as_deref())
        .map(|n| n.to_string())
        .unwrap_or_else(|| format!("{}-{workload_key}", inventory.inventory.name));
    format!("{}.yaml", name.replace(' ', "_"))
}

/// Build a workloads map from a list of workload names.
fn workloads_from_names(names: &[String]) -> BTreeMap<String, sandogasa_inventory::WorkloadMeta> {
    names
        .iter()
        .map(|n| (n.clone(), sandogasa_inventory::WorkloadMeta::default()))
        .collect()
}

/// The real Koji tag lookup backing the shipped-build checks in
/// `triage-updates` and `semver-audit`: latest NVR of a package
/// in a tag's inheritance chain. `None` when the koji CLI isn't
/// on PATH — callers warn and degrade (fail-safe: unverifiable
/// builds count as not shipped / not verified).
fn koji_tag_lookup() -> Option<impl Fn(&str, &str) -> Result<Option<String>, String>> {
    sandogasa_koji::is_available().then_some(|tag: &str, package: &str| {
        sandogasa_koji::latest_tagged(tag, package, None).map(|b| b.map(|tb| tb.nvr))
    })
}

/// Collect inventory paths from -i and -I flags.
/// Resolve a `--batch [EMAIL]` flag: an explicit email wins, a
/// bare `--batch` falls back to the configured Bugzilla email.
fn resolve_batch_email(batch: &Option<Option<String>>) -> Result<Option<String>, String> {
    match batch {
        None => Ok(None),
        Some(Some(email)) => Ok(Some(email.clone())),
        Some(None) => config::resolve_email().map(Some).ok_or_else(|| {
            "--batch needs an email: none configured (run `poi-tracker \
             config`) and none passed (--batch <email>)"
                .to_string()
        }),
    }
}

fn resolve_inventory_paths(cli: &Cli) -> Vec<String> {
    let mut paths = cli.inventory.clone();

    for dir in &cli.inventory_dir {
        if let Ok(entries) = std::fs::read_dir(dir) {
            let mut dir_paths: Vec<String> = entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "toml"))
                .map(|e| e.path().to_string_lossy().to_string())
                .collect();
            dir_paths.sort();
            paths.extend(dir_paths);
        } else {
            eprintln!("warning: could not read directory: {dir}");
        }
    }

    paths
}

fn main() -> ExitCode {
    sandogasa_cli::init();
    // `-C DIR` first, before anything looks at a relative path — the
    // way git does it: scanned off argv so the workspace lookup inside
    // the parse already runs from there.
    if let Some(dir) = directory_flag(std::env::args_os())
        && let Err(e) = std::env::set_current_dir(&dir)
    {
        eprintln!("error: cannot change to {}: {e}", dir.to_string_lossy());
        return ExitCode::FAILURE;
    }
    let cli = sandogasa_cli::parse_with_defaults_and::<Cli>(env!("CARGO_PKG_NAME"), |m| {
        let explicit = m.get_one::<String>("workspace").map(String::as_str);
        let Some((ws, path)) = workspace::Workspace::find(explicit)? else {
            return Ok(None);
        };
        let Some((sub, _)) = m.subcommand() else {
            return Ok(None);
        };
        let closure = m.get_one::<String>("closure").map(String::as_str);
        Ok(ws
            .defaults_for(sub, closure)?
            .map(|table| (table, format!("workspace {}", path.display()))))
    });

    // Import/sync commands produce new files and don't need existing
    // inventory paths. `Config` doesn't touch inventories at all.
    let needs_paths = !matches!(
        cli.command,
        Command::Config
            | Command::Import(_)
            | Command::Reconcile(_)
            | Command::SyncDistgit(_)
            | Command::SyncGitlab(_)
    );

    let paths = resolve_inventory_paths(&cli);

    if needs_paths && paths.is_empty() {
        eprintln!("error: no inventory files specified. Use -i or -I.");
        return ExitCode::FAILURE;
    }

    let result = match &cli.command {
        Command::Add(args) => cmd_add(&paths, args),
        Command::Act(args) => cmd_act(&paths, args),
        Command::Adopt(args) => cmd_adopt(&paths, args),
        Command::Announce(args) => cmd_announce(&paths, args),
        Command::Config => cmd_config(),
        Command::Deps(args) => cmd_deps(&paths, args),
        Command::Export(args) => cmd_export(&paths, args),
        Command::Find(args) => cmd_find(&paths, args),
        Command::Import(args) => cmd_import(args),
        Command::Intersect(args) => cmd_intersect(&paths, args),
        Command::Dependents(args) => cmd_dependents(&paths, args),
        Command::Derive(args) => cmd_derive(&paths, args),
        Command::Keep(args) => cmd_keep(&paths, args),
        Command::Kondo(args) => cmd_kondo(&paths, args),
        Command::Reconcile(args) => cmd_reconcile(cli.workspace.as_deref(), args),
        Command::PruneRetired(args) => cmd_prune_retired(&paths, args),
        Command::Remove(args) => cmd_remove(&paths[0], args),
        Command::SemverAudit(args) => cmd_semver_audit(&paths, args),
        Command::Show(args) => cmd_show(&paths, args),
        Command::SyncDistgit(args) => cmd_sync_distgit(args),
        Command::SyncGitlab(args) => cmd_sync_gitlab(args),
        Command::TriageRetired(args) => cmd_triage_retired(&paths, args),
        Command::TriageUpdates(args) => cmd_triage_updates(&paths, args),
        Command::Unkeep(args) => cmd_unkeep(&paths, args),
        Command::Validate => cmd_validate(&paths),
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Create a Tokio runtime, prefixing the (rare) failure so the
/// boundary's `error: {e}` matches the old inline message.
fn new_runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Runtime::new().map_err(|e| format!("failed to create runtime: {e}"))
}

fn cmd_semver_audit(paths: &[String], args: &SemverAuditArgs) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    // Read-only: anonymous Bugzilla search + public dist-git.
    let bz = sandogasa_bugzilla::BzClient::new(&config::resolve_url());
    let dg = sandogasa_distgit::DistGitClient::new();
    let rt = new_runtime()?;
    let batch_email = resolve_batch_email(&args.batch)?;
    let koji_lookup = koji_tag_lookup();
    let latest_tagged = match &koji_lookup {
        Some(l) => Some(l as &triage_updates::TagLookup),
        None => {
            eprintln!(
                "warning: koji CLI not found; cannot tell stale bugs from \
                 committed-but-unreleased versions — reporting them all as \
                 up to date. Install koji to enable the check."
            );
            None
        }
    };
    let entries = rt.block_on(semver_audit::run(
        &inventory,
        &bz,
        &dg,
        latest_tagged,
        &args.filter,
        args.non_breaking,
        batch_email.as_deref(),
        args.verbose,
    ))?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else {
        semver_audit::print_report(&entries);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_prune_retired(paths: &[String], args: &PruneRetiredArgs) -> CmdResult {
    // Pruning rewrites the inventory, which only makes sense for
    // a single file; --dry-run may preview a merged view.
    if !args.dry_run && paths.len() != 1 {
        return Err(format!(
            "prune-retired modifies the inventory and needs \
             exactly one inventory file (got {}); use --dry-run to \
             preview a merged view",
            paths.len()
        )
        .into());
    }
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    let dg = sandogasa_distgit::DistGitClient::new();
    let rt = new_runtime()?;

    // The active branch set defines "carried anywhere": explicit
    // --branch wins (keeping the user's order), otherwise ask
    // Bodhi for the active releases, ordered most-likely-live
    // first so per-package checks short-circuit early.
    let active: Vec<String> = if !args.branch.is_empty() {
        args.branch.clone()
    } else {
        rt.block_on(prune_retired::active_branches_from_bodhi())?
    };
    if args.verbose {
        eprintln!("[poi-tracker] active branches: {}", active.join(", "));
    }

    let report = rt.block_on(prune_retired::run(
        &inventory,
        &dg,
        &active,
        &args.filter,
        args.jobs,
        args.verbose,
    ))?;

    if !report.candidates.is_empty() {
        println!("Packages no longer carried on any active branch:");
        for c in &report.candidates {
            println!("- {}: {}", c.package, c.reason.describe());
        }
    }
    if !report.invalid.is_empty() {
        println!(
            "\nInvalid entries — no such dist-git package (fix or \
             remove; often a non-rpms project imported by an older \
             sync, or a binary subpackage name recorded instead of \
             the source package):"
        );
        for name in &report.invalid {
            println!("- {name}");
        }
    }
    eprintln!(
        "\n{} checked, {} prunable, {} invalid",
        report.packages_checked,
        report.candidates.len(),
        report.invalid.len()
    );
    if args.dry_run {
        return Ok(ExitCode::SUCCESS);
    }

    // Apply to the single inventory file. Default: update the
    // `unshipped` markers (both directions — clears marks on
    // revived packages). --remove deletes the entries instead.
    let path = &paths[0];
    let mut inv = sandogasa_inventory::load(path).map_err(|e| format!("reloading {path}: {e}"))?;
    if args.remove {
        if report.candidates.is_empty() {
            eprintln!("nothing to remove");
            return Ok(ExitCode::SUCCESS);
        }
        if !args.yes
            && !triage_updates::confirm(&format!(
                "Remove {} package(s) from {path}?",
                report.candidates.len()
            ))?
        {
            eprintln!("aborted: inventory not modified");
            return Ok(ExitCode::SUCCESS);
        }
        let mut removed = 0usize;
        for c in &report.candidates {
            if inv.remove_package(&c.package) {
                removed += 1;
            }
        }
        sandogasa_inventory::save(&inv, path).map_err(|e| format!("saving {path}: {e}"))?;
        eprintln!("removed {removed} package(s) from {path}");
        return Ok(ExitCode::SUCCESS);
    }

    let changed =
        prune_retired::apply_unshipped_marks(&mut inv, &report.checked, &report.candidates);
    if changed == 0 {
        eprintln!("unshipped markers already up to date");
        return Ok(ExitCode::SUCCESS);
    }
    if !args.yes
        && !triage_updates::confirm(&format!(
            "Update unshipped markers on {changed} package(s) in {path}?"
        ))?
    {
        eprintln!("aborted: inventory not modified");
        return Ok(ExitCode::SUCCESS);
    }
    sandogasa_inventory::save(&inv, path).map_err(|e| format!("saving {path}: {e}"))?;
    eprintln!("updated unshipped markers on {changed} package(s) in {path}");
    Ok(ExitCode::SUCCESS)
}

fn cmd_triage_retired(paths: &[String], args: &TriageRetiredArgs) -> CmdResult {
    // --mark writes results back, which only makes sense for a
    // single inventory file (a merged view has no single home).
    if args.mark && paths.len() != 1 {
        return Err(format!(
            "--mark needs exactly one inventory file (got {})",
            paths.len()
        )
        .into());
    }
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    let api_key = config::resolve_api_key(args.api_key.as_deref())?;
    let url = config::resolve_url();
    let bz = sandogasa_bugzilla::BzClient::new(&url).with_api_key(api_key)?;
    let dg = sandogasa_distgit::DistGitClient::new();

    let claim_email = config::resolve_email();
    if args.claim && claim_email.is_none() {
        return Err("--claim needs a configured Bugzilla email.\n\
             Set it with: poi-tracker config"
            .into());
    }

    let batch_email = resolve_batch_email(&args.batch)?;
    let rt = new_runtime()?;
    let report = rt.block_on(triage_retired::run(
        &inventory,
        &bz,
        &dg,
        &args.branch,
        args.all_reporters,
        &args.filter,
        batch_email.as_deref(),
        args.claim,
        claim_email.as_deref(),
        args.dry_run,
        args.yes,
        args.verbose,
    ))?;
    eprintln!(
        "\n{} checked, {} retired, {} planned, {} closed, {} failed",
        report.packages_checked,
        report.packages_retired,
        report.closes_planned,
        report.closes_applied,
        report.failures
    );
    // Record the retirement checks in the inventory. The facts
    // were gathered regardless of whether any bug closures were
    // confirmed, so marking is independent of the close outcome.
    if args.mark {
        let path = &paths[0];
        let mut inv = sandogasa_inventory::load(path)
            .map_err(|e| format!("reloading {path} for --mark: {e}"))?;
        let changed = triage_retired::apply_retirement_marks(&mut inv, &report.checks);
        if changed > 0 {
            sandogasa_inventory::save(&inv, path).map_err(|e| format!("saving {path}: {e}"))?;
            eprintln!("marked {changed} package(s) in {path}");
        } else {
            eprintln!("retirement markers already up to date");
        }
    }
    Ok(if report.failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_intersect(paths: &[String], args: &IntersectArgs) -> CmdResult {
    let base = sandogasa_inventory::load_and_merge(paths)?;
    // Names only — reading the filter files independently avoids
    // load_and_merge's per-package reason-conflict warnings, which
    // are meaningless for a name filter.
    let mut filter: std::collections::BTreeSet<String> = Default::default();
    for path in &args.with {
        filter.extend(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name),
        );
    }
    let meta = base.inventory.clone();
    let packages = intersect::intersect(base, &filter);

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "packages": packages.iter().map(|p| &p.name).collect::<Vec<_>>(),
                "count": packages.len(),
                "output": args.output,
            }))?
        );
        if let Some(path) = &args.output {
            intersect::merge_packages(path, packages, &meta)?;
        }
        return Ok(ExitCode::SUCCESS);
    }

    println!("{} package(s) in the intersection", packages.len());
    for pkg in &packages {
        match &pkg.reason {
            Some(reason) => println!("  {} — {}", pkg.name, reason),
            None => println!("  {}", pkg.name),
        }
    }
    if let Some(path) = &args.output {
        let added = intersect::merge_packages(path, packages, &meta)?;
        println!("merged into {path} ({added} new)");
    }
    Ok(ExitCode::SUCCESS)
}

/// Walk the cull inventory's verdicts (`-i`) and enact them one
/// confirmed action at a time. Interactive only — this changes
/// ownership on a server, so there is no bulk mode.
fn cmd_act(paths: &[String], args: &ActArgs) -> CmdResult {
    use std::io::{IsTerminal, Write};

    if !std::io::stdin().is_terminal() {
        return Err("act is interactive-only: each ACL change needs a confirmation".into());
    }
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let token = config::resolve_distgit_token(args.api_token.as_deref())?;

    let mut names: Vec<String> = Vec::new();
    for path in paths {
        names.extend(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name),
        );
    }
    names.sort();
    names.dedup();
    names.retain(|name| args.filter.matches(name));
    let (culled, warnings) =
        classify_with_cache(&args.user, &names, args.refresh_acls, args.verbose)?;
    for warning in &warnings {
        eprintln!("warning: {warning}");
    }

    let client = DistGitClient::new().with_token(token);
    let rt = new_runtime()?;
    // Fail in seconds on a bad token rather than at the first give,
    // deep into the walk. A valid token can still lack the ACL scope
    // for changing ownership; the per-action errors now carry
    // Pagure's own message when that happens.
    match rt.block_on(client.verify_token()) {
        Ok(who) => eprintln!("authenticated as {who}"),
        Err(e) => {
            return Err(format!(
                "dist-git token check failed: {e}\n\
                 orphaning and giving need an account token with the \
                 \"Modify an existing project\" ACL — regenerate one at \
                 https://src.fedoraproject.org/settings#nav-api-tab"
            )
            .into());
        }
    }
    let total = culled.len();
    let mut enacted: Vec<String> = Vec::new();
    let mut skipped = 0usize;
    let mut unculled = 0usize;
    let read_line = || -> Result<String, String> {
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("reading input: {e}"))?;
        Ok(line)
    };
    'walk: for (i, c) in culled.iter().enumerate() {
        let idx = i + 1;
        let orphanable = matches!(c.action, kondo::Action::Orphan);
        let actionable = orphanable || matches!(c.action, kondo::Action::SelfRemove);
        println!("[{idx}/{total}] {} ({})", c.name, c.level);
        if !actionable {
            println!("  nothing to self-enact — ask to be removed (or uncull)");
        }
        let mut hard_dependents = false;
        if orphanable {
            // Orphaning starts the retirement clock; retiring a
            // package something requires strands its dependents, so
            // look before every leap — features included, on the
            // branches this package actually lives on.
            let branches = match args.branch.as_slice() {
                [auto] if auto == "auto" => match rt.block_on(client.project_branches(&c.name)) {
                    Ok(Some(git)) => {
                        let derived = act::probe_branches(&git);
                        if derived.is_empty() {
                            println!("  no probeable branch (git has: {})", git.join(", "));
                        }
                        derived
                    }
                    Ok(None) => {
                        println!("  no such dist-git project — probing rawhide anyway");
                        vec!["rawhide".to_string()]
                    }
                    Err(e) => {
                        println!("  branch listing failed ({e}) — probing rawhide anyway");
                        vec!["rawhide".to_string()]
                    }
                },
                _ => args.branch.clone(),
            };
            for branch in &branches {
                match act::probe_dependents(branch, &c.name) {
                    Ok(act::Probe::Absent) => {
                        println!("  not present on {branch}");
                    }
                    Ok(act::Probe::Dependents(deps)) if deps.is_empty() => {
                        println!("  no {branch} dependents");
                    }
                    Ok(act::Probe::Dependents(deps)) => {
                        let rendered: Vec<String> = deps
                            .iter()
                            .map(|d| match d.feature_only {
                                true => format!("{} (optional feature)", d.source),
                                false => d.source.clone(),
                            })
                            .collect();
                        if deps.iter().all(|d| d.feature_only) {
                            println!(
                                "  {branch} dependents, all via optional features \
                                 (likely severable): {}",
                                rendered.join(", ")
                            );
                        } else {
                            hard_dependents = true;
                            println!(
                                "  {branch} dependents: {} — orphaning starts the \
                                 retirement clock for them",
                                rendered.join(", ")
                            );
                        }
                    }
                    Err(e) => {
                        hard_dependents = true;
                        println!("  {branch} probe failed ({e}) — assume it has dependents");
                    }
                }
            }
        }
        loop {
            let prompt = match (orphanable, actionable) {
                (true, _) => {
                    "  (y) orphan / (g <user>) give / (u [inventory]) uncull / (s)kip / (q)uit [s]: "
                }
                (_, true) => "  (y) remove my ACL / (u [inventory]) uncull / (s)kip / (q)uit [s]: ",
                _ => "  (u [inventory]) uncull / (s)kip / (q)uit [s]: ",
            };
            print!("{prompt}");
            let _ = std::io::stdout().flush();
            let choice = match act::parse_choice(&read_line()?, actionable, orphanable) {
                Some(choice) => choice,
                None => {
                    println!(
                        "  enter u, s, or q ('u <inventory>' also files it as essential{})",
                        match (orphanable, actionable) {
                            (true, _) => "; y enacts, g <user> gives",
                            (_, true) => "; y enacts",
                            _ => "",
                        }
                    );
                    continue;
                }
            };
            let outcome = match &choice {
                act::Choice::Skip => {
                    skipped += 1;
                    break;
                }
                act::Choice::Quit => break 'walk,
                act::Choice::Uncull(into) => {
                    if let Some(inventory) = into {
                        match kondo::file_into_inventory(inventory, &c.name, &args.user) {
                            Ok(true) => println!(
                                "  filed into {inventory} — not in the dependency graph yet: \
                                 `keep {}` records its edges",
                                c.name
                            ),
                            Ok(false) => println!("  already in {inventory}"),
                            Err(e) => {
                                println!("  could not add to {inventory}: {e} — verdict kept");
                                skipped += 1;
                                break;
                            }
                        }
                    } else {
                        println!("  unculled — returns to candidacy on the next kondo run");
                    }
                    let gone: std::collections::BTreeSet<&str> = [c.name.as_str()].into();
                    for path in paths {
                        unkeep::remove_from_inventory(path, &gone)?;
                    }
                    unculled += 1;
                    break;
                }
                act::Choice::Enact if orphanable => {
                    // A hard dependent means retirement would strand
                    // it; make the operator say so twice.
                    if hard_dependents {
                        print!("  a package still hard-depends on this — orphan anyway? [y/N] ");
                        let _ = std::io::stdout().flush();
                        if !matches!(
                            read_line()?.trim().to_ascii_lowercase().as_str(),
                            "y" | "yes"
                        ) {
                            println!("  kept");
                            skipped += 1;
                            break;
                        }
                    }
                    rt.block_on(client.give_package(&c.name, "orphan"))
                }
                act::Choice::Enact => rt.block_on(client.remove_acl(&c.name, "user", &args.user)),
                act::Choice::Give(fas) => match rt.block_on(client.user_exists(fas)) {
                    Ok(true) => rt.block_on(client.give_package(&c.name, fas)),
                    Ok(false) => {
                        println!("  no such user: {fas}");
                        continue;
                    }
                    // The user endpoint 503s for prolific packagers —
                    // exactly the people who receive packages — so an
                    // unverifiable recipient is not a blocked one:
                    // Pagure validates main_admin on the give itself.
                    Err(e) => {
                        println!(
                            "  could not verify {fas} ({e}); Pagure will reject an unknown user"
                        );
                        rt.block_on(client.give_package(&c.name, fas))
                    }
                },
            };
            match outcome {
                Ok(()) => {
                    let did = match &choice {
                        act::Choice::Give(fas) => format!("gave to {fas}"),
                        _ if orphanable => "orphaned".to_string(),
                        _ => "removed own ACL".to_string(),
                    };
                    println!("  {did}");
                    // Adjust the books immediately, so an interrupted
                    // walk loses nothing already enacted.
                    let gone: std::collections::BTreeSet<&str> = [c.name.as_str()].into();
                    for path in paths {
                        unkeep::remove_from_inventory(path, &gone)?;
                    }
                    if let Some(personal) = &args.personal {
                        unkeep::remove_from_inventory(personal, &gone)?;
                    }
                    enacted.push(c.name.clone());
                    break;
                }
                Err(e) => {
                    println!("  failed: {e} — verdict kept");
                    skipped += 1;
                    break;
                }
            }
        }
    }
    println!(
        "
enacted {} package(s), unculled {}, skipped {}; {} verdict(s) remain in the cull inventory",
        enacted.len(),
        unculled,
        skipped,
        total - enacted.len() - unculled,
    );
    Ok(ExitCode::SUCCESS)
}

/// Render the standing verdicts of a cull inventory (`-i`) as the
/// same grouped report kondo prints for a single run — the artifact
/// the act phase posts to the mailing list. Levels come from
/// re-classification (cache first), never from the reason text,
/// which is user-customizable.
fn cmd_announce(paths: &[String], args: &AnnounceArgs) -> CmdResult {
    let mut names: Vec<String> = Vec::new();
    for path in paths {
        names.extend(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name),
        );
    }
    names.sort();
    names.dedup();
    let (culled, warnings) =
        classify_with_cache(&args.user, &names, args.refresh_acls, args.verbose)?;
    let report = kondo::KondoReport {
        candidates: names.len(),
        culled,
        warnings,
        ..Default::default()
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::SUCCESS);
    }
    print!("{}", kondo::format_report(&report, &args.user));
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Classify `names` by the user's own access level: day-fresh cache
/// answers first, dist-git covers the rest, and successful lookups
/// refresh the cache. Shared by kondo (triage) and announce (render).
fn classify_with_cache(
    user: &str,
    names: &[String],
    refresh_acls: bool,
    verbose: bool,
) -> Result<(Vec<kondo::Culled>, Vec<String>), Box<dyn std::error::Error>> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut cache = kondo::AclCache::load(kondo::AclCache::default_path());
    let mut by_name: BTreeMap<String, kondo::Culled> = Default::default();
    let mut to_lookup: Vec<String> = Vec::new();
    for name in names {
        match (!refresh_acls)
            .then(|| cache.fresh(user, name, now))
            .flatten()
        {
            Some(hit) => {
                by_name.insert(name.clone(), kondo::culled_from_level(name, &hit.level));
            }
            None => to_lookup.push(name.clone()),
        }
    }
    eprintln!(
        "checking dist-git access for {} candidate(s) ({} from cache)...",
        to_lookup.len(),
        by_name.len(),
    );
    let dg = sandogasa_distgit::DistGitClient::new();
    let rt = new_runtime()?;
    let (looked_up, warnings) = rt.block_on(kondo::classify(&dg, user, &to_lookup, verbose));
    for c in looked_up {
        if c.level != "unknown" {
            cache.remember(user, &c.name, c.level.clone(), now);
        }
        by_name.insert(c.name.clone(), c);
    }
    if let Some(warning) = cache.save() {
        eprintln!("warning: {warning}");
    }
    let classified = names.iter().filter_map(|n| by_name.remove(n)).collect();
    Ok((classified, warnings))
}

fn cmd_kondo(paths: &[String], args: &KondoArgs) -> CmdResult {
    use std::io::IsTerminal;

    let personal = sandogasa_inventory::load_and_merge(paths)?;
    let (essential_paths, note) =
        kondo::essential_paths_to_load(&args.essential, args.explain_into.as_deref());
    if let Some(note) = &note {
        eprintln!("note: {note}");
    }
    // Only the names matter for a set difference, so the essential
    // files are read independently rather than merged — merging
    // warns per reason conflict, and the same crate legitimately
    // carries different reasons in an el9 deps inventory and a
    // rawhide one. On the first real run those warnings buried the
    // rescue report.
    let mut essential_names: std::collections::BTreeSet<String> = Default::default();
    for path in &essential_paths {
        essential_names.extend(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name),
        );
    }

    let mut candidates = kondo::cull_candidates(&personal, &essential_names);
    candidates.retain(|name| args.filter.matches(name));
    candidates.sort();
    // The cull inventory is the accumulated verdict — but essential
    // inputs can improve between passes (deps --build justifying a
    // crate stack), so anything on it that is now essential gets
    // rescued first, then what remains decided is not re-asked.
    let rescued = match &args.output {
        Some(path) => kondo::rescue_culled(path, &essential_names)?,
        None => Vec::new(),
    };
    if !rescued.is_empty() {
        eprintln!(
            "rescued {} package(s) from the cull list (now essential): {}",
            rescued.len(),
            rescued.join(", "),
        );
    }
    let prior = match &args.output {
        Some(path) => kondo::prior_culled(path)?,
        None => Default::default(),
    };
    let before = candidates.len();
    candidates.retain(|name| !prior.contains(name));
    let mut report = kondo::KondoReport {
        candidates: candidates.len(),
        previously_culled: before - candidates.len(),
        rescued,
        ..Default::default()
    };
    if report.previously_culled > 0 {
        eprintln!(
            "skipping {} candidate(s) already culled in {}",
            report.previously_culled,
            args.output.as_deref().unwrap_or_default(),
        );
    }
    if candidates.is_empty() {
        if args.json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else if report.previously_culled > 0 {
            println!(
                "Nothing new to triage: every remaining candidate ({}) was \
                 culled in an earlier pass.",
                report.previously_culled
            );
        } else {
            println!("Every package is in an essential inventory; nothing to triage.");
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Access levels first: they contextualize the prompt (a mere
    // committer reads differently from an owner), and the kept
    // candidates come out already classified. Day-fresh answers come
    // from the cache; only the rest go to dist-git.
    let (classified, warnings) =
        classify_with_cache(&args.user, &candidates, args.refresh_acls, args.verbose)?;
    report.warnings.extend(warnings);

    // Prompt only where the house rules allow it; otherwise every
    // candidate stays a candidate, which acts on nothing.
    let interactive = !args.yes && !args.json && std::io::stdin().is_terminal();
    let resolutions = if interactive {
        eprintln!(
            "{} candidate(s). cull = confirm it as a cull candidate; essential = \
             file it into an inventory (`e path.toml` in one line, or `e` then \
             the path); skip = leave it undecided for now (it comes back next run).",
            classified.len()
        );
        sandogasa_review::resolve_interactive_noted_with(
            classified,
            kondo::triage_summary,
            args.explain_into.as_deref(),
            &kondo::CULL_VOCABULARY,
        )?
    } else {
        classified
            .into_iter()
            .map(|c| (c, sandogasa_review::Resolution::Keep, None))
            .collect()
    };
    kondo::apply_resolutions(resolutions, &personal.inventory.maintainer, &mut report);

    let written = if let Some(path) = &args.output {
        let added = kondo::merge_culled(
            path,
            &report,
            &format!("{}-cull", personal.inventory.name),
            &personal.inventory.maintainer,
            args.reason.as_deref(),
        )?;
        Some((path.clone(), added))
    } else {
        None
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "report": report,
                "output": written.as_ref().map(|(p, _)| p),
                "output_added": written.as_ref().map(|(_, n)| n),
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    print!("{}", kondo::format_report(&report, &args.user));
    if !report.rescued.is_empty() {
        println!(
            "rescued earlier this run ({} package(s), now essential): {}",
            report.rescued.len(),
            report.rescued.join(", "),
        );
    }
    for (name, inv) in &report.explained {
        println!("filed {name} → {inv}");
    }
    if !report.removed.is_empty() {
        println!("dropped as false positives: {}", report.removed.join(", "));
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if let Some((path, added)) = written {
        println!("merged into {path} ({added} new)");
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_deps(paths: &[String], args: &DepsArgs) -> CmdResult {
    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    let roots: Vec<String> = inventory
        .package
        .iter()
        .filter(|p| !p.is_unshipped() && args.filter.matches(&p.name))
        .map(|p| p.name.clone())
        .collect();
    if roots.is_empty() {
        return Err("no shipped packages match the filters".into());
    }

    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(args.walk.branch.clone()),
        repo: args.walk.repo.clone(),
    };
    let from: std::collections::BTreeSet<String> = args.walk.from.iter().cloned().collect();
    let own: Option<std::collections::BTreeSet<String>> = match &args.fixpoint {
        Some(path) => Some(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name)
                .collect(),
        ),
        None => None,
    };
    let (report, graph) = deps::walk(
        &fedrq,
        &roots,
        &deps::WalkOpts {
            from: &from,
            base_prefixes: &args.walk.base_repo,
            build: !args.walk.runtime_only,
            fixpoint_own: own.as_ref(),
            verbose: args.verbose,
        },
    )?;
    // A closure without its graph strands the offline commands
    // (unkeep, dependents) a walk behind, so writing an inventory
    // always writes the graph beside it; --graph only moves it.
    let graph_path = args.graph.clone().or_else(|| {
        args.output
            .as_ref()
            .map(|o| format!("{}-graph.json", o.strip_suffix(".toml").unwrap_or(o)))
    });
    if let Some(path) = &graph_path {
        std::fs::write(path, serde_json::to_vec_pretty(&graph)?)
            .map_err(|e| format!("writing {path}: {e}"))?;
    }

    let name = args
        .name
        .clone()
        .unwrap_or_else(|| format!("{}-deps-{}", inventory.inventory.name, args.walk.branch));
    let written = if let Some(path) = &args.output {
        let kind = if args.walk.runtime_only {
            "Runtime dependencies"
        } else {
            "Runtime and build dependencies"
        };
        let description = format!(
            "{kind} of inventory '{}' from repos [{}], \
             resolved with fedrq -b {}{}. Generated by poi-tracker deps.",
            inventory.inventory.name,
            args.walk.from.join(", "),
            args.walk.branch,
            args.walk
                .repo
                .as_deref()
                .map(|r| format!(" -r {r}"))
                .unwrap_or_default(),
        );
        let out = deps::to_inventory(
            &report,
            &name,
            &description,
            &inventory.inventory.maintainer,
        );
        sandogasa_inventory::save(&out, path)?;
        Some(path.clone())
    } else {
        None
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "branch": args.walk.branch,
                "repo": args.walk.repo,
                "from": args.walk.from,
                "report": report,
                "output": written,
                "graph": graph_path,
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }

    println!(
        "{} runtime dependenc{} from [{}] for {} package(s) on {} \
         ({} binaries, {} wave(s), {:.0}s):",
        report.collected.len(),
        if report.collected.len() == 1 {
            "y"
        } else {
            "ies"
        },
        args.walk.from.join(", "),
        report.roots,
        args.walk.branch,
        report.binaries_walked,
        report.waves,
        report.elapsed_secs,
    );
    for dep in &report.collected {
        if dep.via.is_empty() {
            println!("  {} ({})", dep.source, dep.repoid);
        } else {
            println!(
                "  {} ({}) — {} requires {}",
                dep.source, dep.repoid, dep.required_by, dep.via
            );
        }
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    if !report.unmatched.is_empty() {
        eprintln!(
            "warning: {} capabilit{} could not be tied to a provider (--json lists them)",
            report.unmatched.len(),
            if report.unmatched.len() == 1 {
                "y"
            } else {
                "ies"
            },
        );
    }
    if let Some(path) = written {
        println!("wrote {path}");
    }
    if let Some(path) = graph_path {
        println!("wrote graph to {path}");
    }
    Ok(ExitCode::SUCCESS)
}

/// Classify the `-i` inventories' packages by their dependents in a
/// saved deps graph — the pruning report for shrinking a keep set.
fn cmd_dependents(paths: &[String], args: &DependentsArgs) -> CmdResult {
    let bytes = std::fs::read(&args.graph).map_err(|e| format!("reading {}: {e}", args.graph))?;
    let graph: deps::DepsGraph =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", args.graph))?;
    let mut names: std::collections::BTreeSet<String> = Default::default();
    for path in paths {
        names.extend(
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name),
        );
    }
    let report = dependents::classify(&graph, &names);
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", dependents::format_report(&report));
    }
    Ok(ExitCode::SUCCESS)
}

/// Replace `output`'s packages with a derived report's, keeping its
/// header (or borrowing `meta_from`'s when the file is new).
fn apply_derived(
    output: &str,
    meta_from: &str,
    report: &derive::DeriveReport,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut inventory = match std::path::Path::new(output).exists() {
        true => sandogasa_inventory::load(output)?,
        false => sandogasa_inventory::Inventory {
            inventory: sandogasa_inventory::load(meta_from)?.inventory,
            package: Vec::new(),
        },
    };
    inventory.package = report
        .derived
        .iter()
        .map(|d| sandogasa_inventory::Package {
            name: d.name.clone(),
            reason: d.reason.clone(),
            ..Default::default()
        })
        .collect();
    sandogasa_inventory::save(&inventory, output)?;
    Ok(())
}

/// Recompute a derived inventory (owned ∩ reachable-from-keeps) from
/// a saved graph — the global `-i` inventories are the keeps.
fn cmd_derive(paths: &[String], args: &DeriveArgs) -> CmdResult {
    use std::collections::BTreeSet;

    let bytes = std::fs::read(&args.graph).map_err(|e| format!("reading {}: {e}", args.graph))?;
    let graph: deps::DepsGraph =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", args.graph))?;
    let names_of = |path: &str| -> Result<BTreeSet<String>, String> {
        Ok(sandogasa_inventory::load(path)?
            .package
            .into_iter()
            .map(|p| p.name)
            .collect())
    };
    let mut keeps: BTreeSet<String> = Default::default();
    for path in paths {
        keeps.extend(names_of(path)?);
    }
    let owned = names_of(&args.owned)?;
    let current = match std::path::Path::new(&args.output).exists() {
        true => names_of(&args.output)?,
        false => Default::default(),
    };

    let report = derive::derive(&graph, &keeps, &owned, &current);
    if args.apply {
        apply_derived(&args.output, &paths[0], &report)?;
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", derive::format_report(&report, args.apply));
    }
    Ok(ExitCode::SUCCESS)
}

/// Start keeping packages: an incremental walk from the new roots
/// over a graph-backed query (offline for every capability the saved
/// graph knows, fedrq for the frontier), merged into the stored graph,
/// the names filed into a keep inventory, and the derived inventory
/// recomputed — the add half of incremental maintenance.
fn cmd_keep(paths: &[String], args: &KeepArgs) -> CmdResult {
    use std::collections::BTreeSet;
    use std::sync::atomic::Ordering::Relaxed;

    sandogasa_cli::require_tools(&[("fedrq", "sudo dnf install fedrq", Some("--version"))])?;
    let bytes = std::fs::read(&args.graph).map_err(|e| format!("reading {}: {e}", args.graph))?;
    let mut graph: deps::DepsGraph =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", args.graph))?;
    let names_of = |path: &str| -> Result<BTreeSet<String>, String> {
        Ok(sandogasa_inventory::load(path)?
            .package
            .into_iter()
            .map(|p| p.name)
            .collect())
    };
    let mut keeps: BTreeSet<String> = Default::default();
    for path in paths {
        keeps.extend(names_of(path)?);
    }
    for name in &args.names {
        if keeps.contains(name) {
            eprintln!(
                "note: {name} is already a keep; walking it to record its edges, not re-filing"
            );
        }
    }
    let owned = names_of(&args.owned)?;

    let fedrq = sandogasa_fedrq::Fedrq {
        branch: Some(args.walk.branch.clone()),
        repo: args.walk.repo.clone(),
    };
    let query = deps::GraphBackedQuery::new(&fedrq, &graph);
    let from: BTreeSet<String> = args.walk.from.iter().cloned().collect();
    let (report, new_graph) = deps::walk(
        &query,
        &args.names,
        &deps::WalkOpts {
            from: &from,
            base_prefixes: &args.walk.base_repo,
            build: !args.walk.runtime_only,
            fixpoint_own: Some(&owned),
            verbose: args.verbose,
        },
    )?;
    let (offline, online) = (query.offline.load(Relaxed), query.online.load(Relaxed));
    graph.merge(new_graph);
    std::fs::write(&args.graph, serde_json::to_vec_pretty(&graph)?)
        .map_err(|e| format!("writing {}: {e}", args.graph))?;

    let into = args.into.clone().unwrap_or_else(|| paths[0].clone());
    let maintainer = sandogasa_inventory::load(&paths[0])?.inventory.maintainer;
    let mut filed = 0usize;
    for name in args.names.iter().filter(|n| !keeps.contains(*n)) {
        // A keep already held by another -i inventory must not be
        // duplicated into --into: the keep set is their union.
        if kondo::file_into_inventory(&into, name, &maintainer)? {
            filed += 1;
        }
    }
    keeps.extend(args.names.iter().cloned());

    let derived = match &args.deps {
        Some(path) => {
            let current = match std::path::Path::new(path).exists() {
                true => names_of(path)?,
                false => Default::default(),
            };
            let d = derive::derive(&graph, &keeps, &owned, &current);
            apply_derived(path, &paths[0], &d)?;
            Some(d)
        }
        None => None,
    };

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "report": report,
                "graph": args.graph,
                "resolved_offline": offline,
                "resolved_online": online,
                "filed_into": into,
                "derived": derived,
            }))?
        );
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "kept {} package(s): {} collected dependenc{}, {} wave(s), {:.0}s — \
         {} capabilit{} answered from the graph, {} resolved live",
        args.names.len(),
        report.collected.len(),
        if report.collected.len() == 1 {
            "y"
        } else {
            "ies"
        },
        report.waves,
        report.elapsed_secs,
        offline,
        if offline == 1 { "y" } else { "ies" },
        online,
    );
    println!("filed {filed} into {into}; graph updated at {}", args.graph);
    if let Some(d) = &derived {
        print!("{}", derive::format_report(d, true));
    }
    for warning in &report.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The `-C DIR` / `--directory DIR` value in `argv`, if given (also
/// `--directory=DIR`); the first occurrence wins.
fn directory_flag(
    argv: impl IntoIterator<Item = std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    let mut it = argv.into_iter().skip(1);
    while let Some(arg) = it.next() {
        let s = arg.to_string_lossy();
        if s == "-C" || s == "--directory" {
            return it.next();
        }
        if let Some(rest) = s.strip_prefix("--directory=") {
            return Some(rest.to_string().into());
        }
        if let Some(rest) = s.strip_prefix("-C").filter(|r| !r.is_empty()) {
            return Some(rest.to_string().into());
        }
    }
    None
}

/// The maintenance loop over the workspace: see [`reconcile`].
fn cmd_reconcile(workspace: Option<&str>, args: &ReconcileArgs) -> CmdResult {
    use std::io::IsTerminal;

    let Some((ws, path)) = workspace::Workspace::find(workspace)? else {
        return Err(
            "reconcile needs a workspace file: pass -w PATH or run where ./kondo.toml is".into(),
        );
    };
    let resolve = |p: &str| ws.dir.join(p).to_string_lossy().into_owned();
    let opts = reconcile::Options {
        ws: &ws,
        into: args.into.clone(),
        yes: args.yes || args.dry_run,
        json: args.json,
        verbose: args.verbose,
        dry_run: args.dry_run,
    };
    if !args.json {
        println!("reconciling workspace {}", path.display());
    }
    let report = reconcile::run(&opts)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(ExitCode::SUCCESS);
    }
    if args.dry_run {
        println!(
            "dry run: {} owned package(s) would go to the triage",
            report.triage_candidates.len()
        );
        return Ok(ExitCode::SUCCESS);
    }
    if report.triage_candidates.is_empty() {
        println!("nothing left to triage: every owned package is justified or already culled");
        return Ok(ExitCode::SUCCESS);
    }
    let (Some(owned), Some(user)) = (ws.owned.as_deref().map(resolve), ws.user.clone()) else {
        println!(
            "{} package(s) to triage, but the workspace names no `user`: run `poi-tracker kondo --user NAME`",
            report.triage_candidates.len()
        );
        return Ok(ExitCode::SUCCESS);
    };
    println!(
        "
{} owned package(s) no essential inventory justifies — triaging with kondo",
        report.triage_candidates.len()
    );
    let kondo_args = KondoArgs {
        filter: WalkFilterArgs::default(),
        essential: ws.essential().iter().map(|p| resolve(p)).collect(),
        user,
        explain_into: None,
        yes: args.yes || !std::io::stdin().is_terminal(),
        refresh_acls: args.refresh_acls,
        reason: None,
        output: ws.cull.as_deref().map(resolve),
        json: false,
        verbose: args.verbose,
    };
    cmd_kondo(&[owned], &kondo_args)
}

/// Report (and with `--apply`, enact) what unkeeping packages frees,
/// over a saved deps graph instead of a fresh walk. The global `-i`
/// inventories are the keeps; `--deps` names the derived inventories
/// freed packages leave.
fn cmd_unkeep(paths: &[String], args: &UnkeepArgs) -> CmdResult {
    use std::collections::BTreeSet;

    let bytes = std::fs::read(&args.graph).map_err(|e| format!("reading {}: {e}", args.graph))?;
    let graph: deps::DepsGraph =
        serde_json::from_slice(&bytes).map_err(|e| format!("parsing {}: {e}", args.graph))?;
    let load_names = |path: &String| -> Result<(String, BTreeSet<String>), String> {
        Ok((
            path.clone(),
            sandogasa_inventory::load(path)?
                .package
                .into_iter()
                .map(|p| p.name)
                .collect(),
        ))
    };
    let keeps: BTreeMap<String, BTreeSet<String>> =
        paths.iter().map(&load_names).collect::<Result<_, _>>()?;
    let deps_files: BTreeMap<String, BTreeSet<String>> = args
        .deps
        .iter()
        .map(&load_names)
        .collect::<Result<_, _>>()?;

    let report = unkeep::plan(&graph, &args.names, &keeps, &deps_files);
    if args.apply {
        let unkept: BTreeSet<&str> = args.names.iter().map(String::as_str).collect();
        for (file, pkgs) in &keeps {
            if args.names.iter().any(|n| pkgs.contains(n)) {
                unkeep::remove_from_inventory(file, &unkept)?;
            }
        }
        let freed: BTreeSet<&str> = report.freed.iter().map(|f| f.name.as_str()).collect();
        for (file, pkgs) in &deps_files {
            if report.freed.iter().any(|f| pkgs.contains(&f.name)) {
                unkeep::remove_from_inventory(file, &freed)?;
            }
        }
        // Still-reached keeps are provably in the closure, so they
        // move into the derived inventory right away — the reason
        // chain comes from the graph, no fresh walk needed.
        if !report.moved.is_empty() {
            match args.deps.first() {
                Some(dest) => {
                    let packages: Vec<sandogasa_inventory::Package> = report
                        .moved
                        .iter()
                        .map(|(name, reason)| sandogasa_inventory::Package {
                            name: name.clone(),
                            reason: Some(reason.clone()),
                            ..Default::default()
                        })
                        .collect();
                    let meta = sandogasa_inventory::load(&paths[0])?.inventory;
                    intersect::merge_packages(dest, packages, &meta)?;
                }
                None => eprintln!(
                    "warning: {} still-reached package(s) have no --deps \
                     inventory to move into; the next deps walk will \
                     re-derive them",
                    report.moved.len()
                ),
            }
        }
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", unkeep::format_report(&report, args.apply));
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_config() -> CmdResult {
    let rt = new_runtime()?;
    rt.block_on(config::cmd_config())?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_triage_updates(paths: &[String], args: &TriageUpdatesArgs) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    let api_key = config::resolve_api_key(args.api_key.as_deref())?;
    let url = config::resolve_url();
    let client = sandogasa_bugzilla::BzClient::new(&url).with_api_key(api_key)?;

    let rt = new_runtime()?;
    let claim_email = config::resolve_email();
    if args.claim && claim_email.is_none() {
        return Err("--claim needs a configured Bugzilla email.\n\
             Set it with: poi-tracker config"
            .into());
    }
    let batch_email = resolve_batch_email(&args.batch)?;
    let dg = sandogasa_distgit::DistGitClient::new();
    let bodhi = sandogasa_bodhi::BodhiClient::new();
    let koji_lookup = koji_tag_lookup();
    let latest_tagged = match (&koji_lookup, args.skip_stale) {
        (Some(l), false) => Some(l as &triage_updates::TagLookup),
        (None, false) => {
            eprintln!(
                "warning: koji CLI not found; cannot verify builds Bodhi \
                 has no update for — such bugs will be left open. \
                 Install koji to enable the check."
            );
            None
        }
        (_, true) => None,
    };
    let report = rt.block_on(triage_updates::run(
        &inventory,
        &client,
        &dg,
        &bodhi,
        latest_tagged,
        &args.filter,
        batch_email.as_deref(),
        args.skip_stale,
        args.close_stale,
        args.claim,
        claim_email.as_deref(),
        args.dry_run,
        args.yes,
        args.verbose,
    ))?;
    eprintln!(
        "\n{} package(s) with managed priority, {} priority update(s) \
         planned, {} applied; {} stale-bug action(s) planned, {} \
         applied, {} failed",
        report.packages_with_priority,
        report.updates_planned,
        report.updates_applied,
        report.stale_planned,
        report.stale_applied,
        report.failures
    );
    Ok(if report.failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_show(paths: &[String], args: &ShowArgs) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;

    let packages = inventory.packages_for_workload(args.workload.as_deref());

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&packages).expect("JSON serialization failed")
        );
    } else {
        println!(
            "Inventory: {} ({} package(s))\n",
            inventory.inventory.name,
            packages.len()
        );
        for pkg in &packages {
            print!("  {}", pkg.name);
            let wls = inventory.workloads_for_package(&pkg.name);
            if !wls.is_empty() {
                print!(" [{}]", wls.join(", "));
            }
            println!();

            if let Some(ref poc) = pkg.poc {
                println!("    poc: {poc}");
            }
            if let Some(ref reason) = pkg.reason {
                println!("    reason: {reason}");
            }
            if let Some(ref rpms) = pkg.rpms {
                println!("    rpms: {}", rpms.join(", "));
            }
            if let Some(ref track) = pkg.track {
                println!("    track: {track}");
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_validate(paths: &[String]) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;

    let mut errors = 0;

    // Check for duplicate package names.
    let mut seen = std::collections::HashSet::new();
    for pkg in &inventory.package {
        if !seen.insert(&pkg.name) {
            eprintln!("error: duplicate package: {}", pkg.name);
            errors += 1;
        }
    }

    // Check packages are sorted.
    for window in inventory.package.windows(2) {
        if window[0].name > window[1].name {
            eprintln!(
                "warning: packages not sorted: {} before {}",
                window[0].name, window[1].name
            );
        }
    }

    // Check private_fields reference valid field names.
    let valid_fields = ["poc", "reason", "team", "task"];
    for field in &inventory.inventory.private_fields {
        if !valid_fields.contains(&field.as_str()) {
            eprintln!("warning: unknown private field: {field}");
        }
    }

    if errors > 0 {
        eprintln!("\n{errors} error(s) found.");
        Ok(ExitCode::FAILURE)
    } else {
        println!("Inventory OK: {} package(s).", inventory.package.len());
        Ok(ExitCode::SUCCESS)
    }
}

/// Write an export file, labelling the path in the error the way
/// every export site does.
fn write_export(path: &str, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| format!("failed to write {path}: {e}"))
}

fn cmd_export(paths: &[String], args: &ExportArgs) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;

    match &args.format {
        ExportFormat::ContentResolver { workload, output } => {
            // Determine which workloads to export.
            let workload_keys: Vec<&str> = match workload {
                Some(w) => vec![w.as_str()],
                None => {
                    let names = inventory.workload_names();
                    if names.is_empty() {
                        // No workloads defined: single-file export.
                        vec![]
                    } else {
                        names
                    }
                }
            };

            if workload_keys.is_empty() {
                // Single-file export (no workloads or --workload).
                let yaml =
                    sandogasa_inventory::content_resolver::export(&inventory, workload.as_deref());
                let default_filename =
                    format!("{}.yaml", inventory.inventory.name.replace(' ', "_"));
                let path = output.as_deref().unwrap_or(&default_filename);
                write_export(path, &yaml)?;
                eprintln!("Wrote {path}");
            } else if workload_keys.len() == 1 {
                // Single workload: respect -o if given.
                let yaml = sandogasa_inventory::content_resolver::export(
                    &inventory,
                    Some(workload_keys[0]),
                );
                let wl_name = workload_export_filename(&inventory, workload_keys[0]);
                let path = output.as_deref().unwrap_or(&wl_name);
                write_export(path, &yaml)?;
                eprintln!("Wrote {path}");
            } else {
                // Multi-workload: one file per workload.
                if output.is_some() {
                    return Err("-o/--output cannot be used when \
                         exporting multiple workloads"
                        .into());
                }
                for key in &workload_keys {
                    let yaml = sandogasa_inventory::content_resolver::export(&inventory, Some(key));
                    let path = workload_export_filename(&inventory, key);
                    write_export(&path, &yaml)?;
                    eprintln!("Wrote {path}");
                }
            }
        }
        ExportFormat::HsRelmon {
            workload,
            distros,
            track,
            output,
            prune,
        } => {
            let defaults = sandogasa_inventory::hs_relmon::RelmonDefaults {
                distros: distros.clone(),
                track: track.clone(),
                file_issue: true,
            };

            if let Some(path) = output
                && std::path::Path::new(path).exists()
            {
                let result = sandogasa_inventory::hs_relmon::merge_into_manifest(
                    path,
                    &inventory,
                    workload.as_deref(),
                    &defaults,
                    *prune,
                )?;

                if !result.stale.is_empty() && !prune {
                    eprintln!(
                        "warning: {} manifest entry/entries not in \
                         inventory (use --prune to remove):",
                        result.stale.len()
                    );
                    for name in &result.stale {
                        eprintln!("  {name}");
                    }
                }

                write_export(path, &result.content)?;

                if !result.unshipped_removed.is_empty() {
                    eprintln!(
                        "removed {} unshipped package(s) from {path}: {}",
                        result.unshipped_removed.len(),
                        result.unshipped_removed.join(", ")
                    );
                }
                let pruned_msg = if result.pruned > 0 {
                    format!(", {} pruned", result.pruned)
                } else {
                    String::new()
                };
                eprintln!(
                    "Merged into {path}: {} new{pruned_msg}, {} total",
                    result.added, result.total
                );
            } else {
                // Fresh export.
                let toml = sandogasa_inventory::hs_relmon::export(
                    &inventory,
                    workload.as_deref(),
                    &defaults,
                );
                if let Some(path) = output {
                    write_export(path, &toml)?;
                    eprintln!("Wrote {path}");
                } else {
                    print!("{toml}");
                }
            }
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn cmd_find(paths: &[String], args: &FindArgs) -> CmdResult {
    let mut found = false;
    for path in paths {
        let inventory = match sandogasa_inventory::load(path) {
            Ok(inv) => inv,
            Err(e) => {
                eprintln!("warning: {path}: {e}");
                continue;
            }
        };
        if let Some(pkg) = inventory.find_package(&args.name) {
            found = true;
            println!("{path}: {}", pkg.name);
            if let Some(ref poc) = pkg.poc {
                println!("  poc: {poc}");
            }
            if let Some(ref reason) = pkg.reason {
                println!("  reason: {reason}");
            }
            if let Some(ref rpms) = pkg.rpms {
                println!("  rpms: {}", rpms.join(", "));
            }
            let wls = inventory.workloads_for_package(&pkg.name);
            if !wls.is_empty() {
                println!("  workloads: {}", wls.join(", "));
            }
            if let Some(ref track) = pkg.track {
                println!("  track: {track}");
            }
        }
    }
    if !found {
        eprintln!("{} not found in any inventory.", args.name);
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

/// Merge new fields into an existing package without overwriting.
fn merge_into_package(existing: &mut sandogasa_inventory::Package, args: &AddArgs) {
    // Append RPMs (don't replace).
    if !args.rpm.is_empty() {
        let rpms = existing.rpms.get_or_insert_with(Vec::new);
        for rpm in &args.rpm {
            if !rpms.contains(rpm) {
                rpms.push(rpm.clone());
            }
        }
        rpms.sort();
    }
    // Workload membership is handled at the inventory level by the
    // caller (cmd_add) via add_to_workload.

    // Only set metadata if not already present.
    if existing.poc.is_none() {
        existing.poc.clone_from(&args.poc);
    }
    if existing.reason.is_none() {
        existing.reason.clone_from(&args.reason);
    }
    if existing.team.is_none() {
        existing.team.clone_from(&args.team);
    }
    if existing.task.is_none() {
        existing.task.clone_from(&args.task);
    }
    if existing.track.is_none() {
        existing.track.clone_from(&args.track);
    }
}

fn cmd_adopt(paths: &[String], args: &AdoptArgs) -> CmdResult {
    let inventory = sandogasa_inventory::load_and_merge(paths)?;
    // Detection is unauthenticated GETs, so --dry-run works
    // without a token; only actual adoption needs one.
    let token = if args.dry_run {
        None
    } else {
        Some(config::resolve_distgit_token(args.api_token.as_deref())?)
    };
    let mut dg = sandogasa_distgit::DistGitClient::new();
    if let Some(token) = token {
        dg = dg.with_token(token);
    }
    let rt = new_runtime()?;
    // Cheap precondition: a bad token should fail in seconds, not
    // after walking the whole inventory. Also tells us who the
    // new point of contact will be.
    let username = if args.dry_run {
        String::new()
    } else {
        rt.block_on(dg.verify_token()).map_err(|e| {
            format!(
                "dist-git token validation failed: {e}\n\
                 The token needs the \"Modify an existing project\" ACL; \
                 generate one at\n  \
                 https://src.fedoraproject.org/settings/token/new"
            )
        })?
    };
    let report = rt.block_on(adopt::run(
        &inventory,
        &dg,
        &username,
        &args.filter,
        args.dry_run,
        args.yes,
        args.verbose,
    ))?;
    eprintln!(
        "\n{} checked, {} orphaned, {} adopted, {} failed",
        report.packages_checked, report.orphaned_found, report.adopted, report.failures
    );
    Ok(if report.failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn cmd_add(paths: &[String], args: &AddArgs) -> CmdResult {
    // Search all inventories for the package.
    let mut target_path = None;
    for path in paths {
        if let Ok(inv) = sandogasa_inventory::load(path)
            && inv.find_package(&args.name).is_some()
        {
            target_path = Some(path.clone());
            break;
        }
    }

    // Fall back to first inventory file.
    let target_path = target_path.unwrap_or_else(|| paths[0].clone());

    let mut inventory = sandogasa_inventory::load(&target_path)?;

    if let Some(existing) = inventory.find_package_mut(&args.name) {
        // Merge into existing package.
        merge_into_package(existing, args);
        eprintln!("Updated {} in {target_path}", args.name);
    } else {
        // Add new package.
        let pkg = sandogasa_inventory::Package {
            name: args.name.clone(),
            poc: args.poc.clone(),
            reason: args.reason.clone(),
            team: args.team.clone(),
            task: args.task.clone(),
            rpms: (!args.rpm.is_empty()).then(|| args.rpm.clone()),
            track: args.track.clone(),
            ..Default::default()
        };
        inventory.add_package(pkg);
        eprintln!("Added {} to {target_path}", args.name);
    }

    // Add to workloads at the inventory level.
    for wl in &args.workload {
        inventory.add_to_workload(wl, &args.name);
    }

    sandogasa_inventory::save(&inventory, &target_path)?;

    Ok(ExitCode::SUCCESS)
}

fn cmd_remove(path: &str, args: &RemoveArgs) -> CmdResult {
    let mut inventory = sandogasa_inventory::load(path)?;

    if args.rpm.is_empty() {
        // Remove the whole package.
        if !inventory.remove_package(&args.name) {
            return Err(format!("package '{}' not found", args.name).into());
        }
        eprintln!("Removed {} from {path}", args.name);
    } else {
        // Remove specific RPMs from the package.
        let pkg = inventory
            .find_package_mut(&args.name)
            .ok_or_else(|| format!("package '{}' not found", args.name))?;
        if let Some(ref mut rpms) = pkg.rpms {
            for rpm in &args.rpm {
                rpms.retain(|r| r != rpm);
            }
            eprintln!("Removed RPM(s) {} from {}", args.rpm.join(", "), args.name);
        } else {
            return Err(format!("package '{}' has no RPM list", args.name).into());
        }
    }

    sandogasa_inventory::save(&inventory, path)?;

    Ok(ExitCode::SUCCESS)
}

fn cmd_import(args: &ImportArgs) -> CmdResult {
    let mut inventory = sandogasa_inventory::import_json::import_file(&args.json_file)?;

    if !args.private_fields.is_empty() {
        inventory.inventory.private_fields = args.private_fields.clone();
    }

    if !args.workload.is_empty() {
        let pkg_names: Vec<String> = inventory.package.iter().map(|p| p.name.clone()).collect();
        for wl in &args.workload {
            for name in &pkg_names {
                inventory.add_to_workload(wl, name);
            }
        }
    }

    sandogasa_inventory::save(&inventory, &args.output)?;

    eprintln!(
        "Imported {} package(s) from {} to {}",
        inventory.package.len(),
        args.json_file,
        args.output
    );
    Ok(ExitCode::SUCCESS)
}

/// Check if a package name matches any of the Pagure patterns.
/// An empty pattern list (no --pattern / no --auto-prefix) matches everything.
/// Case-insensitive to match Pagure's ILIKE behavior.
fn matches_any_pattern(name: &str, patterns: &[String]) -> bool {
    // No pattern means "all packages" — everything matches.
    if patterns.is_empty() || (patterns.len() == 1 && patterns[0].is_empty()) {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    patterns.iter().any(|pat| {
        if let Some(prefix) = pat.strip_suffix('*') {
            lower.starts_with(&prefix.to_ascii_lowercase())
        } else {
            lower == pat.to_ascii_lowercase()
        }
    })
}

/// Filter projects based on the user's group-access preferences.
fn filter_projects<'a>(
    projects: &'a [sandogasa_distgit::ProjectInfo],
    args: &SyncDistgitArgs,
) -> Vec<&'a sandogasa_distgit::ProjectInfo> {
    let Some(ref username) = args.user else {
        // Group mode: no filtering, return all.
        return projects.iter().collect();
    };

    projects
        .iter()
        .filter(|p| {
            let u = username.as_str();
            let has_direct = p.access_users.owner.iter().any(|x| x == u)
                || p.access_users.admin.iter().any(|x| x == u)
                || p.access_users.commit.iter().any(|x| x == u)
                || p.access_users.collaborator.iter().any(|x| x == u)
                || p.access_users.ticket.iter().any(|x| x == u);

            // Packages with direct access are always included.
            if has_direct {
                return true;
            }

            // User has only group-based access. Apply filters.
            if args.no_groups {
                return false;
            }

            if !args.include_group.is_empty() {
                return args
                    .include_group
                    .iter()
                    .any(|g| p.access_groups.contains_group(g));
            }

            if !args.exclude_group.is_empty() {
                return !args
                    .exclude_group
                    .iter()
                    .any(|g| p.access_groups.contains_group(g));
            }

            // Default: include all.
            true
        })
        .collect()
}

/// Build the list of Pagure name patterns to query.
///
/// User syncs default to per-prefix queries (a-z, 0-9): Pagure's
/// unfiltered username filter scans every project's ACLs and
/// routinely exceeds the gateway timeout (504). An explicit
/// --pattern restricts the query enough to run in one shot.
/// --start-pattern / --end-pattern bound the prefix scan (e.g.
/// to resume an interrupted sync) and imply it, as does
/// --auto-prefix; --no-auto-prefix forces a single unfiltered
/// query. An empty string in the result means "query without a
/// pattern".
fn build_patterns(args: &SyncDistgitArgs) -> Vec<String> {
    // Collapse the flags (clap rejects contradictory combinations)
    // and the mode-dependent default into one scan/no-scan choice.
    let scan = !args.no_auto_prefix
        && (args.auto_prefix
            || args.start_pattern.is_some()
            || args.end_pattern.is_some()
            || (args.user.is_some() && args.pattern.is_none()));
    if !scan {
        return vec![args.pattern.clone().unwrap_or_default()];
    }
    let all_prefixes = ('a'..='z').chain('0'..='9').map(|c| format!("{c}*"));
    let start = args
        .start_pattern
        .as_deref()
        .map(|p| p.trim_end_matches('*'))
        .unwrap_or("");
    let end = args
        .end_pattern
        .as_deref()
        .map(|p| p.trim_end_matches('*'))
        .unwrap_or("");
    let iter: Box<dyn Iterator<Item = String>> = if start.is_empty() {
        Box::new(all_prefixes)
    } else {
        Box::new(all_prefixes.skip_while(move |p| !p.starts_with(start)))
    };
    if end.is_empty() {
        iter.collect()
    } else {
        iter.take_while(|p| !p.starts_with(end)).collect()
    }
}

/// Trim the pattern list for a resumed run: fetching restarts at
/// the recorded failed pattern. A recorded pattern that's no
/// longer in the list (the flags changed between runs) keeps the
/// full list — safe, since re-fetching merges idempotently.
fn resume_patterns(patterns: Vec<String>, failed: &str) -> Vec<String> {
    match patterns.iter().position(|p| p == failed) {
        Some(idx) => patterns[idx..].to_vec(),
        None => patterns,
    }
}

/// Load an existing inventory file as a sync's base, or build a
/// fresh one described by the source when the file doesn't exist
/// yet. `default_name` names the fresh inventory (`--name` or a
/// source-derived default); it's ignored for an existing file.
fn load_or_create_inventory(
    path: &str,
    source_kind: &str,
    source_label: &str,
    default_name: &str,
    workloads: &[String],
) -> Result<sandogasa_inventory::Inventory, String> {
    if std::path::Path::new(path).exists() {
        return sandogasa_inventory::load(path).map_err(|e| format!("{path}: {e}"));
    }
    Ok(sandogasa_inventory::Inventory {
        inventory: sandogasa_inventory::InventoryMeta {
            name: default_name.to_string(),
            description: format!("Packages synced from {source_kind} ({source_label})"),
            maintainer: source_label.to_string(),
            labels: vec![],
            workloads: workloads_from_names(workloads),
            private_fields: vec![],
        },
        package: vec![],
    })
}

/// Add every remote name missing from the inventory, tagging each
/// with the sync's workloads. Returns the names added, which the
/// callers reuse for counts and retirement checks.
fn add_new_packages<'a>(
    inventory: &mut sandogasa_inventory::Inventory,
    names: impl IntoIterator<Item = &'a str>,
    workloads: &[String],
) -> Vec<String> {
    let mut added: Vec<String> = Vec::new();
    for name in names {
        if inventory.find_package(name).is_some() {
            continue;
        }
        inventory.add_package(sandogasa_inventory::Package {
            name: name.to_string(),
            ..Default::default()
        });
        for wl in workloads {
            inventory.add_to_workload(wl, name);
        }
        added.push(name.to_string());
    }
    added
}

/// Inventory packages the remote listing no longer has, scoped to
/// the patterns the run actually queried (an empty pattern list
/// means everything was in scope, so `--prune --pattern 'a*'`
/// won't drop non-`a*` packages). Packages marked unshipped are
/// preserved: a gone project is absent from the remote listing by
/// definition, and the tombstone is what keeps triage-retired
/// processing it.
fn stale_packages(
    inventory: &sandogasa_inventory::Inventory,
    remote_names: &std::collections::HashSet<&str>,
    patterns: &[String],
) -> Vec<String> {
    inventory
        .package
        .iter()
        .filter(|p| !p.is_unshipped())
        .filter(|p| !remote_names.contains(p.name.as_str()))
        .filter(|p| matches_any_pattern(&p.name, patterns))
        .map(|p| p.name.clone())
        .collect()
}

/// Remove the stale packages under `--prune`, or list them as a
/// warning so the user can decide.
fn prune_or_warn_stale(
    inventory: &mut sandogasa_inventory::Inventory,
    stale: &[String],
    prune: bool,
) {
    if stale.is_empty() {
        return;
    }
    if prune {
        for name in stale {
            inventory.remove_package(name);
        }
    } else {
        eprintln!(
            "warning: {} package(s) not in sync scope \
             (use --prune to remove):",
            stale.len()
        );
        for name in stale {
            eprintln!("  {name}");
        }
    }
}

/// The closing line every sync prints once the inventory is saved.
fn print_sync_summary(
    source_label: &str,
    added: usize,
    pruned: usize,
    prune: bool,
    inventory: &sandogasa_inventory::Inventory,
    output: &str,
) {
    let pruned_msg = if prune && pruned > 0 {
        format!(", {pruned} pruned")
    } else {
        String::new()
    };
    eprintln!(
        "Synced {source_label}: {added} new{pruned_msg}, \
         {} total in {output}",
        inventory.package.len()
    );
}

async fn sync_distgit_async(args: &SyncDistgitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new();

    // Validate group filters against actual membership.
    if let Some(ref user) = args.user {
        for group in &args.include_group {
            let members = client.get_group_members(group).await?;
            if !members.iter().any(|m| m == user) {
                return Err(format!("user '{user}' is not a member of group '{group}'").into());
            }
        }
        for group in &args.exclude_group {
            let members = client.get_group_members(group).await?;
            if !members.iter().any(|m| m == user) {
                eprintln!("warning: user '{user}' is not a member of group '{group}'");
            }
        }
    }

    let mut patterns = build_patterns(args);

    // Resume support: a failed run leaves `<output>.partial` (the
    // inventory as of the failure) and `<output>.partial.state`
    // (the pattern that failed). When both exist, pick up from the
    // failed pattern instead of re-fetching completed ones; the
    // partial replaces the output as the base inventory below.
    let partial_path = format!("{}.partial", args.output);
    let state_path = format!("{partial_path}.state");
    let resuming = !args.fast && std::path::Path::new(&partial_path).exists();
    if resuming {
        if let Ok(state) = std::fs::read_to_string(&state_path) {
            patterns = resume_patterns(patterns, state.trim());
            eprintln!(
                "resuming from pattern '{}' using {partial_path}",
                patterns.first().map(String::as_str).unwrap_or("")
            );
        } else {
            eprintln!("found {partial_path} but no state file; re-fetching all patterns");
        }
    }

    let source_label = if let Some(ref user) = args.user {
        format!("user:{user}")
    } else {
        format!("group:{}", args.group.as_deref().unwrap())
    };

    let mut all_projects = Vec::new();
    let mut fetch_error = None;
    let mut failed_pattern: Option<String> = None;
    if args.fast {
        // One request against the owner-alias dump. Entries are
        // synthesized as direct access, so the group filters below
        // pass them through; --pattern applies client-side. The
        // prune scope collapses to that single pattern.
        let user = args.user.as_ref().unwrap();
        all_projects = client.user_packages_fast(user).await?;
        if let Some(ref pat) = args.pattern {
            all_projects.retain(|p| matches_any_pattern(&p.name, std::slice::from_ref(pat)));
        }
        patterns = vec![args.pattern.clone().unwrap_or_default()];
    }
    let scan_patterns: &[String] = if args.fast { &[] } else { &patterns };
    for pat in scan_patterns {
        let result = if pat.is_empty() {
            if let Some(ref user) = args.user {
                client.user_projects(user, args.per_page, None).await
            } else {
                client
                    .group_projects(args.group.as_ref().unwrap(), args.per_page, None)
                    .await
            }
        } else {
            eprintln!("  pattern: {pat}");
            if let Some(ref user) = args.user {
                client.user_projects(user, args.per_page, Some(pat)).await
            } else {
                client
                    .group_projects(args.group.as_ref().unwrap(), args.per_page, Some(pat))
                    .await
            }
        };
        match result {
            Ok(p) => all_projects.extend(p),
            Err(e) => {
                eprintln!("error: {e}");
                if pat.is_empty() && e.to_string().contains("504") {
                    eprintln!(
                        "hint: Pagure's unfiltered project query often \
                         exceeds the gateway timeout; retry with \
                         --auto-prefix (or restrict with --pattern)"
                    );
                }
                fetch_error = Some(e);
                failed_pattern = Some(pat.clone());
                break;
            }
        }
    }
    sandogasa_distgit::client::dedup_projects(&mut all_projects);

    let total_fetched = all_projects.len();
    let mut filtered = filter_projects(&all_projects, args);
    let group_excluded = total_fetched - filtered.len();

    // Apply --exclude globs.
    if !args.exclude.is_empty() {
        filtered.retain(|p| !matches_any_pattern(&p.name, &args.exclude));
    }
    let pkg_excluded = total_fetched - group_excluded - filtered.len();

    if group_excluded > 0 || pkg_excluded > 0 {
        let mut parts = vec![format!("{total_fetched} unique")];
        if group_excluded > 0 {
            parts.push(format!("{group_excluded} excluded by group filter"));
        }
        if pkg_excluded > 0 {
            parts.push(format!("{pkg_excluded} excluded by --exclude"));
        }
        eprintln!("  {}", parts.join(", "));
    }

    // Load the base inventory: the partial when resuming (it was
    // derived from the output plus everything fetched before the
    // failure), the existing output otherwise, or a fresh one.
    let mut inventory = if resuming {
        sandogasa_inventory::load(&partial_path).map_err(|e| format!("{partial_path}: {e}"))?
    } else {
        let default_name = args
            .name
            .clone()
            .unwrap_or_else(|| source_label.replace(':', "-"));
        load_or_create_inventory(
            &args.output,
            "dist-git",
            &source_label,
            &default_name,
            &args.workload,
        )?
    };

    // Update inventory name if explicitly provided.
    if let Some(ref name) = args.name {
        inventory.inventory.name.clone_from(name);
    }

    let remote_names: std::collections::HashSet<&str> =
        filtered.iter().map(|p| p.name.as_str()).collect();

    // Add new packages, remembering which ones for
    // --mark-unshipped.
    let added_names = add_new_packages(
        &mut inventory,
        filtered.iter().map(|p| p.name.as_str()),
        &args.workload,
    );
    let added = added_names.len();

    // On fetch error, save partial results plus the failed
    // pattern, so the next run with the same -o resumes there.
    if let Some(e) = fetch_error {
        sandogasa_inventory::save(&inventory, &partial_path)?;
        if let Some(pat) = failed_pattern {
            std::fs::write(&state_path, format!("{pat}\n"))?;
        }
        eprintln!(
            "Saved {} package(s) to {partial_path} (incomplete); \
             re-run the same command to resume",
            inventory.package.len()
        );
        return Err(e);
    }

    // Detect packages in the inventory but not in the filtered
    // results, scoped to the active pattern(s). Excluded packages
    // naturally fall out of remote_names since they were filtered
    // above.
    let stale = stale_packages(&inventory, &remote_names, &patterns);
    let pruned = stale.len();
    prune_or_warn_stale(&mut inventory, &stale, args.prune);

    // Check the packages this run added against the active
    // branches, so a fresh inventory starts with `unshipped`
    // markers instead of needing a follow-up prune-retired run.
    // Best-effort: a failure here loses the markers, not the
    // sync (prune-retired can backfill them).
    if args.mark_unshipped && !added_names.is_empty() {
        eprintln!(
            "checking {} newly added package(s) for retirement...",
            added_names.len()
        );
        let marked = match prune_retired::active_branches_from_bodhi().await {
            Ok(active) => {
                let scanned = prune_retired::scan_packages(
                    &client,
                    added_names.clone(),
                    &active,
                    args.jobs,
                    false,
                )
                .await;
                match scanned {
                    Ok(findings) => {
                        let (candidates, invalid) = prune_retired::split_invalid(findings);
                        let n = prune_retired::apply_unshipped_marks(
                            &mut inventory,
                            &added_names,
                            &candidates,
                        );
                        for c in &candidates {
                            eprintln!("  {}: {}", c.package, c.reason.describe());
                        }
                        for name in &invalid {
                            eprintln!(
                                "  {name}: no such dist-git project — fix or remove the entry"
                            );
                        }
                        Some(n)
                    }
                    Err(e) => {
                        eprintln!(
                            "warning: retirement check failed ({e}); \
                             run prune-retired to mark unshipped packages"
                        );
                        None
                    }
                }
            }
            Err(e) => {
                eprintln!(
                    "warning: {e}; \
                     run prune-retired to mark unshipped packages"
                );
                None
            }
        };
        if let Some(n) = marked {
            eprintln!("marked {n} package(s) unshipped");
        }
    }

    sandogasa_inventory::save(&inventory, &args.output)?;
    // A completed run supersedes any leftover resume state.
    if resuming {
        let _ = std::fs::remove_file(&partial_path);
        let _ = std::fs::remove_file(&state_path);
    }

    print_sync_summary(
        &source_label,
        added,
        pruned,
        args.prune,
        &inventory,
        &args.output,
    );
    Ok(())
}

fn cmd_sync_distgit(args: &SyncDistgitArgs) -> CmdResult {
    // Group filters only apply to user mode.
    if args.user.is_none()
        && (args.no_groups || !args.include_group.is_empty() || !args.exclude_group.is_empty())
    {
        return Err("--no-groups, --include-group, and \
             --exclude-group only apply with --user"
            .into());
    }

    let rt = new_runtime()?;
    rt.block_on(sync_distgit_async(args))?;
    Ok(ExitCode::SUCCESS)
}

fn resolve_gitlab_url(args: &SyncGitlabArgs) -> Result<String, String> {
    if let Some(ref url) = args.url {
        return Ok(url.clone());
    }
    if let Some(ref preset) = args.preset {
        for &(name, url) in GITLAB_PRESETS {
            if name == preset.as_str() {
                return Ok(url.to_string());
            }
        }
        let valid: Vec<&str> = GITLAB_PRESETS.iter().map(|(n, _)| *n).collect();
        return Err(format!(
            "unknown preset '{preset}'. Valid: {}",
            valid.join(", ")
        ));
    }
    Err("specify --url or --preset".to_string())
}

fn cmd_sync_gitlab(args: &SyncGitlabArgs) -> CmdResult {
    let group_url = resolve_gitlab_url(args)?;

    let source_label = args.preset.clone().unwrap_or_else(|| group_url.clone());

    // Validate the cheap --mark-unshipped preconditions before the
    // long GitLab/CBS fetches: the source must map to a known CBS
    // SIG, and koji (cbs profile) must be available.
    let sig = if args.mark_unshipped {
        let Some(sig) = gitlab_unshipped::Sig::from_source(args.preset.as_deref(), &group_url)
        else {
            return Err(format!(
                "--mark-unshipped supports the hyperscale and \
                 proposed-updates sources only (no CBS release \
                 lifecycle for {source_label})"
            )
            .into());
        };
        sandogasa_cli::require_tools(&[("koji", "sudo dnf install koji", Some("version"))])?;
        Some(sig)
    } else {
        None
    };

    let projects = sandogasa_gitlab::list_group_projects(&group_url)?;

    let total_fetched = projects.len();

    // Apply --exclude globs.
    let names: Vec<&str> = if args.exclude.is_empty() {
        projects.iter().map(|p| p.name.as_str()).collect()
    } else {
        projects
            .iter()
            .map(|p| p.name.as_str())
            .filter(|n| !matches_any_pattern(n, &args.exclude))
            .collect()
    };

    let pkg_excluded = total_fetched - names.len();
    if pkg_excluded > 0 {
        eprintln!("  {total_fetched} fetched, {pkg_excluded} excluded");
    }

    // Load existing inventory or create a new one.
    let default_name = args.name.clone().unwrap_or_else(|| source_label.clone());
    let mut inventory = load_or_create_inventory(
        &args.output,
        "GitLab",
        &source_label,
        &default_name,
        &args.workload,
    )?;

    if let Some(ref name) = args.name {
        inventory.inventory.name.clone_from(name);
    }

    let remote_names: std::collections::HashSet<&str> = names.iter().copied().collect();

    let added = add_new_packages(&mut inventory, names.iter().copied(), &args.workload).len();

    // Detect stale packages. Every synced name is in scope here
    // (GitLab syncs have no pattern flag).
    let stale = stale_packages(&inventory, &remote_names, &[]);
    let pruned = stale.len();
    prune_or_warn_stale(&mut inventory, &stale, args.prune);

    // Mark archived projects with no released CBS build as
    // unshipped. Best-effort: a CBS/GitLab failure warns but the
    // sync still saves (re-run --mark-unshipped to backfill).
    if let Some(sig) = sig {
        let synced: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        eprintln!(
            "checking {} package(s) for CBS release status...",
            synced.len()
        );
        match gitlab_unshipped::shipped_packages(sig, &args.centos_release, false) {
            Ok(shipped) => match sandogasa_gitlab::list_archived_project_names(&group_url) {
                Ok(archived) => {
                    let outcome =
                        gitlab_unshipped::mark(&mut inventory, &synced, &archived, &shipped);
                    eprintln!(
                        "{} unshipped, {} archived-with-builds; {} marker(s) updated",
                        outcome.unshipped.len(),
                        outcome.archived_builds.len(),
                        outcome.changed
                    );
                    if !outcome.unshipped.is_empty() {
                        eprintln!(
                            "  unshipped (archived, no CBS build): {}",
                            outcome.unshipped.join(", ")
                        );
                    }
                    if !outcome.archived_builds.is_empty() {
                        eprintln!(
                            "  archived but still have CBS builds (run hs-relmon \
                             to prune): {}",
                            outcome.archived_builds.join(", ")
                        );
                    }
                }
                Err(e) => eprintln!(
                    "warning: fetching archived projects failed ({e}); \
                     re-run --mark-unshipped to mark unshipped packages"
                ),
            },
            Err(e) => eprintln!(
                "warning: CBS release scan failed ({e}); \
                 re-run --mark-unshipped to mark unshipped packages"
            ),
        }
    }

    sandogasa_inventory::save(&inventory, &args.output)?;

    print_sync_summary(
        &source_label,
        added,
        pruned,
        args.prune,
        &inventory,
        &args.output,
    );
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    /// The committed man page is generated from this CLI; see
    /// `sandogasa_cli::man` and `scripts/gen-man.sh`.
    #[test]
    fn man_page_matches_cli() {
        sandogasa_cli::man::check::<super::Cli>(
            concat!(env!("CARGO_MANIFEST_DIR"), "/man/poi-tracker.1"),
            env!("CARGO_PKG_VERSION"),
        );
    }

    use super::*;

    #[test]
    fn directory_flag_reads_the_git_style_forms() {
        let argv = |v: &[&str]| v.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert_eq!(
            directory_flag(argv(&["poi-tracker", "-C", "/data", "reconcile"])),
            Some("/data".into())
        );
        assert_eq!(
            directory_flag(argv(&["poi-tracker", "reconcile", "--directory=/d"])),
            Some("/d".into())
        );
        assert_eq!(
            directory_flag(argv(&["poi-tracker", "-C/x", "show"])),
            Some("/x".into())
        );
        assert_eq!(
            directory_flag(argv(&["poi-tracker", "show", "-i", "a.toml"])),
            None
        );
    }
    use sandogasa_distgit::ProjectInfo;

    fn make_project(name: &str, owner: &str, groups: &[&str]) -> ProjectInfo {
        let json = serde_json::json!({
            "name": name,
            "access_users": {
                "owner": [owner],
                "admin": [],
                "commit": [],
                "collaborator": [],
                "ticket": []
            },
            "access_groups": {
                "admin": [],
                "commit": groups,
                "collaborator": [],
                "ticket": []
            }
        });
        serde_json::from_value(json).unwrap()
    }

    fn make_project_with_commit(
        name: &str,
        owner: &str,
        commit_users: &[&str],
        groups: &[&str],
    ) -> ProjectInfo {
        let json = serde_json::json!({
            "name": name,
            "access_users": {
                "owner": [owner],
                "admin": [],
                "commit": commit_users,
                "collaborator": [],
                "ticket": []
            },
            "access_groups": {
                "admin": [],
                "commit": groups,
                "collaborator": [],
                "ticket": []
            }
        });
        serde_json::from_value(json).unwrap()
    }

    fn default_args() -> SyncDistgitArgs {
        SyncDistgitArgs {
            user: Some("alice".to_string()),
            group: None,
            output: "out.toml".to_string(),
            fast: false,
            no_groups: false,
            include_group: vec![],
            exclude_group: vec![],
            exclude: vec![],
            pattern: None,
            start_pattern: None,
            end_pattern: None,
            auto_prefix: false,
            no_auto_prefix: false,
            prune: false,
            mark_unshipped: false,
            jobs: 8,
            per_page: 100,
            workload: vec![],
            name: None,
        }
    }

    #[test]
    fn filter_group_mode_returns_all() {
        let projects = vec![
            make_project("aaa", "bob", &["rust-sig"]),
            make_project("bbb", "carol", &[]),
        ];
        let args = SyncDistgitArgs {
            user: None,
            group: Some("rust-sig".to_string()),
            ..default_args()
        };
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn filter_direct_access_always_included() {
        let projects = vec![make_project("pkg", "alice", &["rust-sig"])];
        let mut args = default_args();
        args.no_groups = true;
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_default_includes_group_only() {
        // alice has no direct access, only via rust-sig
        let projects = vec![make_project_with_commit("pkg", "bob", &[], &["rust-sig"])];
        let args = default_args();
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_no_groups_excludes_group_only() {
        let projects = vec![make_project_with_commit("pkg", "bob", &[], &["rust-sig"])];
        let mut args = default_args();
        args.no_groups = true;
        let result = filter_projects(&projects, &args);
        assert!(result.is_empty());
    }

    #[test]
    fn filter_no_groups_keeps_direct() {
        // alice is owner (direct) and also has access via group
        let projects = vec![make_project("pkg", "alice", &["rust-sig"])];
        let mut args = default_args();
        args.no_groups = true;
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn filter_include_group_matches() {
        let projects = vec![
            make_project_with_commit("a", "bob", &[], &["rust-sig"]),
            make_project_with_commit("b", "bob", &[], &["python-packagers-sig"]),
        ];
        let mut args = default_args();
        args.include_group = vec!["rust-sig".to_string()];
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "a");
    }

    #[test]
    fn filter_include_group_still_keeps_direct() {
        let projects = vec![
            make_project("owned", "alice", &[]),
            make_project_with_commit("group-only", "bob", &[], &["python-packagers-sig"]),
        ];
        let mut args = default_args();
        args.include_group = vec!["rust-sig".to_string()];
        let result = filter_projects(&projects, &args);
        // owned (direct) is kept, group-only (python-packagers-sig != rust-sig) is excluded
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "owned");
    }

    #[test]
    fn filter_exclude_group_removes_matching() {
        let projects = vec![
            make_project_with_commit("a", "bob", &[], &["rust-sig"]),
            make_project_with_commit("b", "bob", &[], &["python-packagers-sig"]),
        ];
        let mut args = default_args();
        args.exclude_group = vec!["rust-sig".to_string()];
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "b");
    }

    #[test]
    fn filter_exclude_group_keeps_direct() {
        let projects = vec![
            make_project("owned", "alice", &["rust-sig"]),
            make_project_with_commit("group-only", "bob", &[], &["rust-sig"]),
        ];
        let mut args = default_args();
        args.exclude_group = vec!["rust-sig".to_string()];
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "owned");
    }

    #[test]
    fn filter_include_multiple_groups() {
        let projects = vec![
            make_project_with_commit("a", "bob", &[], &["rust-sig"]),
            make_project_with_commit("b", "bob", &[], &["python-packagers-sig"]),
            make_project_with_commit("c", "bob", &[], &["kde-sig"]),
        ];
        let mut args = default_args();
        args.include_group = vec!["rust-sig".to_string(), "python-packagers-sig".to_string()];
        let result = filter_projects(&projects, &args);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].name, "a");
        assert_eq!(result[1].name, "b");
    }

    // ---- build_patterns ----

    #[test]
    fn build_patterns_user_defaults_to_auto_prefix() {
        // No --pattern: user syncs scan a-z then 0-9 by default,
        // since the unfiltered Pagure query times out (504).
        let args = default_args();
        let patterns = build_patterns(&args);
        assert_eq!(patterns.len(), 36);
        assert_eq!(patterns.first().unwrap(), "a*");
        assert_eq!(patterns[25], "z*");
        assert_eq!(patterns[26], "0*");
        assert_eq!(patterns.last().unwrap(), "9*");
    }

    #[test]
    fn build_patterns_user_explicit_pattern_is_single_query() {
        let mut args = default_args();
        args.pattern = Some("rust-*".to_string());
        assert_eq!(build_patterns(&args), vec!["rust-*".to_string()]);
    }

    #[test]
    fn build_patterns_no_auto_prefix_forces_single_query() {
        let mut args = default_args();
        args.no_auto_prefix = true;
        assert_eq!(build_patterns(&args), vec![String::new()]);
    }

    #[test]
    fn build_patterns_group_defaults_to_single_query() {
        let args = SyncDistgitArgs {
            user: None,
            group: Some("rust-sig".to_string()),
            ..default_args()
        };
        assert_eq!(build_patterns(&args), vec![String::new()]);
    }

    #[test]
    fn build_patterns_group_auto_prefix_opt_in() {
        let args = SyncDistgitArgs {
            user: None,
            group: Some("rust-sig".to_string()),
            auto_prefix: true,
            ..default_args()
        };
        assert_eq!(build_patterns(&args).len(), 36);
    }

    #[test]
    fn build_patterns_start_pattern_bounds_scan() {
        let mut args = default_args();
        args.start_pattern = Some("x".to_string());
        let patterns = build_patterns(&args);
        // x*, y*, z*, then 0*-9*
        assert_eq!(patterns.len(), 13);
        assert_eq!(patterns.first().unwrap(), "x*");
        assert_eq!(patterns[2], "z*");
        assert_eq!(patterns.last().unwrap(), "9*");
    }

    #[test]
    fn build_patterns_end_pattern_stops_scan() {
        let mut args = default_args();
        args.start_pattern = Some("b*".to_string());
        args.end_pattern = Some("e".to_string());
        assert_eq!(build_patterns(&args), vec!["b*", "c*", "d*"]);
    }

    #[test]
    fn build_patterns_group_scan_implied_by_bounds() {
        // Scan bounds imply prefix mode without --auto-prefix,
        // also for group syncs.
        let args = SyncDistgitArgs {
            user: None,
            group: Some("rust-sig".to_string()),
            end_pattern: Some("c".to_string()),
            ..default_args()
        };
        assert_eq!(build_patterns(&args), vec!["a*", "b*"]);
    }

    // ---- matches_any_pattern ----

    #[test]
    fn pattern_empty_matches_all() {
        assert!(matches_any_pattern("anything", &[]));
        assert!(matches_any_pattern("anything", &[String::new()]));
    }

    #[test]
    fn pattern_prefix_matches() {
        let pats = vec!["python-*".to_string()];
        assert!(matches_any_pattern("python-psutil", &pats));
        assert!(!matches_any_pattern("rust-libc", &pats));
    }

    #[test]
    fn pattern_exact_matches() {
        let pats = vec!["systemd".to_string()];
        assert!(matches_any_pattern("systemd", &pats));
        assert!(!matches_any_pattern("systemd-networkd", &pats));
    }

    #[test]
    fn resume_patterns_restarts_at_failed_pattern() {
        let pats = vec!["a*".to_string(), "b*".to_string(), "c*".to_string()];
        assert_eq!(resume_patterns(pats.clone(), "b*"), vec!["b*", "c*"]);
        // Failed on the first pattern: nothing was completed.
        assert_eq!(resume_patterns(pats.clone(), "a*"), pats);
    }

    #[test]
    fn resume_patterns_unknown_state_keeps_all() {
        // Flags changed between runs: re-fetch everything (safe,
        // merging is idempotent).
        let pats = vec!["a*".to_string(), "b*".to_string()];
        assert_eq!(resume_patterns(pats.clone(), "x*"), pats);
    }

    #[test]
    fn walk_filter_defaults_match_everything() {
        let f = WalkFilterArgs::default();
        assert!(f.matches("anything"));
    }

    #[test]
    fn walk_filter_range_is_inclusive_both_ends() {
        let f = WalkFilterArgs {
            pattern: vec![],
            start_from: Some("rust-nu-cli".to_string()),
            end_with: Some("rust-nu-engine".to_string()),
        };
        assert!(!f.matches("rust-itertools"));
        assert!(f.matches("rust-nu-cli"));
        assert!(f.matches("rust-nu-cmd-base"));
        assert!(f.matches("rust-nu-engine"));
        assert!(!f.matches("rust-nu-utils"));
    }

    #[test]
    fn walk_filter_pattern_and_range_compose() {
        let f = WalkFilterArgs {
            pattern: vec!["rust-*".to_string()],
            start_from: Some("rust-nu".to_string()),
            end_with: None,
        };
        // In range but wrong pattern:
        assert!(!f.matches("systemd"));
        // Matches pattern but before the range:
        assert!(!f.matches("rust-libc"));
        assert!(f.matches("rust-nu-cli"));
    }

    #[test]
    fn walk_filter_bare_pattern_is_exact() {
        // A bare name (no glob) replaces the old --package flag.
        let f = WalkFilterArgs {
            pattern: vec!["python-django3".to_string()],
            start_from: None,
            end_with: None,
        };
        assert!(f.matches("python-django3"));
        assert!(!f.matches("python-django30"));
    }

    #[test]
    fn pattern_multiple_any_matches() {
        let pats = vec!["a*".to_string(), "b*".to_string()];
        assert!(matches_any_pattern("autoconf", &pats));
        assert!(matches_any_pattern("btrfs-progs", &pats));
        assert!(!matches_any_pattern("cmake", &pats));
    }

    #[test]
    fn pattern_case_insensitive() {
        let pats = vec!["p*".to_string()];
        assert!(matches_any_pattern("python-psutil", &pats));
        assert!(matches_any_pattern("PackageKit", &pats));
        assert!(!matches_any_pattern("systemd", &pats));
    }
}
