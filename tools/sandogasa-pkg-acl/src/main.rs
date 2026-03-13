// SPDX-License-Identifier: MPL-2.0

mod config;

use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_config::ConfigFile;
use sandogasa_distgit::DistGitClient;
use serde::Serialize;

use config::{AclConfig, AppConfig, DistGitConfig};

const TOOL_NAME: &str = "sandogasa-pkg-acl";

/// View and manage Fedora package ACLs via the
/// Pagure dist-git API
#[derive(Parser)]
#[command(about)]
struct Cli {
    /// Output machine-readable JSON
    #[arg(long, global = true)]
    json: bool,

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
    },

    /// Set up or verify stored API token
    Config,

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

        /// ACL level
        /// (ticket, collaborator, commit, admin)
        #[arg(long, value_parser = parse_acl_level)]
        level: String,
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
    let json = cli.json;
    match cli.command {
        Command::Apply { config } => {
            let token = resolve_token()?;
            cmd_apply(&config, &token, json).await
        }
        Command::Config => cmd_config().await,
        Command::Remove {
            package,
            user,
            group,
        } => {
            let token = resolve_token()?;
            let (user_type, name) = resolve_target(user, group)?;
            cmd_remove(&package, &user_type, &name, &token, json).await
        }
        Command::Set {
            package,
            user,
            group,
            level,
        } => {
            let token = resolve_token()?;
            let (user_type, name) = resolve_target(user, group)?;
            cmd_set(&package, &user_type, &name, &level, &token, json).await
        }
        Command::Show { package } => cmd_show(&package, json).await,
    }
}

fn resolve_token() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(token) = std::env::var("PAGURE_API_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let cf = ConfigFile::for_tool(TOOL_NAME);
    let config: AppConfig = cf.load()?;
    Ok(config.dist_git.api_token)
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

async fn cmd_config() -> Result<(), Box<dyn std::error::Error>> {
    let cf = ConfigFile::for_tool(TOOL_NAME);

    match cf.load::<AppConfig>() {
        Ok(config) => {
            println!("Found config at {}", cf.path().display());
            print!("Verifying API token... ");
            io::stdout().flush()?;

            let client = DistGitClient::new().with_token(config.dist_git.api_token);
            match client.verify_token().await {
                Ok(username) => {
                    println!("OK (authenticated as {username})");
                    return Ok(());
                }
                Err(e) => {
                    println!("FAILED ({e})");
                    println!("Would you like to replace the token? [y/N] ");
                    io::stdout().flush()?;
                    let mut answer = String::new();
                    io::stdin().read_line(&mut answer)?;
                    if !answer.trim().eq_ignore_ascii_case("y") {
                        return Err("Token verification failed".into());
                    }
                }
            }
        }
        Err(_) => {
            println!("No config found at {}", cf.path().display());
        }
    }

    let token = sandogasa_config::prompt_field("dist-git", "API token", true, None)?;

    print!("Verifying token... ");
    io::stdout().flush()?;
    let client = DistGitClient::new().with_token(token.clone());
    let username = client.verify_token().await?;
    println!("OK (authenticated as {username})");

    let config = AppConfig {
        dist_git: DistGitConfig { api_token: token },
    };
    cf.save(&config)?;
    println!("Saved to {}", cf.path().display());

    Ok(())
}

async fn cmd_show(package: &str, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new();
    let acls = client.get_acls(package).await?;

    // Only fetch contributors (extra API call) when collaborators exist,
    // since that endpoint includes branch info for collaborators.
    let has_collaborators =
        !acls.access_users.collaborator.is_empty() || !acls.access_groups.collaborator.is_empty();
    let contribs = if has_collaborators {
        Some(client.get_contributors(package).await?)
    } else {
        None
    };

    if json {
        if let Some(contribs) = &contribs {
            // Merge owner info into the contributors response.
            let merged = serde_json::json!({
                "owner": acls.access_users.owner,
                "users": contribs.users,
                "groups": contribs.groups,
            });
            println!("{}", serde_json::to_string_pretty(&merged)?);
        } else {
            println!("{}", serde_json::to_string_pretty(&acls)?);
        }
        return Ok(());
    }

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
    if let Some(contribs) = &contribs {
        for collab in &contribs.users.collaborators {
            if let Some(branches) = collab.branches() {
                println!("  {}: collaborator ({})", collab.name(), branches);
            } else {
                println!("  {}: collaborator", collab.name());
            }
        }
    } else {
        for name in &acls.access_users.collaborator {
            println!("  {name}: collaborator");
        }
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
        if let Some(contribs) = &contribs {
            for collab in &contribs.groups.collaborators {
                if let Some(branches) = collab.branches() {
                    println!("  {}: collaborator ({})", collab.name(), branches);
                } else {
                    println!("  {}: collaborator", collab.name());
                }
            }
        } else {
            for name in &acls.access_groups.collaborator {
                println!("  {name}: collaborator");
            }
        }
        for name in &acls.access_groups.ticket {
            println!("  {name}: ticket");
        }
    } else {
        println!("  (none)");
    }

    Ok(())
}

#[derive(Serialize)]
struct AclChange {
    package: String,
    user_type: String,
    name: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
}

async fn cmd_set(
    package: &str,
    user_type: &str,
    name: &str,
    level: &str,
    token: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());
    client.set_acl(package, user_type, name, level).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&AclChange {
                package: package.to_string(),
                user_type: user_type.to_string(),
                name: name.to_string(),
                action: "set".to_string(),
                level: Some(level.to_string()),
            })?
        );
    } else {
        println!("Set {user_type} '{name}' to '{level}' on {package}");
    }
    Ok(())
}

