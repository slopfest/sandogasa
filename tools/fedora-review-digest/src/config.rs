// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Bugzilla credentials for `--post`: the API key from the desktop
//! keyring when there is one (#79), else from
//! `~/.config/fedora-review-digest/config.toml` (`[bugzilla] api_key`),
//! and the email from that file (`[bugzilla] email`), overridable by the
//! `BUGZILLA_API_KEY` / `BUGZILLA_EMAIL` env vars. Each is prompted for
//! and saved on first use if unset. The email is the Bugzilla login the
//! bug is assigned to when claiming it.

use sandogasa_config::ConfigFile;
use sandogasa_config::identity;
use sandogasa_config::secret::{Before, SecretStore, Slot};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Config {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
    #[serde(default, skip_serializing_if = "DiffConfig::is_empty")]
    pub diff: DiffConfig,
}

/// `[diff]`: how to show a Python spec's changes from pyp2spec's output
/// at the terminal (#160).
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct DiffConfig {
    /// A diff command, such as `difft`, given the two specs as its last
    /// arguments; unset, the unified diff is shown as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

impl DiffConfig {
    fn is_empty(&self) -> bool {
        self.tool.is_none()
    }
}

/// The diff tool from `[diff] tool` in the config files, if set.
pub fn diff_tool() -> Option<String> {
    diff_tool_in(&ConfigFile::for_tool("fedora-review-digest"))
}

/// [`diff_tool`], from `file`.
fn diff_tool_in(file: &ConfigFile) -> Option<String> {
    let cfg: Config = file.load().unwrap_or_default();
    cfg.diff.tool.filter(|t| !t.trim().is_empty())
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    /// Empty when the key is in the keyring (#79).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_key: String,
    /// Empty when the shared file holds it (#259).
    #[serde(default, skip_serializing_if = "String::is_empty")]
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
/// it. Loads (and saves) the config file once for both. `store` is the
/// keyring; `validate` checks a key typed in at the prompt, with the
/// email, before it is saved.
pub fn credentials(
    store: &dyn SecretStore,
    validate: &dyn Fn(&str, &str) -> sandogasa_cli::credential::KeyVerdict,
) -> Result<Credentials, Box<dyn std::error::Error>> {
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
                store,
                None,
                &crate::secrets::bugzilla(),
                in_file.as_deref(),
            ) {
                Some(k) => k,
                None => {
                    eprintln!(
                        "No Bugzilla API key set: none in the desktop keyring, {} or \
                         $BUGZILLA_API_KEY.",
                        file.path().display()
                    );
                    eprint!(
                        "{}",
                        sandogasa_config::credentials::token_help(
                            "sandogasa: Bugzilla API key",
                            "fedora-review-digest",
                            crate::secrets::BUGZILLA_URL,
                        )
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

    // The tool's own email, else the one the sandogasa tools share
    // (#259); one typed in here becomes the shared one once saved.
    let email_host = identity::host_of(crate::secrets::BUGZILLA_URL);
    let mut typed_email = None;
    let email = match env_nonempty("BUGZILLA_EMAIL") {
        Some(e) => e,
        None => {
            match identity::resolve(&identity::BUGZILLA_EMAIL, &email_host, &cfg.bugzilla.email) {
                Some(e) => e,
                None => {
                    cfg.bugzilla.email = sandogasa_config::prompt_field(
                        "Bugzilla",
                        "email",
                        false,
                        Some(&sandogasa_config::validate_email),
                    )?;
                    dirty = true;
                    typed_email = Some(cfg.bugzilla.email.clone());
                    cfg.bugzilla.email.clone()
                }
            }
        }
    };
    // Asked mid-run, for --post: nothing else is asked about, and the
    // other tools' copies are left as they are.
    let share = |typed: &Option<String>| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(e) = typed {
            identity::settle(
                &identity::BUGZILLA_EMAIL,
                &email_host,
                "fedora-review-digest",
                e,
                &sandogasa_config::secret::never,
            )?;
        }
        Ok(())
    };

    if prompted_key {
        // A key typed in now is checked before it is kept, as `config`
        // checks one (#164): an expired one is named as such, with the
        // chance to enter another, instead of failing the post later.
        let checked = sandogasa_cli::credential::validated_key_blocking(
            "Bugzilla",
            "API key",
            cfg.bugzilla.api_key.clone(),
            |key| validate(key, &email),
        );
        cfg.bugzilla.api_key = checked.key;
        // None was found, so there is nothing to turn off: a new key.
        save(&file, store, &mut cfg, &[Before::Absent])?;
        share(&typed_email)?;
        return Ok(Credentials {
            api_key: cfg.bugzilla.api_key.clone(),
            email,
        });
    } else if dirty {
        file.save(&cfg)?;
        share(&typed_email)?;
    }
    Ok(Credentials { api_key, email })
}

/// The secrets `cfg` holds: the Bugzilla API key.
fn slots(cfg: &mut Config) -> Vec<Slot<'_>> {
    vec![Slot {
        table: "bugzilla",
        key: "api_key",
        what: "Bugzilla API key",
        entry: crate::secrets::bugzilla(),
        value: &mut cfg.bugzilla.api_key,
        // `--post` cannot work without it.
        required: true,
    }]
}

/// Fill `cfg`'s API key from `store`, the keyring, which wins over the
/// file's, so `config` offers it as the saved value (see
/// [`sandogasa_config::secret::fill`], which `review`, the `--all` run,
/// and `ask` go to). How it stood, for [`save`].
pub fn fill(
    store: &dyn SecretStore,
    cfg: &mut Config,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
) -> Vec<Before> {
    sandogasa_config::secret::fill(store, env!("CARGO_PKG_NAME"), slots(cfg), review, ask)
}

/// Save `cfg` to `file`, the API key in `store`, the keyring, when there
/// is one and out of the file then (see
/// [`sandogasa_config::secret::save_with`]); `before` is what [`fill`]
/// said. Whether the file is still there, which it is not once nothing
/// is left in it.
pub fn save(
    file: &ConfigFile,
    store: &dyn SecretStore,
    cfg: &mut Config,
    before: &[Before],
) -> Result<bool, Box<dyn std::error::Error>> {
    sandogasa_config::secret::save_with(file, store, env!("CARGO_PKG_NAME"), cfg, before, slots)
}

/// A non-empty environment variable, trimmed away when blank/unset.
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_diff_tool_is_read_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let file = ConfigFile::from_path(path.clone());
        // No file, or no [diff]: none.
        assert_eq!(diff_tool_in(&file), None);
        std::fs::write(&path, "[bugzilla]\nemail = \"a@b\"\n").unwrap();
        assert_eq!(diff_tool_in(&file), None);
        std::fs::write(&path, "[diff]\ntool = \"difft --display inline\"\n").unwrap();
        assert_eq!(
            diff_tool_in(&file).as_deref(),
            Some("difft --display inline")
        );
        std::fs::write(&path, "[diff]\ntool = \"  \"\n").unwrap();
        assert_eq!(diff_tool_in(&file), None);
    }

    #[test]
    fn an_unset_diff_tool_is_not_written() {
        let cfg = Config::default();
        assert!(!serde_json::to_string(&cfg).unwrap().contains("diff"));
    }

    /// --post's credentials take the email the sandogasa tools share
    /// when the tool has none of its own (#259), without asking, and the
    /// tool's own over it.
    #[test]
    #[serial_test::serial]
    fn post_takes_the_shared_email_unless_the_tool_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_str().unwrap();
        let _env = sandogasa_config::test_support::EnvGuard::new(&[
            ("XDG_CONFIG_HOME", root),
            ("SANDOGASA_ETC", &format!("{root}/etc")),
            ("BUGZILLA_API_KEY", "key"),
            ("BUGZILLA_EMAIL", ""),
        ]);
        std::fs::create_dir_all(dir.path().join("sandogasa")).unwrap();
        std::fs::write(
            dir.path().join("sandogasa/config.toml"),
            "[bugzilla.\"bugzilla.redhat.com\"]\nemail = \"shared@example.com\"\n",
        )
        .unwrap();
        let store = sandogasa_config::secret::MemoryStore::default();
        let unchecked = |_: &str, _: &str| panic!("checked a key");
        let creds = credentials(&store, &unchecked).unwrap();
        assert_eq!(creds.email, "shared@example.com");
        assert!(!dir.path().join("fedora-review-digest").exists());
        std::fs::create_dir_all(dir.path().join("fedora-review-digest")).unwrap();
        std::fs::write(
            dir.path().join("fedora-review-digest/config.toml"),
            "[bugzilla]\nemail = \"own@example.com\"\n",
        )
        .unwrap();
        assert_eq!(
            credentials(&store, &unchecked).unwrap().email,
            "own@example.com"
        );
    }

    /// Where the identity registry says the tool keeps the Bugzilla
    /// email is where its config reads it from.
    #[test]
    fn identity_fields_match_the_config() {
        let fields: Vec<_> = identity::fields_of("fedora-review-digest").collect();
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].identity, &identity::BUGZILLA_EMAIL);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, fields[0].toml("x@example.com")).unwrap();
        let cfg: Config = ConfigFile::from_path(path).load().unwrap();
        assert_eq!(cfg.bugzilla.email, "x@example.com");
        let empty = serde_json::to_string(&Config::default()).unwrap();
        assert!(!empty.contains("email"), "{empty}");
    }
}
