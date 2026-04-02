// SPDX-License-Identifier: MPL-2.0

mod config;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use sandogasa_config::ConfigFile;
use sandogasa_distgit::{AccessLevel, AccessResult, DistGitClient};
use serde::Serialize;

use config::{AclConfig, AppConfig, DistGitConfig};

const TOOL_NAME: &str = "sandogasa-pkg-acl";

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
        config: PathBuf,

        /// Package name(s)
        #[arg(required = true)]
        packages: Vec<String>,

        /// Downgrade access if already higher than requested
        #[arg(long)]
        strict: bool,
    },

    /// Set up or verify stored API token
    Config,

    /// Give package ownership to another user
    Give {
        /// Username of new owner
        user: String,

        /// Package name(s)
        #[arg(required = true)]
        packages: Vec<String>,
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

        /// Downgrade access if already higher than requested
        #[arg(long)]
        strict: bool,
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
        Command::Apply {
            config,
            packages,
            strict,
        } => {
            let token = resolve_token()?;
            cmd_apply(&config, &packages, &token, strict, json).await
        }
        Command::Config => cmd_config().await,
        Command::Give { user, packages } => {
            let token = resolve_token()?;
            cmd_give(&packages, &user, &token, json).await
        }
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
            strict,
        } => {
            let token = resolve_token()?;
            let (user_type, name) = resolve_target(user, group)?;
            cmd_set(&package, &user_type, &name, &level, &token, strict, json).await
        }
        Command::Show { package } => cmd_show(&package, json).await,
    }
}

fn resolve_token() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(token) = std::env::var("PAGURE_API_TOKEN")
        && !token.is_empty()
    {
        return Ok(token);
    }
    let cf = ConfigFile::for_tool(TOOL_NAME);
    let config: AppConfig = cf.load()?;
    Ok(config.dist_git.api_token)
}

