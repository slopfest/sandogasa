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

/// The secrets `config` holds, for [`sandogasa_config::secret::fill`]
/// and [`save`].
pub fn slots(config: &mut Config) -> Vec<sandogasa_config::secret::Slot<'_>> {
    vec![sandogasa_config::secret::Slot {
        table: "forgejo",
        key: "token",
        what: "Forgejo token",
        entry: crate::secrets::forgejo(),
        value: &mut config.forgejo.token,
        required: true,
    }]
}

/// Save `config` to `cf`, the token in the keyring when there is one
/// and out of the file then; `before` is what
/// [`sandogasa_config::secret::fill`] said. Whether the file, left with
/// nothing in it, was removed.
pub fn save(
    cf: &sandogasa_config::ConfigFile,
    mut config: Config,
    before: &[sandogasa_config::secret::Before],
) -> Result<bool, Box<dyn std::error::Error>> {
    let kept = sandogasa_config::secret::save_with(
        cf,
        crate::secrets::keyring(),
        "fesco-chair",
        &mut config,
        before,
        slots,
    )?;
    Ok(!kept)
}

/// The config `cmd_config` starts from, with what
/// [`sandogasa_config::secret::fill`] said: the token kept in the
/// keyring is the saved one (#79). The token is required, so one turned
/// off is offered on every run: blank, so the marker is never checked
/// against the forge, unless `ask` says to use the keyring's entry again.
/// `review` is the `--all` run.
fn for_prompts(
    cf: &sandogasa_config::ConfigFile,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
) -> (Config, Vec<sandogasa_config::secret::Before>) {
    let mut config: Config = cf.load_user().unwrap_or_default();
    let before = sandogasa_config::secret::fill(
        crate::secrets::keyring(),
        "fesco-chair",
        slots(&mut config),
        review,
        ask,
    );
    (config, before)
}

/// Interactive config setup: prompt for the Forgejo token (again,
/// with `all`), validate it against the instance, and save.
pub fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("fesco-chair");
    let (mut config, before) = for_prompts(&cf, args.all, &sandogasa_cli::ask_to_reenable);

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
        std::mem::take(&mut config.forgejo.token),
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

    let removed = save(&cf, config, &before).map_err(|e| format!("failed to save config: {e}"))?;
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

    /// The prompt offers the keyring's token over a stale one in the
    /// file, and saving it again leaves the file without it.
    #[test]
    #[serial_test::serial]
    fn the_prompt_offers_the_keyrings_token() {
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        std::fs::write(cf.path(), "[forgejo]\ntoken = \"stale\"\n").unwrap();
        let keyring = crate::secrets::keyring();
        keyring.store(&crate::secrets::forgejo(), "fresh").unwrap();
        let (config, before) = for_prompts(&cf, false, &sandogasa_config::secret::never);
        assert_eq!(
            before,
            [sandogasa_config::secret::Before::Set {
                keyring: Some("fresh".into())
            }]
        );
        assert_eq!(config.forgejo.token, "fresh");
        assert!(save(&cf, config, &before).unwrap());
        assert!(!cf.path().exists());
        keyring.clear(&crate::secrets::forgejo()).unwrap();
    }

    /// The token is required, so one turned off is offered on a plain
    /// run too: blank, so "-" is never the saved value the forge is asked
    /// to accept; with the keyring's entry there, it is asked about,
    /// defaulting to yes, and a yes offers that again and saving takes
    /// the marker out.
    #[test]
    #[serial_test::serial]
    fn a_token_turned_off_is_offered_blank() {
        use sandogasa_config::secret::{Before, never};
        let dir = tempfile::tempdir().unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(dir.path().join("config.toml"));
        std::fs::write(cf.path(), "[forgejo]\ntoken = \"-\"\n").unwrap();
        let (config, before) = for_prompts(&cf, false, &|q: &str, _| panic!("asked {q}"));
        assert_eq!(
            (config.forgejo.token.as_str(), &before[..]),
            ("", &[Before::Off][..])
        );

        let keyring = crate::secrets::keyring();
        keyring.store(&crate::secrets::forgejo(), "kept").unwrap();
        let (config, _) = for_prompts(&cf, false, &never);
        assert_eq!(config.forgejo.token, "");
        let asked = std::cell::RefCell::new(Vec::new());
        let (config, before) = for_prompts(&cf, false, &|q: &str, default_yes| {
            asked.borrow_mut().push((q.to_string(), default_yes));
            true
        });
        assert_eq!(
            asked.into_inner(),
            [(
                "Forgejo token: off for fesco-chair (the keyring has one). Use it again?"
                    .to_string(),
                true
            )]
        );
        assert_eq!(config.forgejo.token, "kept");
        assert!(save(&cf, config, &before).unwrap());
        assert!(!cf.path().exists(), "the marker left the file");
        keyring.clear(&crate::secrets::forgejo()).unwrap();
    }

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
