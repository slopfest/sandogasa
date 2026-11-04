// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for poi-tracker.
//!
//! Stores the Bugzilla API key at
//! `~/.config/poi-tracker/config.toml` with restricted
//! permissions (handled by `sandogasa-config`). Mirrors the
//! `ebranch` shape so a future refactor can fold both into a
//! shared crate without changing the on-disk format.

use serde::{Deserialize, Serialize};

/// Top-level config structure.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PoiTrackerConfig {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
    #[serde(default, rename = "dist-git")]
    pub dist_git: DistGitConfig,
}

/// Dist-git (Pagure) configuration — the `[dist-git]` table,
/// matching sandogasa-pkg-acl's shape.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DistGitConfig {
    /// API token with the `modify_project` ACL, for `adopt`.
    #[serde(default)]
    pub api_token: String,
}

/// Bugzilla configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub url: String,
}

impl BugzillaConfig {
    /// Default Bugzilla instance for Fedora / EPEL bugs.
    pub const DEFAULT_URL: &'static str = "https://bugzilla.redhat.com";
}

/// Load the Bugzilla API key, checking in order:
/// 1. `--api-key` CLI flag
/// 2. `BUGZILLA_API_KEY` environment variable
/// 3. `~/.config/poi-tracker/config.toml`
///
/// Returns an error with setup instructions when nothing is set.
pub fn resolve_api_key(cli_key: Option<&str>) -> Result<String, String> {
    if let Some(key) = cli_key
        && !key.is_empty()
    {
        return Ok(key.to_string());
    }
    if let Ok(key) = std::env::var("BUGZILLA_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }
    if let Ok(config) =
        sandogasa_config::ConfigFile::for_tool("poi-tracker").load::<PoiTrackerConfig>()
        && !config.bugzilla.api_key.is_empty()
    {
        return Ok(config.bugzilla.api_key);
    }
    Err("Bugzilla API key not found.\n\
         Set it up with: poi-tracker config\n\
         Or pass --api-key or set BUGZILLA_API_KEY."
        .to_string())
}

/// Load the dist-git (Pagure) API token, checking in order:
/// 1. `--api-token` CLI flag
/// 2. `PAGURE_API_TOKEN` environment variable (pkg-acl's
///    convention)
/// 3. `~/.config/poi-tracker/config.toml` `[dist-git] api_token`
///
/// Returns an error with setup instructions when nothing is set.
pub fn resolve_distgit_token(cli_token: Option<&str>) -> Result<String, String> {
    if let Some(token) = cli_token
        && !token.is_empty()
    {
        return Ok(token.to_string());
    }
    if let Ok(token) = std::env::var("PAGURE_API_TOKEN")
        && !token.is_empty()
    {
        return Ok(token);
    }
    if let Ok(config) =
        sandogasa_config::ConfigFile::for_tool("poi-tracker").load::<PoiTrackerConfig>()
        && !config.dist_git.api_token.is_empty()
    {
        return Ok(config.dist_git.api_token);
    }
    Err("dist-git API token not found.\n\
         Generate one at https://src.fedoraproject.org/settings/token/new\n\
         with the \"Modify an existing project\" ACL, then set it up\n\
         with: poi-tracker config\n\
         Or pass --api-token or set PAGURE_API_TOKEN."
        .to_string())
}

/// Load the Bugzilla base URL: config file first, then the
/// hardcoded default. (No CLI override; the URL is per-instance
/// and rarely changes day-to-day.)
pub fn resolve_url() -> String {
    sandogasa_config::ConfigFile::for_tool("poi-tracker")
        .load::<PoiTrackerConfig>()
        .ok()
        .filter(|c| !c.bugzilla.url.is_empty())
        .map(|c| c.bugzilla.url)
        .unwrap_or_else(|| BugzillaConfig::DEFAULT_URL.to_string())
}

/// Read the user's configured Bugzilla email (used to claim
/// ownership when closing bugs). Returns `None` if the file
/// isn't present, can't be parsed, or has no email set.
pub fn resolve_email() -> Option<String> {
    sandogasa_config::ConfigFile::for_tool("poi-tracker")
        .load::<PoiTrackerConfig>()
        .ok()
        .map(|c| c.bugzilla.email)
        .filter(|e| !e.is_empty())
}

/// Interactive config setup. Prompts for what is missing (with
/// `all`, for every setting), validates the Bugzilla API key, and
/// writes the result.
pub async fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    use sandogasa_config::{Setting, ask};
    let cf = sandogasa_config::ConfigFile::for_tool("poi-tracker");
    let mut config: PoiTrackerConfig = cf.load_user().unwrap_or_default();

    println!("poi-tracker configuration\n");
    println!("Config file: {}\n", cf.path().display());

    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = BugzillaConfig::DEFAULT_URL.to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);

    config.bugzilla.api_key = ask(
        &Setting::new("Bugzilla", "API key").sensitive().hint(
            "\nGenerate an API key at:\n  \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey\n",
        ),
        &config.bugzilla.api_key,
        args.all,
    )
    .map_err(|e| format!("failed to read API key: {e}"))?;

    // Email is optional — it's only used by `triage-retired
    // --claim` to set `assigned_to` on closed bugs.
    config.bugzilla.email = ask(
        &Setting::new("Bugzilla", "email (optional, for --claim)")
            .optional()
            .validated(&sandogasa_config::validate_email),
        &config.bugzilla.email,
        args.all,
    )
    .map_err(|e| format!("failed to read e-mail: {e}"))?;

    // The dist-git token is optional — only `adopt` needs it.
    config.dist_git.api_token = ask(
        &Setting::new("dist-git", "API token (optional, empty to skip)")
            .sensitive()
            .optional()
            .hint(
                "\nOptional: a dist-git API token lets `adopt` take orphaned\n\
                 packages. Generate one at\n  \
                 https://src.fedoraproject.org/settings/token/new\n\
                 with the \"Modify an existing project\" ACL.",
            ),
        &config.dist_git.api_token,
        args.all,
    )
    .map_err(|e| format!("failed to read token: {e}"))?;

    // Checked by trying to use it, not by searching: Bugzilla serves
    // bug searches to anyone, so the old check passed for a key that
    // could never write. `api_key_accepted` asks to create a bug with
    // no fields, which the credential is checked before, and which
    // cannot create anything because every required field is absent.
    let url = config.bugzilla.url.clone();
    config.bugzilla.api_key = sandogasa_cli::credential::validated_key(
        "Bugzilla",
        "API key",
        config.bugzilla.api_key.clone(),
        move |key| {
            let url = url.clone();
            Box::pin(async move {
                use sandogasa_cli::credential::KeyVerdict;
                let bz = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(key) {
                    Ok(bz) => bz,
                    Err(e) => return KeyVerdict::Unknown(format!("could not check: {e}")),
                };
                match bz.api_key_accepted().await {
                    Ok(true) => KeyVerdict::Accepted("accepted.".to_string()),
                    Ok(false) => KeyVerdict::Refused("refused.".to_string()),
                    Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
                }
            })
        },
    )
    .await
    .key;

    cf.save(&config)
        .map_err(|e| format!("failed to save config: {e}"))?;
    println!("\nConfig saved to {}", cf.path().display());
    Ok(())
}
