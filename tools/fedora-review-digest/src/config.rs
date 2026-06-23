// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla credentials for `--post`, from
//! `~/.config/fedora-review-digest/config.toml` (`[bugzilla] api_key`),
//! overridable by the `BUGZILLA_API_KEY` env var. The key is prompted
//! for and saved on first use if neither is set.

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

/// The Bugzilla API key: `BUGZILLA_API_KEY` if set, else the config
/// file's `[bugzilla] api_key`, prompting for and saving it the first
/// time it's missing.
pub fn api_key() -> Result<String, Box<dyn std::error::Error>> {
    if let Ok(k) = std::env::var("BUGZILLA_API_KEY")
        && !k.trim().is_empty()
    {
        return Ok(k);
    }
    let file = ConfigFile::for_tool("fedora-review-digest");
    let mut cfg: Config = file.load().unwrap_or_default();
    if cfg.bugzilla.api_key.trim().is_empty() {
        eprintln!(
            "No Bugzilla API key set (config: {}, or $BUGZILLA_API_KEY).",
            file.path().display()
        );
        cfg.bugzilla.api_key = sandogasa_config::prompt_field("Bugzilla", "API key", true, None)?;
        file.save(&cfg)?;
    }
    Ok(cfg.bugzilla.api_key)
}
