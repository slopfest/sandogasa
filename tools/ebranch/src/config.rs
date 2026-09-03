// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for ebranch.
//!
//! Stores the Bugzilla API key at `~/.config/ebranch/config.toml`
//! with restricted permissions (dir 700, file 600), and carries
//! standing preferences such as the crates `check-crate` ignores.

use serde::{Deserialize, Serialize};

/// Top-level config structure.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EbranchConfig {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
    /// `[check-crate]` table.
    #[serde(default, rename = "check-crate")]
    pub check_crate: CheckCrateConfig,
}

/// Standing `check-crate` preferences.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CheckCrateConfig {
    /// Crates to ignore in every run — direct or transitive — as if
    /// they were not dependencies at all. Fedora almost always drops
    /// some upstream dependencies (benchmark harnesses like
    /// `criterion`, say), so listing them here keeps every report
    /// honest about what will actually be packaged. Merged with
    /// `--exclude`. Unset, [`DEFAULT_EXCLUDES`] applies; a list set
    /// here replaces it (`exclude = []` excludes nothing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
}

/// The benchmark harnesses `check-crate` ignores unless the config
/// file sets its own `exclude` list: Fedora drops them from every
/// build, so they are never something to package.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "codspeed",
    "codspeed-bencher-compat",
    "codspeed-criterion-compat",
    "codspeed-divan-compat",
    "count_instructions",
    "criterion",
    "criterion2",
    "divan",
    "iai",
    "iai-callgrind",
];

/// The effective exclude list and whether the config file set it
/// (`false`: [`DEFAULT_EXCLUDES`] is in force).
pub fn resolve_excludes(configured: Option<Vec<String>>) -> (Vec<String>, bool) {
    match configured {
        Some(list) => (list, true),
        None => (
            DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect(),
            false,
        ),
    }
}

/// The crates `check-crate` ignores: the config file's list (system
/// file beneath user file, as usual), or [`DEFAULT_EXCLUDES`] when no
/// file sets one. The flag says which.
pub fn check_crate_excludes() -> (Vec<String>, bool) {
    resolve_excludes(
        sandogasa_config::ConfigFile::for_tool("ebranch")
            .load::<EbranchConfig>()
            .ok()
            .and_then(|c| c.check_crate.exclude),
    )
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
    let mut config: EbranchConfig = cf.load().unwrap_or_default();

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_crate_excludes_parse_from_their_table_and_default_empty() {
        let cfg: EbranchConfig = toml::from_str(
            r#"
            [bugzilla]
            url = "https://bugzilla.example"
            [check-crate]
            exclude = ["criterion", "pretty_assertions"]
            "#,
        )
        .unwrap();
        let (list, set) = resolve_excludes(cfg.check_crate.exclude);
        assert_eq!(list, ["criterion", "pretty_assertions"]);
        assert!(set);
        // No list: the built-in benchmark set applies.
        let bare: EbranchConfig = toml::from_str("").unwrap();
        let (list, set) = resolve_excludes(bare.check_crate.exclude);
        assert_eq!(list, DEFAULT_EXCLUDES);
        assert!(!set);
        // An explicit empty list replaces it: someone packaging criterion.
        let none: EbranchConfig = toml::from_str("[check-crate]\nexclude = []").unwrap();
        let (list, set) = resolve_excludes(none.check_crate.exclude);
        assert!(list.is_empty());
        assert!(set);
    }
}
