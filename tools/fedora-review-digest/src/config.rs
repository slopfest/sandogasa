// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla credentials for `--post`, from
//! `~/.config/fedora-review-digest/config.toml` (`[bugzilla] api_key`,
//! `email`), overridable by the `BUGZILLA_API_KEY` / `BUGZILLA_EMAIL`
//! env vars. Each is prompted for and saved on first use if unset. The
//! email is the Bugzilla login the bug is assigned to when claiming it.

use sandogasa_config::ConfigFile;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    #[serde(default)]
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
/// the email from `$BUGZILLA_EMAIL`, each falling back to the config file
/// and then an interactive prompt that saves it. Loads (and saves) the
/// config file once for both.
pub fn credentials() -> Result<Credentials, Box<dyn std::error::Error>> {
    let file = ConfigFile::for_tool("fedora-review-digest");
    let mut cfg: Config = file.load().unwrap_or_default();
    let mut dirty = false;

    let api_key = match env_nonempty("BUGZILLA_API_KEY") {
        Some(k) => k,
        None => {
            if cfg.bugzilla.api_key.trim().is_empty() {
                eprintln!(
                    "No Bugzilla API key set (config: {}, or $BUGZILLA_API_KEY).",
                    file.path().display()
                );
                cfg.bugzilla.api_key =
                    sandogasa_config::prompt_field("Bugzilla", "API key", true, None)?;
                dirty = true;
            }
            cfg.bugzilla.api_key.clone()
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

    if dirty {
        file.save(&cfg)?;
    }
    Ok(Credentials { api_key, email })
}

/// A non-empty environment variable, trimmed away when blank/unset.
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}
