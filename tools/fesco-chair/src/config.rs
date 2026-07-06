// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration for fesco-chair — the Forgejo API token, stored at
//! `~/.config/fesco-chair/config.toml` with restricted permissions
//! (dir 700, file 600, via sandogasa-config).

use serde::{Deserialize, Serialize};

use crate::sources;

/// Top-level config structure.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub forgejo: ForgejoConfig,
}

/// Forgejo configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ForgejoConfig {
    /// API token for forge.fedoraproject.org.
    #[serde(default)]
    pub token: String,
}

/// The stored Forgejo token, if any.
pub fn stored_token() -> Option<String> {
    let config: Config = sandogasa_config::ConfigFile::for_tool("fesco-chair")
        .load()
        .ok()?;
    (!config.forgejo.token.is_empty()).then_some(config.forgejo.token)
}

/// Interactive config setup: prompt for the Forgejo token, validate
/// it against the instance, and save.
pub fn cmd_config() -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("fesco-chair");
    let mut config: Config = cf.load().unwrap_or_default();

    println!("fesco-chair configuration\n");
    println!("Config file: {}\n", cf.path().display());

    if config.forgejo.token.is_empty() {
        println!(
            "Generate a Forgejo token at:\n  \
             {}/user/settings/applications\n\
             \n\
             Scopes: read:issue + read:repository (agenda queries) and\n\
             read:user (this validation step). Pick read-and-write on\n\
             issue instead to be ready for the planned ticket updates.\n\
             See the fesco-chair README for details.\n",
            sources::FORGE_URL
        );
        config.forgejo.token = sandogasa_config::prompt_field("Forgejo", "API token", true, None)
            .map_err(|e| format!("failed to read token: {e}"))?;
    } else {
        println!("Forgejo token: (set — delete the config file to replace it)");
    }

    // Validate against the instance; a network hiccup shouldn't lose
    // the token the user just pasted, so save regardless and warn.
    print!("Validating token against {}... ", sources::FORGE_URL);
    match sandogasa_forgejo::validate_token(sources::FORGE_URL, &config.forgejo.token) {
        Ok(true) => println!("valid."),
        Ok(false) => {
            println!("rejected.");
            eprintln!(
                "warning: {} rejected the token; it was saved but won't work",
                sources::FORGE_URL
            );
        }
        Err(e) => {
            println!("could not verify.");
            eprintln!("warning: {e}; the token was saved unverified");
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
    fn config_round_trips_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        // Missing file loads as an error; defaults are all-empty.
        assert!(cf.load::<Config>().is_err());
        let config = Config {
            forgejo: ForgejoConfig {
                token: "tok-123".to_string(),
            },
        };
        cf.save(&config).unwrap();
        let loaded: Config = cf.load().unwrap();
        assert_eq!(loaded.forgejo.token, "tok-123");
        // A file without the forgejo section still loads (serde defaults).
        std::fs::write(cf.path(), "").unwrap();
        let empty: Config = cf.load().unwrap();
        assert!(empty.forgejo.token.is_empty());
    }
}
