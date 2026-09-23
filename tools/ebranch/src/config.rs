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
    /// `[nvd]` table: an API key lifts NVD's pace from one request
    /// per 6 s to one per 0.6 s when CVE trackers are judged.
    #[serde(default)]
    pub nvd: NvdConfig,
    /// `[check-crate]` table.
    #[serde(default, rename = "check-crate")]
    pub check_crate: CheckCrateConfig,
}

/// NVD configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct NvdConfig {
    #[serde(default)]
    pub api_key: String,
}

/// The configured NVD API key, empty when there is none — the layered
/// config, so a key shipped system-wide counts.
pub fn nvd_api_key() -> String {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.nvd.api_key)
        .unwrap_or_default()
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
    /// here replaces it (`exclude = []` excludes nothing), and the
    /// entry [`DEFAULT_SENTINEL`] inside it stands for that set, so
    /// crates can be added without retyping it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Per-crate `--in-tree` lists, keyed by crate name: the globs
    /// (and `@repository`) naming the crates a package builds from
    /// its own tree, so `check-crate coreutils` knows about `uu_*`
    /// without being told each run. Merged with `--in-tree`.
    #[serde(
        default,
        rename = "in-tree",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub in_tree: std::collections::BTreeMap<String, Vec<String>>,
    /// Per-crate staging COPR (`owner/project`), keyed by crate name:
    /// `--copr` without typing it, for a crate whose update is
    /// staged there.
    #[serde(
        default,
        rename = "copr",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub copr: std::collections::BTreeMap<String, String>,
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

/// The list entry that expands to [`DEFAULT_EXCLUDES`]: TOML has no
/// `+=`, so `exclude = ["@default", "pretty_assertions"]` is how a
/// config adds to the built-in set instead of replacing it. `@` cannot
/// start a crate name, so it cannot collide.
pub const DEFAULT_SENTINEL: &str = "@default";

/// The effective exclude list and whether the config file set it
/// (`false`: [`DEFAULT_EXCLUDES`] is in force on its own).
pub fn resolve_excludes(configured: Option<Vec<String>>) -> (Vec<String>, bool) {
    let defaults = || DEFAULT_EXCLUDES.iter().map(|s| s.to_string());
    match configured {
        Some(list) => (
            list.into_iter()
                .flat_map(|e| {
                    if e == DEFAULT_SENTINEL {
                        defaults().collect::<Vec<_>>()
                    } else {
                        vec![e]
                    }
                })
                .collect(),
            true,
        ),
        None => (defaults().collect(), false),
    }
}

/// The `[check-crate]` table as configured (system file beneath user
/// file, as usual); default when there is no file.
fn load_check_crate() -> CheckCrateConfig {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.check_crate)
        .unwrap_or_default()
}

/// The crates `check-crate` ignores: the config file's list, or
/// [`DEFAULT_EXCLUDES`] when no file sets one. The flag says which.
pub fn check_crate_excludes() -> (Vec<String>, bool) {
    resolve_excludes(load_check_crate().exclude)
}

/// The configured staging COPR for a crate, if any.
pub fn check_crate_copr(crate_name: &str) -> Option<String> {
    load_check_crate().copr.remove(crate_name)
}

/// The configured `--in-tree` list for a crate; empty when none.
pub fn check_crate_in_tree(crate_name: &str) -> Vec<String> {
    load_check_crate()
        .in_tree
        .remove(crate_name)
        .unwrap_or_default()
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
    let mut config: EbranchConfig = cf.load_user().unwrap_or_default();

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

    // NVD API key: optional, for judging CVE trackers against the
    // fix NVD records.
    if config.nvd.api_key.is_empty() {
        println!(
            "\nAn NVD API key raises the rate limit from 5 to 50 requests per 30 s when \
             check-update judges CVE bugs. Request one free at \
             https://nvd.nist.gov/developers/request-an-api-key, or leave empty to skip."
        );
        if let Some(key) = sandogasa_config::prompt_optional_field(
            "NVD",
            "API key (optional, empty to skip)",
            true,
        )
        .map_err(|e| format!("failed to read NVD API key: {e}"))?
        {
            config.nvd.api_key = key;
        }
    } else {
        println!("NVD API key: (set)");
    }
    if !config.nvd.api_key.is_empty() {
        print!("Validating NVD API key... ");
        match sandogasa_cve::cache::check_api_key(&config.nvd.api_key).await {
            sandogasa_cve::cache::KeyCheck::Accepted => {
                println!("valid; NVD lookups pace at 0.6 s instead of 6 s.")
            }
            sandogasa_cve::cache::KeyCheck::Refused => println!(
                "not accepted (404): NVD does not know this key — it is mistyped, or not yet \
                 activated; the activation link arrives by mail after the request. Runs fall \
                 back to the keyless pace until it works."
            ),
            sandogasa_cve::cache::KeyCheck::Throttled(status) => {
                println!("NVD refused the request ({status}) — rate limited; try again in a minute")
            }
            sandogasa_cve::cache::KeyCheck::Unreachable(e) => println!("could not check: {e}"),
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
        // Per-crate in-tree lists and staging COPRs live in their own tables.
        let cfg: EbranchConfig = toml::from_str(
            r#"
            [check-crate.in-tree]
            coreutils = ["uu_*", "uucore*", "uutests"]
            [check-crate.copr]
            coreutils = "@rust/uutils-and-nushell"
            phf = "@rust/uutils-and-nushell"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.check_crate.copr["phf"], "@rust/uutils-and-nushell");
        assert!(!cfg.check_crate.copr.contains_key("serde"));
        assert_eq!(
            cfg.check_crate.in_tree["coreutils"],
            ["uu_*", "uucore*", "uutests"]
        );
        assert!(!cfg.check_crate.in_tree.contains_key("phf"));
        assert!(bare.check_crate.in_tree.is_empty());
        // "@default" inside the list stands for the built-in set.
        let (list, set) = resolve_excludes(Some(
            ["@default", "pretty_assertions"].map(String::from).to_vec(),
        ));
        let mut want: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        want.push("pretty_assertions".to_string());
        assert_eq!(list, want);
        assert!(set);
    }
}
