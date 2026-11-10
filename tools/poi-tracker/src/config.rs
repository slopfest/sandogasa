// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for poi-tracker.
//!
//! Keeps the Bugzilla API key and the dist-git token in the
//! desktop keyring when there is one (#79), and the rest at
//! `~/.config/poi-tracker/config.toml` with restricted
//! permissions (handled by `sandogasa-config`); the secrets go in
//! the file only where no keyring can be used. Mirrors the
//! `ebranch` shape so a future refactor can fold both into a
//! shared crate without changing the on-disk format.

use serde::{Deserialize, Serialize};

/// Top-level config structure.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PoiTrackerConfig {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
    #[serde(default, rename = "dist-git")]
    pub dist_git: DistGitConfig,
}

/// Dist-git (Pagure) configuration — the `[dist-git]` table,
/// matching sandogasa-pkg-acl's shape.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DistGitConfig {
    /// API token with the `modify_project` ACL, for `adopt`;
    /// empty when it is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_token: String,
}

/// Bugzilla configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    /// Empty when the key is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
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

fn config_file() -> sandogasa_config::ConfigFile {
    sandogasa_config::ConfigFile::for_tool("poi-tracker")
}

/// The layered config (`/etc`, then the user file).
fn load() -> Result<PoiTrackerConfig, Box<dyn std::error::Error>> {
    config_file().load()
}

/// Load the Bugzilla API key, checking in order:
/// 1. `--api-key` CLI flag
/// 2. `BUGZILLA_API_KEY` environment variable
/// 3. the keyring's entry for the configured Bugzilla
/// 4. `~/.config/poi-tracker/config.toml`
///
/// Returns an error with setup instructions when nothing is set.
pub fn resolve_api_key(cli_key: Option<&str>) -> Result<String, String> {
    if let Some(key) = cli_key
        && !key.is_empty()
    {
        return Ok(key.to_string());
    }
    let file = load().map(|c| c.bugzilla.api_key).unwrap_or_default();
    sandogasa_config::secret::resolve(
        crate::secrets::keyring(),
        Some("BUGZILLA_API_KEY"),
        &crate::secrets::bugzilla(&resolve_url()),
        Some(&file),
    )
    .ok_or_else(|| {
        "Bugzilla API key not found.\n\
         Set it up with: poi-tracker config\n\
         Or pass --api-key or set BUGZILLA_API_KEY."
            .to_string()
    })
}

/// Load the dist-git (Pagure) API token, checking in order:
/// 1. `--api-token` CLI flag
/// 2. `PAGURE_API_TOKEN` environment variable (pkg-acl's
///    convention)
/// 3. the keyring's src.fedoraproject.org entry, which
///    sandogasa-pkg-acl shares
/// 4. `~/.config/poi-tracker/config.toml` `[dist-git] api_token`
///
/// Returns an error with setup instructions when nothing is set.
pub fn resolve_distgit_token(cli_token: Option<&str>) -> Result<String, String> {
    if let Some(token) = cli_token
        && !token.is_empty()
    {
        return Ok(token.to_string());
    }
    let file = load().map(|c| c.dist_git.api_token).unwrap_or_default();
    sandogasa_config::secret::resolve(
        crate::secrets::keyring(),
        Some("PAGURE_API_TOKEN"),
        &crate::secrets::distgit(),
        Some(&file),
    )
    .ok_or_else(|| {
        "dist-git API token not found.\n\
         Generate one at https://src.fedoraproject.org/settings/token/new\n\
         with the \"Modify an existing project\" ACL, then set it up\n\
         with: poi-tracker config\n\
         Or pass --api-token or set PAGURE_API_TOKEN."
            .to_string()
    })
}

/// Load the Bugzilla base URL: config file first, then the
/// hardcoded default. (No CLI override; the URL is per-instance
/// and rarely changes day-to-day.)
pub fn resolve_url() -> String {
    load()
        .ok()
        .filter(|c| !c.bugzilla.url.is_empty())
        .map(|c| c.bugzilla.url)
        .unwrap_or_else(|| BugzillaConfig::DEFAULT_URL.to_string())
}

/// Read the user's configured Bugzilla email (used to claim
/// ownership when closing bugs). Returns `None` if the file
/// isn't present, can't be parsed, or has no email set.
pub fn resolve_email() -> Option<String> {
    load()
        .ok()
        .map(|c| c.bugzilla.email)
        .filter(|e| !e.is_empty())
}

