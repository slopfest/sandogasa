// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration for fesco-chair — the Forgejo API token, kept in the
//! desktop keyring when there is one (#79), else in
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
    /// API token for forge.fedoraproject.org; empty when it is in the
    /// keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub token: String,
}

/// The Forgejo token in the config file, if any. The keyring's comes
/// first; see [`crate::sources::forge_token`].
pub fn stored_token() -> Option<String> {
    let config: Config = sandogasa_config::ConfigFile::for_tool("fesco-chair")
        .load()
        .ok()?;
    (!config.forgejo.token.is_empty()).then_some(config.forgejo.token)
}

/// Save `config` to `cf`, the token in the keyring when there is one:
/// `save` lays the struct over the file and never removes a key, so a
/// token moved to the keyring is taken out of the file here. Whether
/// the file, left with nothing in it, was removed.
pub fn save(
    cf: &sandogasa_config::ConfigFile,
    mut config: Config,
) -> Result<bool, Box<dyn std::error::Error>> {
    if !config.forgejo.token.is_empty() {
        config.forgejo.token = crate::secrets::keep_for_file(
            "Forgejo token",
            &crate::secrets::forgejo(),
            std::mem::take(&mut config.forgejo.token),
        );
    }
    let in_keyring = config.forgejo.token.is_empty();
    cf.save(&config)?;
    if in_keyring {
        return cf.remove("forgejo", "token");
    }
    Ok(false)
}

/// Interactive config setup: prompt for the Forgejo token (again,
/// with `all`), validate it against the instance, and save.
pub fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("fesco-chair");
    let mut config: Config = cf.load_user().unwrap_or_default();
    // A token kept in the keyring is the saved one (#79).
    if let Some(token) = sandogasa_config::secret::resolve(
        crate::secrets::keyring(),
        None,
        &crate::secrets::forgejo(),
        Some(&config.forgejo.token),
    ) {
        config.forgejo.token = token;
    }

    println!("fesco-chair configuration\n");
    println!("Config file: {}\n", cf.path().display());

    let hint = format!(
        "Generate a Forgejo token at:\n  \
         {}/user/settings/applications\n\
         \n\
         Scopes: read:issue + read:repository (agenda queries) and\n\
         read:user (this validation step). Pick read-and-write on\n\
         issue instead to be ready for the planned ticket updates.\n\
         See the fesco-chair README for details.\n",
        sources::FORGE_URL
    );
    config.forgejo.token = sandogasa_config::ask(
        &sandogasa_config::Setting::new("Forgejo", "API token")
            .sensitive()
            .hint(&hint),
        &config.forgejo.token,
        args.all,
    )
    .map_err(|e| format!("failed to read token: {e}"))?;

    // Validated against the instance; a network hiccup shouldn't lose
    // the token just pasted, so a check that could not be made keeps
    // it and says so. A rejection offers a replacement instead of
    // leaving the only remedy as editing the file.
    config.forgejo.token = sandogasa_cli::credential::validated_key_blocking(
        "Forgejo",
        "token",
        config.forgejo.token.clone(),
        |token| {
            use sandogasa_cli::credential::KeyVerdict;
            match sandogasa_forgejo::validate_token(sources::FORGE_URL, token) {
                Ok(true) => KeyVerdict::Accepted("valid.".to_string()),
                Ok(false) => KeyVerdict::Refused(format!("{} rejected it.", sources::FORGE_URL)),
                Err(e) => KeyVerdict::Unknown(format!("could not verify: {e}")),
            }
        },
    )
    .key;

    let removed = save(&cf, config).map_err(|e| format!("failed to save config: {e}"))?;
    if removed {
        println!(
            "\nNothing else to keep in {}; removed it.",
            cf.path().display()
        );
    } else {
        println!("\nConfig saved to {}", cf.path().display());
    }
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