/// Return the cached username from the config file, if available.
fn resolve_username() -> Option<String> {
    let cf = ConfigFile::for_tool(TOOL_NAME);
    let config: AppConfig = cf.load().ok()?;
    if config.dist_git.username.is_empty() {
        None
    } else {
        Some(config.dist_git.username)
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

async fn cmd_config() -> Result<(), Box<dyn std::error::Error>> {
    let cf = ConfigFile::for_tool(TOOL_NAME);

    match cf.load::<AppConfig>() {
        Ok(config) => {
            println!("Found config at {}", cf.path().display());
            print!("Verifying API token... ");
            io::stdout().flush()?;

            let client = DistGitClient::new().with_token(config.dist_git.api_token.clone());
            match client.verify_token().await {
                Ok(username) => {
                    println!("OK (authenticated as {username})");
                    cache_username(&username);
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
        dist_git: DistGitConfig {
            api_token: token,
            username,
        },
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

    // Check current user's access if a token is available
    let my_access = check_my_access(&client, &acls).await;

    if json {
        let mut output = if let Some(contribs) = &contribs {
            serde_json::json!({
                "owner": acls.access_users.owner,
                "users": contribs.users,
                "groups": contribs.groups,
            })
        } else {
            serde_json::to_value(&acls)?
        };
        if let Some((username, ref result)) = my_access {
            let access_obj = match result {
                AccessResult::Direct(level) => serde_json::json!({
                    "username": username,
                    "level": level.to_string(),
                    "source": "direct",
                }),
                AccessResult::ViaGroup { level, group } => serde_json::json!({
                    "username": username,
                    "level": level.to_string(),
                    "source": "group",
                    "group": group,
                }),
                AccessResult::Insufficient { level } => serde_json::json!({
                    "username": username,
                    "level": level.map(|l| l.to_string()),
                    "source": serde_json::Value::Null,
                }),
            };
            output["your_access"] = access_obj;
        }
        println!("{}", serde_json::to_string_pretty(&output)?);
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

    if let Some((username, result)) = my_access {
        match result {
            AccessResult::Direct(level) => {
                println!("\nYour access ({username}): {level}");
            }
            AccessResult::ViaGroup { level, group } => {
                println!("\nYour access ({username}): {level} (via {group})");
            }
            AccessResult::Insufficient { level: Some(level) } => {
                println!("\nYour access ({username}): {level}");
            }
            AccessResult::Insufficient { level: None } => {
                println!("\nYour access ({username}): none");
            }
        }
    }

    Ok(())
}

/// If a token/username is available, check the user's access level.
///
/// Uses the cached username from the config file when available,
/// falling back to `verify_token()` only if needed.
async fn check_my_access(
    client: &DistGitClient,
    acls: &sandogasa_distgit::ProjectAcls,
) -> Option<(String, AccessResult)> {
    let username = match resolve_username() {
        Some(u) => u,
        None => {
            let token = resolve_token().ok()?;
            let auth_client = DistGitClient::new().with_token(token);
            let u = auth_client.verify_token().await.ok()?;
            cache_username(&u);
            u
        }
    };
    let result = client
        .check_access(acls, &username, AccessLevel::Admin)
        .await
        .ok()?;
    Some((username, result))
}

/// Verify that the current user has admin access on a package.
///
/// Fetches ACLs, resolves the username, and checks access level.
/// Returns the ACLs on success (for reuse by callers), or an error
/// if the user does not have admin (or owner) access.
async fn require_admin(
    client: &DistGitClient,
    package: &str,
) -> Result<sandogasa_distgit::ProjectAcls, Box<dyn std::error::Error>> {
    let acls = client.get_acls(package).await?;
    let username = match resolve_username() {
        Some(u) => u,
        None => {
            let u = client.verify_token().await?;
            cache_username(&u);
            u
        }
    };
    let result = client
        .check_access(&acls, &username, AccessLevel::Admin)
        .await?;
    if result.is_sufficient() {
        Ok(acls)
    } else {
        let current = match result {
            AccessResult::Insufficient { level: Some(l) } => l.to_string(),
            _ => "none".to_string(),
        };
        Err(format!(
            "{username} does not have admin access on {package} \
             (current level: {current})"
        )
        .into())
    }
}

/// Verify that the current user is the package owner.
///
/// Returns the ACLs on success, or an error if the user is not the owner.
async fn require_owner(
    client: &DistGitClient,
    package: &str,
) -> Result<sandogasa_distgit::ProjectAcls, Box<dyn std::error::Error>> {
    let acls = client.get_acls(package).await?;
    let username = match resolve_username() {
        Some(u) => u,
        None => {
            let u = client.verify_token().await?;
            cache_username(&u);
            u
        }
    };
    if acls.access_users.owner.iter().any(|o| o == &username) {
        Ok(acls)
    } else {
        Err(format!("{username} is not the owner of {package}").into())
    }
}

/// Save the username into the config file if it's not already cached.
fn cache_username(username: &str) {
    let cf = ConfigFile::for_tool(TOOL_NAME);
    if let Ok(config) = cf.load::<AppConfig>()
        && config.dist_git.username != username
    {
        let updated = AppConfig {
            dist_git: DistGitConfig {
                api_token: config.dist_git.api_token,
                username: username.to_string(),
            },
        };
        let _ = cf.save(&updated);
    }
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

async fn cmd_give(
    packages: &[String],
    new_owner: &str,
    token: &str,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());

    // Validate the target user exists (once, before any transfers)
    if !client.user_exists(new_owner).await? {
        return Err(format!("user '{new_owner}' does not exist").into());
    }

    let mut errors = Vec::new();
    let mut entries = Vec::new();

    for package in packages {
        if let Err(e) = require_owner(&client, package).await {
            if !json {
                eprintln!("error: {package}: {e}");
            }
            if json {
                entries.push(serde_json::json!({
                    "package": package,
                    "ok": false,
                    "error": e.to_string(),
                }));
            }
            errors.push(e);
            continue;
        }

        match client.give_package(package, new_owner).await {
            Ok(()) => {
                if !json {
                    println!("Gave {package} to '{new_owner}'");
                }
                if json {
                    entries.push(serde_json::json!({
                        "package": package,
                        "ok": true,
                    }));
                }
            }
            Err(e) => {
                if !json {
                    eprintln!("error: {package}: {e}");
                }
                if json {
                    entries.push(serde_json::json!({
                        "package": package,
                        "ok": false,
                        "error": e.to_string(),
                    }));
                }
                errors.push(e);
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "give",
                "new_owner": new_owner,
                "results": entries,
            }))?
        );
    }

    if !errors.is_empty() {
        return Err(format!("{} package(s) failed", errors.len()).into());
    }
    Ok(())
}

async fn cmd_set(
    package: &str,
    user_type: &str,
    name: &str,
    level: &str,
    token: &str,
    strict: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DistGitClient::new().with_token(token.to_string());
    let acls = require_admin(&client, package).await?;

    let requested: AccessLevel = level.parse().unwrap();
    let current = if user_type == "user" {
        acls.user_level(name)
    } else {
        acls.group_level(name)
    };

    if let Some(current) = current {
        // Cannot modify a package owner
        if current == AccessLevel::Owner {
            return Err(format!(
                "cannot modify {user_type} '{name}' on {package}: \
                 package owner"
            )
            .into());
        }
        if current == requested {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&AclChange {
                        package: package.to_string(),
                        user_type: user_type.to_string(),
                        name: name.to_string(),
                        action: "skipped".to_string(),
                        level: Some(level.to_string()),
                    })?
                );
            } else {
                println!(
                    "Skipped {user_type} '{name}' on {package}: \
                     already has {current}"
                );
            }
            return Ok(());
        }
        if current > requested && !strict {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&AclChange {
                        package: package.to_string(),
                        user_type: user_type.to_string(),
                        name: name.to_string(),
                        action: "skipped".to_string(),
                        level: Some(current.to_string()),
                    })?
                );
            } else {
                println!(
                    "Skipped {user_type} '{name}' on {package}: \
                     already has {current} (use --strict to downgrade)"
                );
            }
            return Ok(());
        }
    }

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
    let acls = require_admin(&client, package).await?;
    // Cannot remove a package owner
    if user_type == "user" && acls.user_level(name) == Some(AccessLevel::Owner) {
        return Err(format!(
            "cannot remove user '{name}' from {package}: \
             package owner"
        )
        .into());
    }
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
    config_path: &Path,
    packages: &[String],
    token: &str,
    strict: bool,
    json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = AclConfig::from_file(config_path)?;
    let client = DistGitClient::new().with_token(token.to_string());

    let mut all_results = Vec::new();
    let mut total_errors = 0usize;

    for package in packages {
        let acls = match require_admin(&client, package).await {
            Ok(acls) => acls,
            Err(e) => {
                if !json {
                    eprintln!("error: {package}: {e}");
                }
                if json {
                    all_results.push(ApplyResult {
                        package: package.clone(),
                        results: vec![ApplyEntry {
                            user_type: String::new(),
                            name: String::new(),
                            action: "error".to_string(),
                            level: None,
                            ok: false,
                            error: Some(e.to_string()),
                        }],
                    });
                }
                total_errors += 1;
                continue;
            }
        };

        let mut errors = Vec::new();
        let mut entries = Vec::new();

        for (name, level) in &config.users {
            let is_remove = level == "remove";

            // Cannot modify a package owner
            if acls.user_level(name) == Some(AccessLevel::Owner) {
                if !json {
                    eprintln!(
                        "Skipped user '{name}' on {package}: \
                         cannot modify package owner"
                    );
                }
                if json {
                    entries.push(ApplyEntry {
                        user_type: "user".to_string(),
                        name: name.clone(),
                        action: "skipped".to_string(),
                        level: Some("owner".to_string()),
                        ok: false,
                        error: Some("cannot modify package owner".to_string()),
                    });
                }
                continue;
            }

            // Check for skip (only when setting, not removing)
            if !is_remove
                && let Some(action) = check_skip(
                    "user",
                    name,
                    level,
                    acls.user_level(name),
                    strict,
                    package,
                    json,
                )
            {
                if json {
                    entries.push(action);
                }
                continue;
            }

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

            if !is_remove
                && let Some(action) = check_skip(
                    "group",
                    name,
                    level,
                    acls.group_level(name),
                    strict,
                    package,
                    json,
                )
            {
                if json {
                    entries.push(action);
                }
                continue;
            }

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

        total_errors += errors.len();

        if json {
            all_results.push(ApplyResult {
                package: package.clone(),
                results: entries,
            });
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&all_results)?);
    }

    if total_errors > 0 {
        return Err(format!("{total_errors} operation(s) failed").into());
    }

    Ok(())
}

