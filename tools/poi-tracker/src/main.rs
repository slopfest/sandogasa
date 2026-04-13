// SPDX-License-Identifier: MPL-2.0

use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    #[arg(short, long, default_value = "inventory.toml")]
    inventory: Vec<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add a package to the inventory.
    Add(AddArgs),
    /// Export inventory to another format.
    Export(ExportArgs),
    /// Import from legacy JSON format.
    Import(ImportArgs),
    /// Remove a package from the inventory.
    Remove(RemoveArgs),
    /// Show inventory contents.
    Show(ShowArgs),
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

    /// Binary RPM subpackage(s) to track.
    #[arg(long, value_delimiter = ',')]
    rpm: Vec<String>,

    /// Domain tag(s).
    #[arg(long, value_delimiter = ',')]
    domain: Vec<String>,

    /// Track branch for hs-relmon (e.g. upstream, fedora-rawhide).
    #[arg(long)]
    track: Option<String>,
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

fn main() -> ExitCode {
    let cli = Cli::parse();

    match &cli.command {
        Command::Show(args) => cmd_show(&cli.inventory, args),
        Command::Validate => cmd_validate(&cli.inventory),
        Command::Export(args) => cmd_export(&cli.inventory, args),
        // Mutating commands operate on the first inventory file only.
        Command::Add(args) => cmd_add(&cli.inventory[0], args),
        Command::Remove(args) => cmd_remove(&cli.inventory[0], args),
        Command::Import(args) => cmd_import(args),
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

fn cmd_add(path: &str, args: &AddArgs) -> ExitCode {
    let mut inventory = match sandogasa_inventory::load(path) {
        Ok(inv) => inv,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

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

    let replacing = inventory.find_package(&args.name).is_some();
    inventory.add_package(pkg);

    if let Err(e) = sandogasa_inventory::save(&inventory, path) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }

    if replacing {
        eprintln!("Updated {} in {path}", args.name);
    } else {
        eprintln!("Added {} to {path}", args.name);
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
