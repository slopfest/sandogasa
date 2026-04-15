// SPDX-License-Identifier: Apache-2.0 OR MIT

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
    /// Export inventory to another format.
    Export(ExportArgs),
    /// Find which inventory file(s) contain a package.
    Find(FindArgs),
    /// Import from legacy JSON format.
    Import(ImportArgs),
    /// Remove a package from the inventory.
    Remove(RemoveArgs),
    /// Show inventory contents.
    Show(ShowArgs),
    /// Sync inventory from Fedora dist-git (Pagure) access.
    SyncDistgit(SyncDistgitArgs),
    /// Sync inventory from a GitLab RPM group.
    SyncGitlab(SyncGitlabArgs),
    /// Validate inventory consistency.
    Validate,
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

    /// Domain tag(s) (comma-separated or repeated).
    #[arg(long, value_delimiter = ',')]
    domain: Vec<String>,

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
    /// Filter by domain.
    #[arg(long)]
    domain: Option<String>,

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
        /// Filter by domain.
        #[arg(long)]
        domain: Option<String>,
        /// Output file (default: {inventory-name}.yaml).
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Export as hs-relmon manifest TOML.
    HsRelmon {
        /// Filter by domain.
        #[arg(long)]
        domain: Option<String>,
        /// Output file (default: stdout).
        #[arg(short, long)]
        output: Option<String>,

        /// Default distros list.
        #[arg(long, default_value = "upstream,fedora,centos,hyperscale")]
        distros: String,

        /// Default tracking branch.
        #[arg(long, default_value = "upstream")]
        track: String,
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

    /// Domain tag(s) to apply to all imported packages.
    #[arg(long, value_delimiter = ',', value_name = "DOMAIN,...")]
    domain: Vec<String>,
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

    /// Name pattern, or --auto-prefix start point.
    #[arg(long)]
    pattern: Option<String>,

    /// Stop --auto-prefix before this prefix.
    #[arg(long, requires = "auto_prefix")]
    end_pattern: Option<String>,

    /// Query by a-z/0-9 prefix to avoid timeouts.
    #[arg(long)]
    auto_prefix: bool,

    /// Remove packages no longer in dist-git results.
    #[arg(long)]
    prune: bool,

    /// Pagure API page size.
    #[arg(long, default_value = "100")]
    per_page: u32,

    /// Domain tags (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "DOMAIN,...")]
    domain: Vec<String>,

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

    /// Domain tags (CSV or repeated).
    #[arg(long, value_delimiter = ',', value_name = "DOMAIN,...")]
    domain: Vec<String>,

    /// Inventory name (default: derived from group).
    #[arg(long)]
    name: Option<String>,
}