/// Interactive config setup. Prompts for what is missing (with
/// `all`, for every setting), validates the Bugzilla API key, and
/// writes the result.
pub async fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    use sandogasa_config::{Setting, ask};
    let cf = config_file();
    let mut config: PoiTrackerConfig = cf.load_user().unwrap_or_default();

    println!("poi-tracker configuration\n");
    println!("Config file: {}\n", cf.path().display());

    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = BugzillaConfig::DEFAULT_URL.to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);
    // A secret kept in the keyring is the saved one (#79), as reading
    // takes it first: a stale value an older run left in the file must
    // not be offered, and so saved back over the shared entry. One turned
    // off for poi-tracker stays off.
    let off = sandogasa_config::secret::OFF;
    if config.bugzilla.api_key != off
        && let Some(key) = crate::secrets::lookup(&crate::secrets::bugzilla(&config.bugzilla.url))
    {
        config.bugzilla.api_key = key;
    }
    if config.dist_git.api_token != off
        && let Some(token) = crate::secrets::lookup(&crate::secrets::distgit())
    {
        config.dist_git.api_token = token;
    }
    let had = [
        !config.bugzilla.api_key.is_empty(),
        !config.dist_git.api_token.is_empty(),
    ];

    config.bugzilla.api_key = ask(
        &Setting::new("Bugzilla", "API key").sensitive().hint(
            "\nGenerate an API key at:\n  \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey\n",
        ),
        &config.bugzilla.api_key,
        args.all,
    )
    .map_err(|e| format!("failed to read API key: {e}"))?;

    // Email is optional — it's only used by `triage-retired
    // --claim` to set `assigned_to` on closed bugs.
    config.bugzilla.email = ask(
        &Setting::new("Bugzilla", "email (optional, for --claim)")
            .optional()
            .validated(&sandogasa_config::validate_email),
        &config.bugzilla.email,
        args.all,
    )
    .map_err(|e| format!("failed to read e-mail: {e}"))?;

    // The dist-git token is optional — only `adopt` needs it.
    config.dist_git.api_token = ask(
        &Setting::new("dist-git", "API token (optional, empty to skip)")
            .sensitive()
            .optional()
            .hint(
                "\nOptional: a dist-git API token lets `adopt` take orphaned\n\
                 packages. Generate one at\n  \
                 https://src.fedoraproject.org/settings/token/new\n\
                 with the \"Modify an existing project\" ACL.",
            ),
        &config.dist_git.api_token,
        args.all,
    )
    .map_err(|e| format!("failed to read token: {e}"))?;

    // Checked by trying to use it, not by searching: Bugzilla serves
    // bug searches to anyone, so the old check passed for a key that
    // could never write. `api_key_accepted` asks to create a bug with
    // no fields, which the credential is checked before, and which
    // cannot create anything because every required field is absent.
    let url = config.bugzilla.url.clone();
    config.bugzilla.api_key = sandogasa_cli::credential::validated_key(
        "Bugzilla",
        "API key",
        config.bugzilla.api_key.clone(),
        move |key| {
            let url = url.clone();
            Box::pin(async move {
                use sandogasa_cli::credential::KeyVerdict;
                let bz = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(key) {
                    Ok(bz) => bz,
                    Err(e) => return KeyVerdict::Unknown(format!("could not check: {e}")),
                };
                match bz.check_api_key().await {
                    Ok(sandogasa_bugzilla::KeyCheck::Accepted) => {
                        KeyVerdict::Accepted("accepted.".to_string())
                    }
                    Ok(sandogasa_bugzilla::KeyCheck::Refused(why)) => {
                        KeyVerdict::Refused(format!("refused: {why}"))
                    }
                    Ok(sandogasa_bugzilla::KeyCheck::Unclear(why)) => {
                        KeyVerdict::Unknown(format!("could not tell: {why}"))
                    }
                    Ok(_) => KeyVerdict::Unknown("could not tell.".to_string()),
                    Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
                }
            })
        },
    )
    .await
    .key;

    let removed = save_keeping_secrets(&cf, config, had)
        .map_err(|e| format!("failed to save config: {e}"))?;
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

