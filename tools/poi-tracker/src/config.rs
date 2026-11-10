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
    // The keyring's secret is the saved one (#79), as reading takes it
    // first: a stale value an older run left in the file must not be
    // offered, and so saved back over the shared entry. See `fill` for
    // what one turned off for poi-tracker is offered.
    let before = sandogasa_config::secret::fill(
        crate::secrets::keyring(),
        env!("CARGO_PKG_NAME"),
        slots(&mut config),
        args.all,
        &sandogasa_cli::ask_to_reenable,
    );

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
        std::mem::take(&mut config.bugzilla.api_key),
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

    let removed = save_keeping_secrets(&cf, config, &before)
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

/// The secrets `config` holds, for [`sandogasa_config::secret::fill`]
/// and [`save_keeping_secrets`].
fn slots(config: &mut PoiTrackerConfig) -> Vec<sandogasa_config::secret::Slot<'_>> {
    use sandogasa_config::secret::Slot;
    vec![
        Slot {
            table: "bugzilla",
            key: "api_key",
            what: "Bugzilla API key",
            entry: crate::secrets::bugzilla(&config.bugzilla.url),
            value: &mut config.bugzilla.api_key,
            required: true,
        },
        Slot {
            table: "dist-git",
            key: "api_token",
            what: "Pagure token",
            entry: crate::secrets::distgit(),
            value: &mut config.dist_git.api_token,
            required: false,
        },
    ]
}

