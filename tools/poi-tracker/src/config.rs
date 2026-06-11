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

/// Interactive config setup. Prompts for the Bugzilla API key,
/// validates it with a minimal search, and writes the result.
pub async fn cmd_config() -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("poi-tracker");
    let mut config: PoiTrackerConfig = cf.load().unwrap_or_default();

    println!("poi-tracker configuration\n");
    println!("Config file: {}\n", cf.path().display());

    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = BugzillaConfig::DEFAULT_URL.to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);

    if config.bugzilla.api_key.is_empty() {
        println!(
            "\nGenerate an API key at:\n  \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey\n"
        );
        let key = sandogasa_config::prompt_field("Bugzilla", "API key", true, None)
            .map_err(|e| format!("failed to read API key: {e}"))?;
        config.bugzilla.api_key = key;
    } else {
        println!("Bugzilla API key: (set)");
    }

    // Email is optional — it's only used by `triage-retired
    // --claim` to set `assigned_to` on closed bugs. Blank input
    // keeps the current value (which may itself be empty).
    let current = if config.bugzilla.email.is_empty() {
        "<unset>"
    } else {
        config.bugzilla.email.as_str()
    };
    print!("Bugzilla email [{current}] (for --claim; blank to keep): ");
    use std::io::{BufRead, Write};
    std::io::stdout()
        .flush()
        .map_err(|e| format!("flush: {e}"))?;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("read: {e}"))?;
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        config.bugzilla.email = trimmed.to_string();
    }

    print!("Validating API key... ");
    let bz = sandogasa_bugzilla::BzClient::new(&config.bugzilla.url)
        .with_api_key(config.bugzilla.api_key.clone())
        .map_err(|e| e.to_string())?;
    match bz.search("product=Fedora&limit=1", 1).await {
        Ok(_) => println!("valid."),
        Err(e) => {
            println!("failed.");
            eprintln!("warning: {e}");
            eprintln!("The key was saved but may not work.");
        }
    }

    cf.save(&config)
        .map_err(|e| format!("failed to save config: {e}"))?;
    println!("\nConfig saved to {}", cf.path().display());
    Ok(())
}