async fn cmd_remove(
    package: &str,
    user_type: &str,
    name: &str,
    token: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());
    client.remove_acl(package, user_type, name).await?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&AclChange {
                package: package.to_string(),
                user_type: user_type.to_string(),
                name: name.to_string(),
                action: "remove".to_string(),
                level: None,
            })?
        );
    } else {
        println!("Removed {user_type} '{name}' from {package}");
    }
    Ok(())
}

#[derive(Serialize)]
struct ApplyResult {
    package: String,
    results: Vec<ApplyEntry>,
}

#[derive(Serialize)]
struct ApplyEntry {
    user_type: String,
    name: String,
    action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    level: Option<String>,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

async fn cmd_apply(
    config_path: &PathBuf,
    token: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = AclConfig::from_file(config_path)?;
    let client = DistGitClient::new().with_token(token.to_string());
    let package = &config.package;

    let mut errors = Vec::new();
    let mut entries = Vec::new();

    for (name, level) in &config.users {
        let is_remove = level == "remove";
        let result = if is_remove {
            client.remove_acl(package, "user", name).await
        } else {
            client.set_acl(package, "user", name, level).await
        };
        match &result {
            Ok(()) => {
                if !json {
                    if is_remove {
                        println!("Removed user '{name}' from {package}");
                    } else {
                        println!("Set user '{name}' to '{level}' on {package}");
                    }
                }
            }
            Err(e) => {
                if !json {
                    eprintln!("error: user '{name}' on {package}: {e}");
                }
            }
        }
        if json {
            entries.push(ApplyEntry {
                user_type: "user".to_string(),
                name: name.clone(),
                action: if is_remove {
                    "remove".to_string()
                } else {
                    "set".to_string()
                },
                level: if is_remove { None } else { Some(level.clone()) },
                ok: result.is_ok(),
                error: result.as_ref().err().map(|e| e.to_string()),
            });
        }
        if let Err(e) = result {
            errors.push(e);
        }
    }

    for (name, level) in &config.groups {
        let is_remove = level == "remove";
        let result = if is_remove {
            client.remove_acl(package, "group", name).await
        } else {
            client.set_acl(package, "group", name, level).await
        };
        match &result {
            Ok(()) => {
                if !json {
                    if is_remove {
                        println!("Removed group '{name}' from {package}");
                    } else {
                        println!("Set group '{name}' to '{level}' on {package}");
                    }
                }
            }
            Err(e) => {
                if !json {
                    eprintln!("error: group '{name}' on {package}: {e}");
                }
            }
        }
        if json {
            entries.push(ApplyEntry {
                user_type: "group".to_string(),
                name: name.clone(),
                action: if is_remove {
                    "remove".to_string()
                } else {
                    "set".to_string()
                },
                level: if is_remove { None } else { Some(level.clone()) },
                ok: result.is_ok(),
                error: result.as_ref().err().map(|e| e.to_string()),
            });
        }
        if let Err(e) = result {
            errors.push(e);
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&ApplyResult {
                package: package.clone(),
                results: entries,
            })?
        );
    }