/// Save `config` to `cf`, its secrets in the keyring when there is
/// one and then out of the file, since `save` never removes a key. A
/// secret cleared at the prompts is turned off for poi-tracker; one in
/// the keyring stays there, since other tools may read it. `before` is
/// what [`sandogasa_config::secret::fill`] said. Whether the file, left
/// with nothing in it, was removed.
fn save_keeping_secrets(
    cf: &sandogasa_config::ConfigFile,
    mut config: PoiTrackerConfig,
    before: &[sandogasa_config::secret::Before],
) -> Result<bool, Box<dyn std::error::Error>> {
    let kept = sandogasa_config::secret::save_with(
        cf,
        crate::secrets::keyring(),
        env!("CARGO_PKG_NAME"),
        &mut config,
        before,
        slots,
    )?;
    Ok(!kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sandogasa_config::test_support::EnvGuard;

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
        let removed = save_keeping_secrets(&cf, cf.load_user().unwrap(), &set_from_file()).unwrap();
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
        save_keeping_secrets(&cf, config, &set_from_file()).unwrap();
        let text = std::fs::read_to_string(cf.path()).unwrap();
        assert!(!text.contains("pagure-from-file"), "{text}");
        // Turned off for poi-tracker: the marker is in its file, and the
        // token is not read, from the keyring or anywhere else.
        assert!(text.contains("api_token = \"-\""), "{text}");
        assert!(resolve_distgit_token(None).is_err());
        assert_eq!(resolve_api_key(None).unwrap(), "bz-from-file");
        clear_keyring();
    }

    /// Both secrets as `fill` finds them in [`PLAIN_TEXT`].
    fn set_from_file() -> [sandogasa_config::secret::Before; 2] {
        let set = sandogasa_config::secret::Before::Set { keyring: None };
        [set.clone(), set]
    }

    /// A config with both secrets turned off for poi-tracker.
    fn both_off() -> PoiTrackerConfig {
        let off = sandogasa_config::secret::OFF;
        PoiTrackerConfig {
            bugzilla: BugzillaConfig {
                api_key: off.into(),
                url: "https://bugzilla.keyring.test".into(),
                ..Default::default()
            },
            dist_git: DistGitConfig {
                api_token: off.into(),
            },
        }
    }

    /// `fill` as `cmd_config` calls it, `review` being `--all`.
    fn fill(
        config: &mut PoiTrackerConfig,
        review: bool,
        ask: &dyn Fn(&str, bool) -> bool,
    ) -> Vec<sandogasa_config::secret::Before> {
        sandogasa_config::secret::fill(
            crate::secrets::keyring(),
            "poi-tracker",
            slots(config),
            review,
            ask,
        )
    }

    /// An `ask` that records each question and its default, answering
    /// `answer`.
    fn recording(
        asked: &std::cell::RefCell<Vec<(String, bool)>>,
        answer: bool,
    ) -> impl Fn(&str, bool) -> bool + '_ {
        move |q, default_yes| {
            asked.borrow_mut().push((q.to_string(), default_yes));
            answer
        }
    }

    fn store_both(bugzilla: &str, pagure: &str) {
        let keyring = crate::secrets::keyring();
        keyring
            .store(
                &crate::secrets::bugzilla("https://bugzilla.keyring.test"),
                bugzilla,
            )
            .unwrap();
        keyring.store(&crate::secrets::distgit(), pagure).unwrap();
    }

    /// The prompts offer the keyring's secret for the configured
    /// Bugzilla over a stale one in the file.
    #[test]
    #[serial_test::serial]
    fn the_prompts_offer_the_keyrings_secret() {
        use sandogasa_config::secret::{Before, never};
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let mut config = both_off();
        config.bugzilla.api_key = "bz-stale".into();
        config.dist_git.api_token.clear();
        crate::secrets::keyring()
            .store(
                &crate::secrets::bugzilla("https://bugzilla.keyring.test"),
                "bz-fresh",
            )
            .unwrap();
        let before = fill(&mut config, false, &never);
        assert_eq!(
            before,
            [
                Before::Set {
                    keyring: Some("bz-fresh".into())
                },
                Before::Absent
            ]
        );
        assert_eq!(config.bugzilla.api_key, "bz-fresh");
        clear_keyring();
    }

    /// A plain run: the dist-git token, optional and turned off, stays
    /// "-" and is not asked about, so its prompt asks nothing either;
    /// the Bugzilla key, required, is asked about, defaulting to yes,
    /// and otherwise offered blank, so "-" is never the key Bugzilla is
    /// asked to accept.
    #[test]
    #[serial_test::serial]
    fn a_plain_run_asks_only_about_the_required_key() {
        use sandogasa_config::secret::{Before, OFF};
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let none = std::cell::RefCell::new(Vec::new());
        let mut config = both_off();
        assert_eq!(
            fill(&mut config, false, &recording(&none, true)),
            [Before::Off, Before::Off]
        );
        assert!(none.borrow().is_empty(), "no entry, nothing to ask");
        assert_eq!(config.bugzilla.api_key, "");
        assert_eq!(config.dist_git.api_token, OFF);

        store_both("bz-kept", "pagure-kept");
        let asked = std::cell::RefCell::new(Vec::new());
        let mut config = both_off();
        fill(&mut config, false, &recording(&asked, false));
        assert_eq!(config.bugzilla.api_key, "");
        assert_eq!(config.dist_git.api_token, OFF);
        let mut config = both_off();
        fill(&mut config, false, &recording(&asked, true));
        assert_eq!(config.bugzilla.api_key, "bz-kept");
        assert_eq!(config.dist_git.api_token, OFF);
        let q = "Bugzilla API key: off for poi-tracker (the keyring has one). Use it again?";
        assert_eq!(
            asked.into_inner(),
            [(q.to_string(), true), (q.to_string(), true)]
        );
        clear_keyring();
    }

    /// `--all` goes over the optional token too: asked about, defaulting
    /// to no, and offered blank unless the answer is yes.
    #[test]
    #[serial_test::serial]
    fn a_review_offers_the_optional_token_too() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        store_both("bz-kept", "pagure-kept");
        let asked = std::cell::RefCell::new(Vec::new());
        let mut config = both_off();
        fill(&mut config, true, &recording(&asked, false));
        assert_eq!(config.dist_git.api_token, "");
        let mut config = both_off();
        fill(&mut config, true, &|q, default_yes| {
            asked.borrow_mut().push((q.to_string(), default_yes));
            q.starts_with("Pagure")
        });
        assert_eq!(config.bugzilla.api_key, "");
        assert_eq!(config.dist_git.api_token, "pagure-kept");
        let bz = "Bugzilla API key: off for poi-tracker (the keyring has one). Use it again?";
        let pagure = "Pagure token: off for poi-tracker (the keyring has one). Use it again?";
        assert_eq!(
            asked.into_inner(),
            [
                (bz.to_string(), true),
                (pagure.to_string(), false),
                (bz.to_string(), true),
                (pagure.to_string(), false),
            ]
        );
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