/// Save `config` to `cf`, its secrets in the keyring when there is
/// one and then out of the file, since `save` never removes a key. A
/// secret left empty is taken out of the file; one in the keyring
/// stays there, since other tools may read it. Whether the file, left
/// with nothing in it, was removed.
fn save_keeping_secrets(
    cf: &sandogasa_config::ConfigFile,
    mut config: PoiTrackerConfig,
    had: [bool; 2],
) -> Result<bool, Box<dyn std::error::Error>> {
    let bugzilla = crate::secrets::bugzilla(&config.bugzilla.url);
    let distgit = crate::secrets::distgit();
    let mut out_of_file = Vec::new();
    for (table, key, what, secret, value, had) in [
        (
            "bugzilla",
            "api_key",
            "Bugzilla API key",
            &bugzilla,
            &mut config.bugzilla.api_key,
        ),
        (
            "dist-git",
            "api_token",
            "dist-git API token",
            &distgit,
            &mut config.dist_git.api_token,
        ),
    ]
    .into_iter()
    .zip(had)
    .map(|((t, k, w, s, v), had)| (t, k, w, s, v, had))
    {
        if value.is_empty() && had {
            // Cleared: turned off for poi-tracker, its file saying so;
            // the keyring entry is the service's, which other tools use.
            if let Some(note) = sandogasa_config::secret::kept_on_clear(
                crate::secrets::keyring(),
                env!("CARGO_PKG_NAME"),
                what,
                secret,
            ) {
                eprintln!("{note}");
            }
            *value = sandogasa_config::secret::OFF.to_string();
        } else if !value.is_empty() {
            *value = crate::secrets::keep_for_file(what, secret, std::mem::take(value));
        }
        if value.is_empty() {
            out_of_file.push((table, key));
        }
    }
    cf.save(&config)?;
    let mut removed = false;
    for (table, key) in out_of_file {
        removed |= cf.remove(table, key)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;

    const PLAIN_TEXT: &str = "[bugzilla]\n\
        api_key = \"bz-from-file\"\n\
        email = \"me@example.com\"\n\
        url = \"https://bugzilla.keyring.test\"\n\
        \n\
        [dist-git]\n\
        api_token = \"pagure-from-file\"\n";

    /// A config directory of its own, with nothing exported that
    /// would answer before the keyring.
    fn isolated(dir: &tempfile::TempDir) -> EnvGuard {
        EnvGuard::new(&[("XDG_CONFIG_HOME", &dir.path().to_string_lossy())])
            .unset(&["BUGZILLA_API_KEY", "PAGURE_API_TOKEN"])
    }

    fn write_config(text: &str) {
        let path = config_file().path().to_path_buf();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }

    fn clear_keyring() {
        let keyring = crate::secrets::keyring();
        keyring
            .clear(&crate::secrets::bugzilla("https://bugzilla.keyring.test"))
            .unwrap();
        keyring.clear(&crate::secrets::distgit()).unwrap();
    }

    /// The secrets `config` keeps in the keyring are what the tool then
    /// uses, the file is left without them, and the environment and
    /// the command line still win.
    #[test]
    #[serial_test::serial]
    fn secrets_kept_in_the_keyring_are_the_ones_used() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        write_config(PLAIN_TEXT);
        // An existing setup keeps working.
        assert_eq!(resolve_api_key(None).unwrap(), "bz-from-file");
        assert_eq!(resolve_distgit_token(None).unwrap(), "pagure-from-file");

        let cf = config_file();
        let removed = save_keeping_secrets(&cf, cf.load_user().unwrap(), [true, true]).unwrap();
        assert!(!removed, "the email and URL are still in the file");
        let text = std::fs::read_to_string(cf.path()).unwrap();
        assert!(!text.contains("from-file"), "the file is left without them");
        assert!(!text.contains("api_key") && !text.contains("api_token"));
        assert!(text.contains("me@example.com"));

        // From the keyring now: the file no longer has them.
        assert_eq!(resolve_api_key(None).unwrap(), "bz-from-file");
        assert_eq!(resolve_distgit_token(None).unwrap(), "pagure-from-file");
        // The environment still wins, and the command line over it.
        let _env2 = EnvGuard::new(&[
            ("BUGZILLA_API_KEY", "bz-from-env"),
            ("PAGURE_API_TOKEN", "pagure-from-env"),
        ]);
        assert_eq!(resolve_api_key(None).unwrap(), "bz-from-env");
        assert_eq!(resolve_distgit_token(None).unwrap(), "pagure-from-env");
        assert_eq!(resolve_api_key(Some("bz-cli")).unwrap(), "bz-cli");
        assert_eq!(
            resolve_distgit_token(Some("pagure-cli")).unwrap(),
            "pagure-cli"
        );
        clear_keyring();
    }

    /// A token cleared at the prompt is taken out of the file, which
    /// `save` alone would leave it in.
    #[test]
    #[serial_test::serial]
    fn a_cleared_token_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        write_config(PLAIN_TEXT);
        let cf = config_file();
        let mut config: PoiTrackerConfig = cf.load_user().unwrap();
        config.dist_git.api_token.clear();
        save_keeping_secrets(&cf, config, [true, true]).unwrap();
        let text = std::fs::read_to_string(cf.path()).unwrap();
        assert!(!text.contains("pagure-from-file"), "{text}");
        // Turned off for poi-tracker: the marker is in its file, and the
        // token is not read, from the keyring or anywhere else.
        assert!(text.contains("api_token = \"-\""), "{text}");
        assert!(resolve_distgit_token(None).is_err());
        assert_eq!(resolve_api_key(None).unwrap(), "bz-from-file");
        clear_keyring();
    }

    #[test]
    #[serial_test::serial]
    fn nothing_set_says_how_to_set_it_up() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        assert!(
            resolve_api_key(None)
                .unwrap_err()
                .contains("poi-tracker config")
        );
        assert!(
            resolve_distgit_token(None)
                .unwrap_err()
                .contains("PAGURE_API_TOKEN")
        );
        assert_eq!(resolve_url(), BugzillaConfig::DEFAULT_URL);
        assert_eq!(resolve_email(), None);
    }
}