/// Collect inventory paths from -i and -I flags.
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
    // inventory paths.
    let needs_paths = !matches!(
        cli.command,
        Command::Import(_) | Command::SyncDistgit(_) | Command::SyncGitlab(_)
    );

    let paths = resolve_inventory_paths(&cli);

    if needs_paths && paths.is_empty() {
        eprintln!("error: no inventory files specified. Use -i or -I.");
        return ExitCode::FAILURE;
    }

    match &cli.command {
        Command::Add(args) => cmd_add(&paths, args),
        Command::Export(args) => cmd_export(&paths, args),
        Command::Find(args) => cmd_find(&paths, args),
        Command::Import(args) => cmd_import(args),
        Command::Remove(args) => cmd_remove(&paths[0], args),
        Command::Show(args) => cmd_show(&paths, args),
        Command::SyncDistgit(args) => cmd_sync_distgit(args),
        Command::SyncGitlab(args) => cmd_sync_gitlab(args),
        Command::Validate => cmd_validate(&paths),
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

    let packages = inventory.packages_for_domain(args.domain.as_deref());

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
            if let Some(ref domains) = pkg.domains {
                print!(" [{}]", domains.join(", "));
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

    let (content, default_filename) = match &args.format {
        ExportFormat::ContentResolver { domain, .. } => {
            let yaml = sandogasa_inventory::content_resolver::export(&inventory, domain.as_deref());
            let filename = format!("{}.yaml", inventory.inventory.name.replace(' ', "_"));
            (yaml, Some(filename))
        }
        ExportFormat::HsRelmon {
            domain,
            distros,
            track,
            ..
        } => {
            let defaults = sandogasa_inventory::hs_relmon::RelmonDefaults {
                distros: distros.clone(),
                track: track.clone(),
                file_issue: true,
            };
            let toml =
                sandogasa_inventory::hs_relmon::export(&inventory, domain.as_deref(), &defaults);
            (toml, None)
        }
    };

    let output_path = match &args.format {
        ExportFormat::ContentResolver { output, .. } | ExportFormat::HsRelmon { output, .. } => {
            output.clone().or(default_filename)
        }
    };

    if let Some(ref path) = output_path {
        if let Err(e) = std::fs::write(path, &content) {
            eprintln!("error: failed to write {path}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!("Wrote {path}");
    } else {
        print!("{content}");
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
            if let Some(ref domains) = pkg.domains {
                println!("  domains: {}", domains.join(", "));
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
    // Append domains (don't replace).
    if !args.domain.is_empty() {
        let domains = existing.domains.get_or_insert_with(Vec::new);
        for d in &args.domain {
            if !domains.contains(d) {
                domains.push(d.clone());
            }
        }
        domains.sort();
    }
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
            domains: if args.domain.is_empty() {
                None
            } else {
                Some(args.domain.clone())
            },
            track: args.track.clone(),
            repology_name: None,
            distros: None,
            file_issue: None,
        };
        inventory.add_package(pkg);
        eprintln!("Added {} to {target_path}", args.name);
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

    if !args.domain.is_empty() {
        for pkg in &mut inventory.package {
            pkg.domains = Some(args.domain.clone());
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

    let all_prefixes: Vec<String> = ('a'..='z')
        .chain('0'..='9')
        .map(|c| format!("{c}*"))
        .collect();

    let patterns = if args.auto_prefix {
        let start = args
            .pattern
            .as_deref()
            .map(|p| p.trim_end_matches('*'))
            .unwrap_or("");
        let end = args
            .end_pattern
            .as_deref()
            .map(|p| p.trim_end_matches('*'))
            .unwrap_or("");
        let iter = all_prefixes.into_iter();
        let iter: Box<dyn Iterator<Item = String>> = if start.is_empty() {
            Box::new(iter)
        } else {
            Box::new(iter.skip_while(move |p| !p.starts_with(start)))
        };
        if end.is_empty() {
            iter.collect()
        } else {
            iter.take_while(|p| !p.starts_with(end)).collect()
        }
    } else {
        vec![args.pattern.clone().unwrap_or_default()]
    };

    let source_label = if let Some(ref user) = args.user {
        format!("user:{user}")
    } else {
        format!("group:{}", args.group.as_deref().unwrap())
    };

    let mut all_projects = Vec::new();
    let mut fetch_error = None;
    for pat in &patterns {
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
                fetch_error = Some(e);
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

    // Load existing inventory or create a new one.
    let mut inventory = if std::path::Path::new(&args.output).exists() {
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
                domains: args.domain.clone(),
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

    // Add new packages.
    let mut added = 0usize;
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
            domains: if args.domain.is_empty() {
                None
            } else {
                Some(args.domain.clone())
            },
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
        });
        added += 1;
    }

    // On fetch error, save partial results and bail.
    if let Some(e) = fetch_error {
        let partial = format!("{}.partial", args.output);
        sandogasa_inventory::save(&inventory, &partial)?;
        eprintln!(
            "Saved {} package(s) to {partial} (incomplete, \
             verify before renaming)",
            inventory.package.len()
        );
        return Err(e);
    }

    // Detect packages in the inventory but not in the filtered results.
    // Scoped to the active pattern(s) so --prune with --pattern 'a*'
    // won't drop non-a* packages. Excluded packages naturally fall
    // out of remote_names since they were filtered above.
    let stale: Vec<String> = inventory
        .package
        .iter()
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

    sandogasa_inventory::save(&inventory, &args.output)?;

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
                domains: args.domain.clone(),
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
            domains: if args.domain.is_empty() {
                None
            } else {
                Some(args.domain.clone())
            },
            track: None,
            repology_name: None,
            distros: None,
            file_issue: None,
        });
        added += 1;
    }

    // Detect stale packages.
    let stale: Vec<String> = inventory
        .package
        .iter()
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
            no_groups: false,
            include_group: vec![],
            exclude_group: vec![],
            exclude: vec![],
            pattern: None,
            end_pattern: None,
            auto_prefix: false,
            prune: false,
            per_page: 100,
            domain: vec![],
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