/// Check if a set operation should be skipped because the target
/// already has equal or higher access. Returns `Some(ApplyEntry)` if
/// skipped (for JSON output), or `None` to proceed.
fn check_skip(
    user_type: &str,
    name: &str,
    requested_level: &str,
    current: Option<AccessLevel>,
    strict: bool,
    package: &str,
    json: bool,
) -> Option<ApplyEntry> {
    let requested: AccessLevel = requested_level.parse().unwrap();
    let current = current?;

    // Cannot modify a package owner
    if current == AccessLevel::Owner {
        if !json {
            eprintln!(
                "Skipped {user_type} '{name}' on {package}: \
                 cannot modify package owner"
            );
        }
        return Some(ApplyEntry {
            user_type: user_type.to_string(),
            name: name.to_string(),
            action: "skipped".to_string(),
            level: Some("owner".to_string()),
            ok: false,
            error: Some("cannot modify package owner".to_string()),
        });
    }

    if current == requested {
        if !json {
            println!(
                "Skipped {user_type} '{name}' on {package}: \
                 already has {current}"
            );
        }
        return Some(ApplyEntry {
            user_type: user_type.to_string(),
            name: name.to_string(),
            action: "skipped".to_string(),
            level: Some(current.to_string()),
            ok: true,
            error: None,
        });
    }

    if current > requested && !strict {
        if !json {
            println!(
                "Skipped {user_type} '{name}' on {package}: \
                 already has {current} (use --strict to downgrade)"
            );
        }
        return Some(ApplyEntry {
            user_type: user_type.to_string(),
            name: name.to_string(),
            action: "skipped".to_string(),
            level: Some(current.to_string()),
            ok: true,
            error: None,
        });
    }

    None
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

    // ---- check_skip owner protection ----

    #[test]
    fn check_skip_owner_always_skips() {
        // Even with strict=true, owner cannot be modified
        for strict in [false, true] {
            let result = check_skip(
                "user",
                "pkgowner",
                "commit",
                Some(AccessLevel::Owner),
                strict,
                "mypkg",
                true,
            );
            let entry = result.expect("should skip owner");
            assert_eq!(entry.action, "skipped");
            assert_eq!(entry.level, Some("owner".to_string()));
            assert!(!entry.ok);
            assert!(entry.error.as_ref().unwrap().contains("package owner"));
        }
    }

    #[test]
    fn check_skip_same_level() {
        let result = check_skip(
            "user",
            "alice",
            "commit",
            Some(AccessLevel::Commit),
            false,
            "mypkg",
            true,
        );
        let entry = result.expect("should skip same level");
        assert_eq!(entry.action, "skipped");
        assert!(entry.ok);
    }

    #[test]
    fn check_skip_higher_without_strict() {
        let result = check_skip(
            "user",
            "alice",
            "ticket",
            Some(AccessLevel::Admin),
            false,
            "mypkg",
            true,
        );
        let entry = result.expect("should skip higher level");
        assert_eq!(entry.action, "skipped");
        assert!(entry.ok);
    }

    #[test]
    fn check_skip_higher_with_strict_proceeds() {
        let result = check_skip(
            "user",
            "alice",
            "ticket",
            Some(AccessLevel::Admin),
            true,
            "mypkg",
            true,
        );
        assert!(result.is_none(), "strict should proceed with downgrade");
    }

    #[test]
    fn check_skip_lower_proceeds() {
        let result = check_skip(
            "user",
            "alice",
            "admin",
            Some(AccessLevel::Commit),
            false,
            "mypkg",
            true,
        );
        assert!(result.is_none(), "should proceed to upgrade");
    }

    #[test]
    fn check_skip_no_current_proceeds() {
        let result = check_skip("user", "alice", "commit", None, false, "mypkg", true);
        assert!(result.is_none(), "should proceed for new user");
    }

    // ---- check_skip text output paths (json=false) ----

    #[test]
    fn check_skip_owner_text_output() {
        let result = check_skip(
            "user",
            "pkgowner",
            "commit",
            Some(AccessLevel::Owner),
            false,
            "mypkg",
            false,
        );
        let entry = result.expect("should skip owner");
        assert!(!entry.ok);
        assert!(entry.error.is_some());
    }

    #[test]
    fn check_skip_same_level_text_output() {
        let result = check_skip(
            "user",
            "alice",
            "commit",
            Some(AccessLevel::Commit),
            false,
            "mypkg",
            false,
        );
        let entry = result.expect("should skip same level");
        assert!(entry.ok);
    }

    #[test]
    fn check_skip_higher_text_output() {
        let result = check_skip(
            "user",
            "alice",
            "ticket",
            Some(AccessLevel::Admin),
            false,
            "mypkg",
            false,
        );
        let entry = result.expect("should skip higher level");
        assert!(entry.ok);
        assert_eq!(entry.level, Some("admin".to_string()));
    }

    // ---- apply entry owner skip serialization ----

    #[test]
    fn apply_entry_owner_skip_serializes() {
        let entry = ApplyEntry {
            user_type: "user".to_string(),
            name: "pkgowner".to_string(),
            action: "skipped".to_string(),
            level: Some("owner".to_string()),
            ok: false,
            error: Some("cannot modify package owner".to_string()),
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&entry).unwrap()).unwrap();
        assert_eq!(json["action"], "skipped");
        assert_eq!(json["level"], "owner");
        assert_eq!(json["ok"], false);
        assert!(json["error"].as_str().unwrap().contains("package owner"));
    }

    #[test]
    fn resolve_target_both_errors() {
        assert!(resolve_target(Some("a".to_string()), Some("b".to_string())).is_err());
    }

    // ---- check_skip with different access level combinations ----

    #[test]
    fn check_skip_collaborator_to_ticket_without_strict() {
        let result = check_skip(
            "group",
            "kde-sig",
            "ticket",
            Some(AccessLevel::Collaborator),
            false,
            "mypkg",
            true,
        );
        let entry = result.expect("should skip higher level");
        assert_eq!(entry.level, Some("collaborator".to_string()));
        assert_eq!(entry.user_type, "group");
    }

    #[test]
    fn check_skip_ticket_to_admin_proceeds() {
        let result = check_skip(
            "user",
            "alice",
            "admin",
            Some(AccessLevel::Ticket),
            false,
            "mypkg",
            true,
        );
        assert!(result.is_none(), "should proceed to upgrade");
    }

    #[test]
    fn check_skip_same_ticket_level() {
        let result = check_skip(
            "group",
            "python-sig",
            "ticket",
            Some(AccessLevel::Ticket),
            false,
            "mypkg",
            false,
        );
        let entry = result.expect("should skip same level");
        assert_eq!(entry.level, Some("ticket".to_string()));
        assert_eq!(entry.name, "python-sig");
    }

    // ---- multi-package apply result serialization ----

    #[test]
    fn apply_results_array_serializes() {
        let results = vec![
            ApplyResult {
                package: "pkg-a".to_string(),
                results: vec![ApplyEntry {
                    user_type: "user".to_string(),
                    name: "alice".to_string(),
                    action: "set".to_string(),
                    level: Some("commit".to_string()),
                    ok: true,
                    error: None,
                }],
            },
            ApplyResult {
                package: "pkg-b".to_string(),
                results: vec![ApplyEntry {
                    user_type: "group".to_string(),
                    name: "kde-sig".to_string(),
                    action: "set".to_string(),
                    level: Some("admin".to_string()),
                    ok: true,
                    error: None,
                }],
            },
        ];
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&results).unwrap()).unwrap();
        let arr = json.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["package"], "pkg-a");
        assert_eq!(arr[0]["results"][0]["name"], "alice");
        assert_eq!(arr[1]["package"], "pkg-b");
        assert_eq!(arr[1]["results"][0]["name"], "kde-sig");
    }

    #[test]
    fn check_skip_admin_to_commit_with_strict_proceeds() {
        let result = check_skip(
            "user",
            "alice",
            "commit",
            Some(AccessLevel::Admin),
            true,
            "mypkg",
            false,
        );
        assert!(result.is_none(), "strict should allow downgrade");
    }

    #[test]
    fn apply_result_with_error_entry_serializes() {
        let result = ApplyResult {
            package: "pkg-a".to_string(),
            results: vec![ApplyEntry {
                user_type: String::new(),
                name: String::new(),
                action: "error".to_string(),
                level: None,
                ok: false,
                error: Some("not an admin".to_string()),
            }],
        };
        let json: serde_json::Value =
            serde_json::from_str(&serde_json::to_string_pretty(&result).unwrap()).unwrap();
        assert_eq!(json["results"][0]["action"], "error");
        assert_eq!(json["results"][0]["ok"], false);
        assert_eq!(json["results"][0]["error"], "not an admin");
    }
}
