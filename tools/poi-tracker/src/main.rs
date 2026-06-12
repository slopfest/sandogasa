// SPDX-License-Identifier: Apache-2.0 OR MIT

mod config;
mod prune_retired;
mod semver_audit;
mod triage_retired;
mod triage_updates;

use std::collections::BTreeMap;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_distgit::DistGitClient;

#[derive(Parser)]
#[command(
    version,
    about,
    long_about = None,
    before_help = concat!(
        env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION")
    )
)]
struct Cli {
    /// Path(s) to inventory TOML file(s).
    #[arg(short, long)]
    inventory: Vec<String>,

    /// Directory to scan for *.toml inventory files.
    #[arg(short = 'I', long, value_name = "DIR")]
    inventory_dir: Vec<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a package to the inventory.
    Add(AddArgs),
    /// Configure poi-tracker (Bugzilla API key, etc.).
    Config,
    /// Export inventory to another format.
    Export(ExportArgs),
    /// Find which inventory file(s) contain a package.
    Find(FindArgs),
    /// Import from legacy JSON format.
    Import(ImportArgs),
    /// Mark (or remove) packages no longer carried on any
    /// active branch (dist-git project gone or retired
    /// everywhere).
    PruneRetired(PruneRetiredArgs),
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

/// Package filters shared by the inventory-walking commands
/// (`semver-audit`, `triage-retired`, `triage-updates`). The
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

    /// Inventory name (default: derived from group).
    #[arg(long)]
    name: Option<String>,
}

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
    let cli = Cli::parse();

    // Import/sync commands produce new files and don't need existing
    // inventory paths. `Config` doesn't touch inventories at all.
    let needs_paths = !matches!(
        cli.command,
        Command::Config | Command::Import(_) | Command::SyncDistgit(_) | Command::SyncGitlab(_)
    );

    let paths = resolve_inventory_paths(&cli);

    if needs_paths && paths.is_empty() {
        eprintln!("error: no inventory files specified. Use -i or -I.");
        return ExitCode::FAILURE;
    }

    match &cli.command {
        Command::Add(args) => cmd_add(&paths, args),
        Command::Config => cmd_config(),
        Command::Export(args) => cmd_export(&paths, args),
        Command::Find(args) => cmd_find(&paths, args),
        Command::Import(args) => cmd_import(args),
        Command::PruneRetired(args) => cmd_prune_retired(&paths, args),
        Command::Remove(args) => cmd_remove(&paths[0], args),
        Command::SemverAudit(args) => cmd_semver_audit(&paths, args),
        Command::Show(args) => cmd_show(&paths, args),
        Command::SyncDistgit(args) => cmd_sync_distgit(args),
        Command::SyncGitlab(args) => cmd_sync_gitlab(args),
        Command::TriageRetired(args) => cmd_triage_retired(&paths, args),
        Command::TriageUpdates(args) => cmd_triage_updates(&paths, args),
        Command::Validate => cmd_validate(&paths),
    }
}

