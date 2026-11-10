// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla credentials for `--post`: the API key from the desktop
//! keyring when there is one (#79), else from
//! `~/.config/fedora-review-digest/config.toml` (`[bugzilla] api_key`),
//! and the email from that file (`[bugzilla] email`), overridable by the
//! `BUGZILLA_API_KEY` / `BUGZILLA_EMAIL` env vars. Each is prompted for
//! and saved on first use if unset. The email is the Bugzilla login the
//! bug is assigned to when claiming it.

use sandogasa_config::ConfigFile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    /// Empty when the key is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    #[serde(default)]
    pub email: String,
}

/// The reviewer's Bugzilla credentials for `--post`.
pub struct Credentials {
    /// API key for authenticating the write.
    pub api_key: String,
    /// Bugzilla login (email) — the assignee when claiming the bug.
    pub email: String,
}

/// The reviewer's credentials: the API key from `$BUGZILLA_API_KEY` and
/// the email from `$BUGZILLA_EMAIL`, the key falling back to the keyring,
/// each then to the config file and then an interactive prompt that saves
/// it. Loads (and saves) the config file once for both.
pub fn credentials() -> Result<Credentials, Box<dyn std::error::Error>> {
    let file = ConfigFile::for_tool("fedora-review-digest");
    let mut cfg: Config = file.load_user().unwrap_or_default();
    let mut dirty = false;
    let mut prompted_key = false;

    let api_key = match env_nonempty("BUGZILLA_API_KEY") {
        Some(k) => k,
        None => {
            let in_file = cfg.bugzilla.api_key.clone();
            let in_file = (!in_file.trim().is_empty()).then_some(in_file);
            match sandogasa_config::secret::resolve(
                crate::secrets::keyring(),
                None,
                &crate::secrets::bugzilla(),
                in_file.as_deref(),
            ) {
                Some(k) => k,
                None => {
                    eprintln!(
                        "No Bugzilla API key set (keyring: `{}`, config: {}, or \
                         $BUGZILLA_API_KEY).",
                        crate::secrets::bugzilla().lookup_command(),
                        file.path().display()
                    );
                    cfg.bugzilla.api_key =
                        sandogasa_config::prompt_field("Bugzilla", "API key", true, None)?;
                    dirty = true;
                    prompted_key = true;
                    cfg.bugzilla.api_key.clone()
                }
            }
        }
    };

    let email = match env_nonempty("BUGZILLA_EMAIL") {
        Some(e) => e,
        None => {
            if cfg.bugzilla.email.trim().is_empty() {
                cfg.bugzilla.email = sandogasa_config::prompt_field(
                    "Bugzilla",
                    "email",
                    false,
                    Some(&sandogasa_config::validate_email),
                )?;
                dirty = true;
            }
            cfg.bugzilla.email.clone()
        }
    };

    if prompted_key {
        save(&file, cfg)?;
    } else if dirty {
        file.save(&cfg)?;
    }
    Ok(Credentials { api_key, email })
}

/// Save `cfg` to `file`, the API key in the keyring when there is one:
/// `save` lays the struct over the file and never removes a key, so a
/// key moved to the keyring is taken out of the file here. Whether the
/// file, left with nothing in it, was removed.
pub fn save(file: &ConfigFile, mut cfg: Config) -> Result<bool, Box<dyn std::error::Error>> {
    if !cfg.bugzilla.api_key.trim().is_empty() {
        cfg.bugzilla.api_key = crate::secrets::keep_for_file(
            "Bugzilla API key",
            &crate::secrets::bugzilla(),
            std::mem::take(&mut cfg.bugzilla.api_key),
        );
    }
    let in_keyring = cfg.bugzilla.api_key.is_empty();
    file.save(&cfg)?;
    if in_keyring {
        return file.remove("bugzilla", "api_key");
    }
    Ok(false)
}

/// A non-empty environment variable, trimmed away when blank/unset.
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}
