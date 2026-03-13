// SPDX-License-Identifier: MPL-2.0

mod config;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_distgit::DistGitClient;

use config::AclConfig;

/// View and manage Fedora package ACLs via the
/// Pagure dist-git API
#[derive(Parser)]
#[command(about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply ACLs from a TOML config file
    Apply {
        /// Path to TOML config file
        #[arg(short = 'f', long)]
        config: PathBuf,

        /// Pagure API token
        #[arg(long, env = "PAGURE_API_TOKEN")]
        token: String,
    },

    /// Remove all ACLs for a user or group
    Remove {
        /// Package name
        package: String,

        /// User to remove ACLs for
        #[arg(long, group = "target")]
        user: Option<String>,

        /// Group to remove ACLs for
        #[arg(long, group = "target")]
        group: Option<String>,

        /// Pagure API token
        #[arg(long, env = "PAGURE_API_TOKEN")]
        token: String,
    },

    /// Set an ACL level for a user or group
    Set {
        /// Package name
        package: String,

        /// User to set ACL for
        #[arg(long, group = "target")]
        user: Option<String>,

        /// Group to set ACL for
        #[arg(long, group = "target")]
        group: Option<String>,

        /// ACL level (ticket, collaborator, commit, admin)
        #[arg(long, value_parser = parse_acl_level)]
        level: String,

        /// Pagure API token
        #[arg(long, env = "PAGURE_API_TOKEN")]
        token: String,
    },

    /// Show current ACLs for a package
    Show {
        /// Package name
        package: String,
    },
}

fn parse_acl_level(s: &str) -> Result<String, String> {
    match s {
        "ticket" | "collaborator" | "commit" | "admin" => Ok(s.to_string()),
        _ => Err(format!(
            "invalid ACL level '{s}' \
             (valid: ticket, collaborator, commit, admin)"
        )),
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Command::Apply { config, token } => cmd_apply(&config, &token).await,
        Command::Remove {
            package,
            user,
            group,
            token,
        } => {
            let (user_type, name) = resolve_target(user, group)?;
            cmd_remove(&package, &user_type, &name, &token).await
        }
        Command::Set {
            package,
            user,
            group,
            level,
            token,
        } => {
            let (user_type, name) = resolve_target(user, group)?;
            cmd_set(&package, &user_type, &name, &level, &token).await
        }
        Command::Show { package } => cmd_show(&package).await,
    }
}

fn resolve_target(
    user: Option<String>,
    group: Option<String>,
) -> Result<(String, String), Box<dyn std::error::Error>> {
    match (user, group) {
        (Some(u), None) => Ok(("user".to_string(), u)),
        (None, Some(g)) => Ok(("group".to_string(), g)),
        _ => Err("specify exactly one of --user or --group".into()),
    }
}

async fn cmd_show(package: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new();
    let acls = client.get_acls(package).await?;

    println!("Package: {package}");

    println!("\nUsers:");
    for name in &acls.access_users.owner {
        println!("  {name}: owner");
    }
    for name in &acls.access_users.admin {
        println!("  {name}: admin");
    }
    for name in &acls.access_users.commit {
        println!("  {name}: commit");
    }
    for name in &acls.access_users.collaborator {
        println!("  {name}: collaborator");
    }
    for name in &acls.access_users.ticket {
        println!("  {name}: ticket");
    }

    println!("\nGroups:");
    let has_groups = !acls.access_groups.admin.is_empty()
        || !acls.access_groups.commit.is_empty()
        || !acls.access_groups.collaborator.is_empty()
        || !acls.access_groups.ticket.is_empty();

    if has_groups {
        for name in &acls.access_groups.admin {
            println!("  {name}: admin");
        }
        for name in &acls.access_groups.commit {
            println!("  {name}: commit");
        }
        for name in &acls.access_groups.collaborator {
            println!("  {name}: collaborator");
        }
        for name in &acls.access_groups.ticket {
            println!("  {name}: ticket");
        }
    } else {
        println!("  (none)");
    }

    Ok(())
}

async fn cmd_set(
    package: &str,
    user_type: &str,
    name: &str,
    level: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());
    client.set_acl(package, user_type, name, level).await?;
    println!("Set {user_type} '{name}' to '{level}' on {package}");
    Ok(())
}

async fn cmd_remove(
    package: &str,
    user_type: &str,
    name: &str,
    token: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());
    client.remove_acl(package, user_type, name).await?;
    println!("Removed {user_type} '{name}' from {package}");
    Ok(())
}

async fn cmd_apply(config_path: &PathBuf, token: &str) -> Result<(), Box<dyn std::error::Error>> {
    let config = AclConfig::from_file(config_path)?;
    let client = DistGitClient::new().with_token(token.to_string());
    let package = &config.package;

    let mut errors = Vec::new();

    for (name, level) in &config.users {
        let result = if level == "remove" {
            client.remove_acl(package, "user", name).await
        } else {
            client.set_acl(package, "user", name, level).await
        };
        match result {
            Ok(()) => {
                if level == "remove" {
                    println!("Removed user '{name}' from {package}");
                } else {
                    println!("Set user '{name}' to '{level}' on {package}");
                }
            }
            Err(e) => {
                eprintln!("error: user '{name}' on {package}: {e}");
                errors.push(e);
            }
        }
    }

    for (name, level) in &config.groups {
        let result = if level == "remove" {
            client.remove_acl(package, "group", name).await
        } else {
            client.set_acl(package, "group", name, level).await
        };
        match result {
            Ok(()) => {
                if level == "remove" {
                    println!("Removed group '{name}' from {package}");
                } else {
                    println!("Set group '{name}' to '{level}' on {package}");
                }
            }
            Err(e) => {
                eprintln!("error: group '{name}' on {package}: {e}");
                errors.push(e);
            }
        }
    }

    if !errors.is_empty() {
        return Err(format!("{} operation(s) failed", errors.len()).into());
    }

    Ok(())
}