    if !errors.is_empty() {
        return Err(format!("{} operation(s) failed", errors.len()).into());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_change_set_serializes_with_level() {
        let change = AclChange {
            package: "freerdp".to_string(),
            user_type: "user".to_string(),
            name: "salimma".to_string(),
            action: "set".to_string(),
            level: Some("commit".to_string()),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&change).unwrap()).unwrap();
        assert_eq!(json["package"], "freerdp");
        assert_eq!(json["user_type"], "user");
        assert_eq!(json["name"], "salimma");
        assert_eq!(json["action"], "set");
        assert_eq!(json["level"], "commit");
    }

    #[test]
    fn acl_change_remove_omits_level() {
        let change = AclChange {
            package: "freerdp".to_string(),
            user_type: "group".to_string(),
            name: "kde-sig".to_string(),
            action: "remove".to_string(),
            level: None,
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&change).unwrap()).unwrap();
        assert_eq!(json["action"], "remove");
        assert!(json.get("level").is_none());
    }

    #[test]
    fn apply_result_serializes() {
        let result = ApplyResult {
            package: "freerdp".to_string(),
            results: vec![
                ApplyEntry {
                    user_type: "user".to_string(),
                    name: "salimma".to_string(),
                    action: "set".to_string(),
                    level: Some("commit".to_string()),
                    ok: true,
                    error: None,
                },
                ApplyEntry {
                    user_type: "user".to_string(),
                    name: "olduser".to_string(),
                    action: "remove".to_string(),
                    level: None,
                    ok: true,
                    error: None,
                },
            ],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&result).unwrap()).unwrap();
        assert_eq!(json["package"], "freerdp");
        let results = json["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["name"], "salimma");
        assert_eq!(results[0]["ok"], true);
        assert!(results[0].get("error").is_none());
        assert_eq!(results[1]["action"], "remove");
        assert!(results[1].get("level").is_none());
    }

    #[test]
    fn apply_entry_with_error_serializes() {
        let entry = ApplyEntry {
            user_type: "user".to_string(),
            name: "baduser".to_string(),
            action: "set".to_string(),
            level: Some("admin".to_string()),
            ok: false,
            error: Some("403 Forbidden".to_string()),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&entry).unwrap()).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"], "403 Forbidden");
        assert_eq!(json["level"], "admin");
    }

    #[test]
    fn resolve_target_user() {
        let (t, n) = resolve_target(Some("alice".to_string()), None).unwrap();
        assert_eq!(t, "user");
        assert_eq!(n, "alice");
    }

    #[test]
    fn resolve_target_group() {
        let (t, n) = resolve_target(None, Some("kde-sig".to_string())).unwrap();
        assert_eq!(t, "group");
        assert_eq!(n, "kde-sig");
    }

    #[test]
    fn resolve_target_neither_errors() {
        assert!(resolve_target(None, None).is_err());
    }

    #[test]
    fn parse_acl_level_valid() {
        for level in &["ticket", "collaborator", "commit", "admin"] {
            assert_eq!(parse_acl_level(level).unwrap(), *level);
        }
    }

    #[test]
    fn parse_acl_level_invalid() {
        assert!(parse_acl_level("owner").is_err());
        assert!(parse_acl_level("superadmin").is_err());
    }
}
