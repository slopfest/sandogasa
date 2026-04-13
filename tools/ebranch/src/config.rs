// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for ebranch.
//!
//! Stores the Bugzilla API key at `~/.config/ebranch/config.toml`
//! with restricted permissions (dir 700, file 600).

use serde::{Deserialize, Serialize};

/// Top-level config structure.
#[derive(Debug, Deserialize, Serialize)]
pub struct EbranchConfig {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
}

/// Bugzilla configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub url: String,
}

/// Load the Bugzilla API key, checking (in order):
/// 1. `--api-key` CLI flag
/// 2. `BUGZILLA_API_KEY` environment variable
/// 3. `~/.config/ebranch/config.toml`
///
/// Returns an error with setup instructions if none found.
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

    if let Ok(config) = sandogasa_config::ConfigFile::for_tool("ebranch").load::<EbranchConfig>()
        && !config.bugzilla.api_key.is_empty()
    {
        return Ok(config.bugzilla.api_key);
    }

    Err("Bugzilla API key not found.\n\
         Set it up with: ebranch config\n\
         Or pass --api-key or set BUGZILLA_API_KEY."
        .to_string())
}

/// Interactive config setup.
pub async fn cmd_config() -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
    let mut config: EbranchConfig = cf.load().unwrap_or(EbranchConfig {
        bugzilla: BugzillaConfig::default(),
    });

    println!("ebranch configuration\n");
    println!("Config file: {}\n", cf.path().display());

    // Bugzilla URL.
    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = "https://bugzilla.redhat.com".to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);

    // API key.
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

    // Validate the key with a minimal search.
    print!("Validating API key... ");
    let bz = sandogasa_bugzilla::BzClient::new(&config.bugzilla.url)
        .with_api_key(config.bugzilla.api_key.clone());

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