fn cmd_semver_audit(paths: &[String], args: &SemverAuditArgs) -> ExitCode {
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Read-only: anonymous Bugzilla search + public dist-git.
    let bz = sandogasa_bugzilla::BzClient::new(&config::resolve_url());
    let dg = sandogasa_distgit::DistGitClient::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let batch_email = match resolve_batch_email(&args.batch) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(semver_audit::run(
        &inventory,
        &bz,
        &dg,
        &args.filter,
        args.non_breaking,
        batch_email.as_deref(),
        args.verbose,
    )) {
        Ok(entries) => {
            if args.json {
                match serde_json::to_string_pretty(&entries) {
                    Ok(s) => println!("{s}"),
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                }
            } else {
                semver_audit::print_report(&entries);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_prune_retired(paths: &[String], args: &PruneRetiredArgs) -> ExitCode {
    // Pruning rewrites the inventory, which only makes sense for
    // a single file; --dry-run may preview a merged view.
    if !args.dry_run && paths.len() != 1 {
        eprintln!(
            "error: prune-retired modifies the inventory and needs \
             exactly one inventory file (got {}); use --dry-run to \
             preview a merged view",
            paths.len()
        );
        return ExitCode::FAILURE;
    }
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dg = sandogasa_distgit::DistGitClient::new();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    // The active branch set defines "carried anywhere": explicit
    // --branch wins (keeping the user's order), otherwise ask
    // Bodhi for the active releases, ordered most-likely-live
    // first so per-package checks short-circuit early.
    let active: Vec<String> = if !args.branch.is_empty() {
        args.branch.clone()
    } else {
        match rt.block_on(prune_retired::active_branches_from_bodhi()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    if args.verbose {
        eprintln!("[poi-tracker] active branches: {}", active.join(", "));
    }

    let report = match rt.block_on(prune_retired::run(
        &inventory,
        &dg,
        &active,
        &args.filter,
        args.jobs,
        args.verbose,
    )) {
        Ok(report) => report,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if !report.candidates.is_empty() {
        println!("Packages no longer carried on any active branch:");
        for c in &report.candidates {
            println!("- {}: {}", c.package, c.reason.describe());
        }
    }
    eprintln!(
        "\n{} checked, {} prunable",
        report.packages_checked,
        report.candidates.len()
    );
    if args.dry_run {
        return ExitCode::SUCCESS;
    }

    // Apply to the single inventory file. Default: update the
    // `unshipped` markers (both directions — clears marks on
    // revived packages). --remove deletes the entries instead.
    let path = &paths[0];
    let mut inv = match sandogasa_inventory::load(path) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: reloading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    if args.remove {
        if report.candidates.is_empty() {
            eprintln!("nothing to remove");
            return ExitCode::SUCCESS;
        }
        if !args.yes {
            match triage_updates::confirm(&format!(
                "Remove {} package(s) from {path}?",
                report.candidates.len()
            )) {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!("aborted: inventory not modified");
                    return ExitCode::SUCCESS;
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        let mut removed = 0usize;
        for c in &report.candidates {
            if inv.remove_package(&c.package) {
                removed += 1;
            }
        }
        if let Err(e) = sandogasa_inventory::save(&inv, path) {
            eprintln!("error: saving {path}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!("removed {removed} package(s) from {path}");
        return ExitCode::SUCCESS;
    }

    let changed =
        prune_retired::apply_unshipped_marks(&mut inv, &report.checked, &report.candidates);
    if changed == 0 {
        eprintln!("unshipped markers already up to date");
        return ExitCode::SUCCESS;
    }
    if !args.yes {
        match triage_updates::confirm(&format!(
            "Update unshipped markers on {changed} package(s) in {path}?"
        )) {
            Ok(true) => {}
            Ok(false) => {
                eprintln!("aborted: inventory not modified");
                return ExitCode::SUCCESS;
            }
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if let Err(e) = sandogasa_inventory::save(&inv, path) {
        eprintln!("error: saving {path}: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("updated unshipped markers on {changed} package(s) in {path}");
    ExitCode::SUCCESS
}

fn cmd_triage_retired(paths: &[String], args: &TriageRetiredArgs) -> ExitCode {
    // --mark writes results back, which only makes sense for a
    // single inventory file (a merged view has no single home).
    if args.mark && paths.len() != 1 {
        eprintln!(
            "error: --mark needs exactly one inventory file (got {})",
            paths.len()
        );
        return ExitCode::FAILURE;
    }
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let api_key = match config::resolve_api_key(args.api_key.as_deref()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let url = config::resolve_url();
    let bz = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(api_key) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dg = sandogasa_distgit::DistGitClient::new();

    let claim_email = config::resolve_email();
    if args.claim && claim_email.is_none() {
        eprintln!(
            "error: --claim needs a configured Bugzilla email.\n\
             Set it with: poi-tracker config"
        );
        return ExitCode::FAILURE;
    }

    let batch_email = match resolve_batch_email(&args.batch) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(triage_retired::run(
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
    )) {
        Ok(report) => {
            eprintln!(
                "\n{} checked, {} retired, {} planned, {} closed, {} failed",
                report.packages_checked,
                report.packages_retired,
                report.closes_planned,
                report.closes_applied,
                report.failures
            );
            // Record the retirement checks in the inventory. The
            // facts were gathered regardless of whether any bug
            // closures were confirmed, so marking is independent
            // of the close outcome.
            if args.mark {
                let path = &paths[0];
                let mut inv = match sandogasa_inventory::load(path) {
                    Ok(inv) => inv,
                    Err(e) => {
                        eprintln!("error: reloading {path} for --mark: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                let changed = triage_retired::apply_retirement_marks(&mut inv, &report.checks);
                if changed > 0 {
                    if let Err(e) = sandogasa_inventory::save(&inv, path) {
                        eprintln!("error: saving {path}: {e}");
                        return ExitCode::FAILURE;
                    }
                    eprintln!("marked {changed} package(s) in {path}");
                } else {
                    eprintln!("retirement markers already up to date");
                }
            }
            if report.failures > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_config() -> ExitCode {
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(config::cmd_config()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_triage_updates(paths: &[String], args: &TriageUpdatesArgs) -> ExitCode {
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let api_key = match config::resolve_api_key(args.api_key.as_deref()) {
        Ok(k) => k,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let url = config::resolve_url();
    let client = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(api_key) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let batch_email = match resolve_batch_email(&args.batch) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let dg = sandogasa_distgit::DistGitClient::new();
    let bodhi = sandogasa_bodhi::BodhiClient::new();
    match rt.block_on(triage_updates::run(
        &inventory,
        &client,
        &dg,
        &bodhi,
        &args.filter,
        batch_email.as_deref(),
        args.skip_stale,
        args.close_stale,
        args.dry_run,
        args.yes,
        args.verbose,
    )) {
        Ok(report) => {
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
            if report.failures > 0 {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_show(paths: &[String], args: &ShowArgs) -> ExitCode {
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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

    ExitCode::SUCCESS
}

fn cmd_validate(paths: &[String]) -> ExitCode {
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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
        ExitCode::FAILURE
    } else {
        println!("Inventory OK: {} package(s).", inventory.package.len());
        ExitCode::SUCCESS
    }
}

fn cmd_export(paths: &[String], args: &ExportArgs) -> ExitCode {
    let inventory = match sandogasa_inventory::load_and_merge(paths) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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
                if let Err(e) = std::fs::write(path, &yaml) {
                    eprintln!("error: failed to write {path}: {e}");
                    return ExitCode::FAILURE;
                }
                eprintln!("Wrote {path}");
            } else if workload_keys.len() == 1 {
                // Single workload: respect -o if given.
                let yaml = sandogasa_inventory::content_resolver::export(
                    &inventory,
                    Some(workload_keys[0]),
                );
                let wl_name = workload_export_filename(&inventory, workload_keys[0]);
                let path = output.as_deref().unwrap_or(&wl_name);
                if let Err(e) = std::fs::write(path, &yaml) {
                    eprintln!("error: failed to write {path}: {e}");
                    return ExitCode::FAILURE;
                }
                eprintln!("Wrote {path}");
            } else {
                // Multi-workload: one file per workload.
                if output.is_some() {
                    eprintln!(
                        "error: -o/--output cannot be used when \
                         exporting multiple workloads"
                    );
                    return ExitCode::FAILURE;
                }
                for key in &workload_keys {
                    let yaml = sandogasa_inventory::content_resolver::export(&inventory, Some(key));
                    let path = workload_export_filename(&inventory, key);
                    if let Err(e) = std::fs::write(&path, &yaml) {
                        eprintln!("error: failed to write {path}: {e}");
                        return ExitCode::FAILURE;
                    }
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
                let result = match sandogasa_inventory::hs_relmon::merge_into_manifest(
                    path,
                    &inventory,
                    workload.as_deref(),
                    &defaults,
                    *prune,
                ) {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("error: {e}");
                        return ExitCode::FAILURE;
                    }
                };

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

                if let Err(e) = std::fs::write(path, &result.content) {
                    eprintln!("error: failed to write {path}: {e}");
                    return ExitCode::FAILURE;
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
                    if let Err(e) = std::fs::write(path, &toml) {
                        eprintln!("error: failed to write {path}: {e}");
                        return ExitCode::FAILURE;
                    }
                    eprintln!("Wrote {path}");
                } else {
                    print!("{toml}");
                }
            }
        }
    }

    ExitCode::SUCCESS
}

fn cmd_find(paths: &[String], args: &FindArgs) -> ExitCode {
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
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
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

fn cmd_add(paths: &[String], args: &AddArgs) -> ExitCode {
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

    let mut inventory = match sandogasa_inventory::load(&target_path) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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
            rpms: if args.rpm.is_empty() {
                None
            } else {
                Some(args.rpm.clone())
            },
            arch_rpms: None,
            track: args.track.clone(),
            repology_name: None,
            distros: None,
            file_issue: None,
            priority: None,
            retired_on: None,
            unshipped: None,
        };
        inventory.add_package(pkg);
        eprintln!("Added {} to {target_path}", args.name);
    }

    // Add to workloads at the inventory level.
    for wl in &args.workload {
        inventory.add_to_workload(wl, &args.name);
    }

    if let Err(e) = sandogasa_inventory::save(&inventory, &target_path) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn cmd_remove(path: &str, args: &RemoveArgs) -> ExitCode {
    let mut inventory = match sandogasa_inventory::load(path) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    if args.rpm.is_empty() {
        // Remove the whole package.
        if !inventory.remove_package(&args.name) {
            eprintln!("error: package '{}' not found", args.name);
            return ExitCode::FAILURE;
        }
        eprintln!("Removed {} from {path}", args.name);
    } else {
        // Remove specific RPMs from the package.
        let pkg = match inventory.find_package_mut(&args.name) {
            Some(p) => p,
            None => {
                eprintln!("error: package '{}' not found", args.name);
                return ExitCode::FAILURE;
            }
        };
        if let Some(ref mut rpms) = pkg.rpms {
            for rpm in &args.rpm {
                rpms.retain(|r| r != rpm);
            }
            eprintln!("Removed RPM(s) {} from {}", args.rpm.join(", "), args.name);
        } else {
            eprintln!("error: package '{}' has no RPM list", args.name);
            return ExitCode::FAILURE;
        }
    }

    if let Err(e) = sandogasa_inventory::save(&inventory, path) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    ExitCode::SUCCESS
}

fn cmd_import(args: &ImportArgs) -> ExitCode {
    let mut inventory = match sandogasa_inventory::import_json::import_file(&args.json_file) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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

    if let Err(e) = sandogasa_inventory::save(&inventory, &args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    eprintln!(
        "Imported {} package(s) from {} to {}",
        inventory.package.len(),
        args.json_file,
        args.output
    );
    ExitCode::SUCCESS
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
    } else if std::path::Path::new(&args.output).exists() {
        sandogasa_inventory::load(&args.output).map_err(|e| format!("{}: {e}", args.output))?
    } else {
        let inv_name = args
            .name
            .clone()
            .unwrap_or_else(|| source_label.replace(':', "-"));
        sandogasa_inventory::Inventory {
            inventory: sandogasa_inventory::InventoryMeta {
                name: inv_name,
                description: format!("Packages synced from dist-git ({source_label})"),
                maintainer: source_label.clone(),
                labels: vec![],
                workloads: workloads_from_names(&args.workload),
                private_fields: vec![],
            },
            package: vec![],
        }
    };

    // Update inventory name if explicitly provided.
    if let Some(ref name) = args.name {
        inventory.inventory.name.clone_from(name);
    }

    let remote_names: std::collections::HashSet<&str> =
        filtered.iter().map(|p| p.name.as_str()).collect();

    // Add new packages, remembering which ones for
    // --mark-unshipped.
    let mut added_names: Vec<String> = Vec::new();
    for p in &filtered {
        if inventory.find_package(&p.name).is_some() {
            continue;
        }
        inventory.add_package(sandogasa_inventory::Package {
            name: p.name.clone(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
            priority: None,
            retired_on: None,
            unshipped: None,
        });
        for wl in &args.workload {
            inventory.add_to_workload(wl, &p.name);
        }
        added_names.push(p.name.clone());
    }
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

    // Detect packages in the inventory but not in the filtered results.
    // Scoped to the active pattern(s) so --prune with --pattern 'a*'
    // won't drop non-a* packages. Excluded packages naturally fall
    // out of remote_names since they were filtered above. Packages
    // marked unshipped are preserved: a gone project is absent from
    // the remote listing by definition, and the tombstone is what
    // keeps triage-retired processing it.
    let stale: Vec<String> = inventory
        .package
        .iter()
        .filter(|p| !p.is_unshipped())
        .filter(|p| !remote_names.contains(p.name.as_str()))
        .filter(|p| matches_any_pattern(&p.name, &patterns))
        .map(|p| p.name.clone())
        .collect();

    let pruned = stale.len();
    if !stale.is_empty() {
        if args.prune {
            for name in &stale {
                inventory.remove_package(name);
            }
        } else {
            eprintln!(
                "warning: {} package(s) not in sync scope \
                 (use --prune to remove):",
                stale.len()
            );
            for name in &stale {
                eprintln!("  {name}");
            }
        }
    }

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
                match prune_retired::scan_packages(
                    &client,
                    added_names.clone(),
                    &active,
                    args.jobs,
                    false,
                )
                .await
                {
                    Ok(candidates) => {
                        let n = prune_retired::apply_unshipped_marks(
                            &mut inventory,
                            &added_names,
                            &candidates,
                        );
                        for c in &candidates {
                            eprintln!("  {}: {}", c.package, c.reason.describe());
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

    let pruned_msg = if args.prune && pruned > 0 {
        format!(", {pruned} pruned")
    } else {
        String::new()
    };
    eprintln!(
        "Synced {source_label}: {added} new{pruned_msg}, \
         {} total in {}",
        inventory.package.len(),
        args.output
    );
    Ok(())
}

fn cmd_sync_distgit(args: &SyncDistgitArgs) -> ExitCode {
    // Group filters only apply to user mode.
    if args.user.is_none()
        && (args.no_groups || !args.include_group.is_empty() || !args.exclude_group.is_empty())
    {
        eprintln!(
            "error: --no-groups, --include-group, and \
             --exclude-group only apply with --user"
        );
        return ExitCode::FAILURE;
    }

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to create runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    match rt.block_on(sync_distgit_async(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
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

fn cmd_sync_gitlab(args: &SyncGitlabArgs) -> ExitCode {
    let group_url = match resolve_gitlab_url(args) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let source_label = args.preset.clone().unwrap_or_else(|| group_url.clone());

    let projects = match sandogasa_gitlab::list_group_projects(&group_url) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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
    let mut inventory = if std::path::Path::new(&args.output).exists() {
        match sandogasa_inventory::load(&args.output) {
            Ok(inv) => inv,
            Err(e) => {
                eprintln!("error: {}: {e}", args.output);
                return ExitCode::FAILURE;
            }
        }
    } else {
        let inv_name = args.name.clone().unwrap_or_else(|| source_label.clone());
        sandogasa_inventory::Inventory {
            inventory: sandogasa_inventory::InventoryMeta {
                name: inv_name,
                description: format!("Packages synced from GitLab ({source_label})"),
                maintainer: source_label.clone(),
                labels: vec![],
                workloads: workloads_from_names(&args.workload),
                private_fields: vec![],
            },
            package: vec![],
        }
    };

    if let Some(ref name) = args.name {
        inventory.inventory.name.clone_from(name);
    }

    let remote_names: std::collections::HashSet<&str> = names.iter().copied().collect();

    let mut added = 0usize;
    for name in &names {
        if inventory.find_package(name).is_some() {
            continue;
        }
        inventory.add_package(sandogasa_inventory::Package {
            name: name.to_string(),
            poc: None,
            reason: None,
            team: None,
            task: None,
            rpms: None,
            arch_rpms: None,
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
            priority: None,
            retired_on: None,
            unshipped: None,
        });
        for wl in &args.workload {
            inventory.add_to_workload(wl, name);
        }
        added += 1;
    }

    // Detect stale packages. Unshipped tombstones are preserved
    // (see the sync-distgit prune above).
    let stale: Vec<String> = inventory
        .package
        .iter()
        .filter(|p| !p.is_unshipped())
        .filter(|p| !remote_names.contains(p.name.as_str()))
        .map(|p| p.name.clone())
        .collect();

    let pruned = stale.len();
    if !stale.is_empty() {
        if args.prune {
            for name in &stale {
                inventory.remove_package(name);
            }
        } else {
            eprintln!(
                "warning: {} package(s) not in sync scope \
                 (use --prune to remove):",
                stale.len()
            );
            for name in &stale {
                eprintln!("  {name}");
            }
        }
    }

    if let Err(e) = sandogasa_inventory::save(&inventory, &args.output) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    let pruned_msg = if args.prune && pruned > 0 {
        format!(", {pruned} pruned")
    } else {
        String::new()
    };
    eprintln!(
        "Synced {source_label}: {added} new{pruned_msg}, \
         {} total in {}",
        inventory.package.len(),
        args.output
    );
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
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
