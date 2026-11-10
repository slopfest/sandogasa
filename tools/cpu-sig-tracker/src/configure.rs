// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively set up GitLab and JIRA
//! credentials: the tokens in the desktop keyring when there is one,
//! the rest in `~/.config/cpu-sig-tracker/config.toml`.

use std::process::ExitCode;

use sandogasa_config::secret::{self, Before};

use crate::config::{self, Config, GitlabConfig, JiraConfig};
use crate::utils::{gitlab_base, jira_base};

pub fn run(args: &sandogasa_cli::ConfigArgs) -> ExitCode {
    match run_inner(args.all) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Ask for what is missing, or with `all` for every setting with the
/// saved value as the default, check the credentials, and save.
fn run_inner(all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let (existing, before) = saved(all, &sandogasa_cli::ask_to_reenable);
    let gitlab_token = prompt_gitlab_token(existing.gitlab.as_ref(), all)?;
    let jira = prompt_jira(existing.jira.as_ref(), all)?;
    let mut cfg = to_save(gitlab_token, jira, existing.jira);
    save(&mut cfg, &before)
}

/// The settings saved in the user's file alone, never `/etc`'s, which
/// saving would copy into it; the tokens the keyring's first (#79), and
/// how each stood. `review` is the `--all` run, and `ask` whether to use
/// the keyring's entry for a token turned off again. The GitLab table is
/// always there, so the slots line up with those saved.
fn saved(review: bool, ask: &dyn Fn(&str, bool) -> bool) -> (Config, Vec<Before>) {
    let mut existing: Config = config::config_file().load_user().ok().unwrap_or_default();
    existing.gitlab.get_or_insert_with(Default::default);
    let before = secret::fill(
        crate::secrets::keyring(),
        "cpu-sig-tracker",
        crate::secrets::slots(&mut existing),
        review,
        ask,
    );
    (existing, before)
}

/// What `config` saves: the answers, with a Jira set up before and
/// skipped now kept with its token cleared, which turns it off for
/// the tool rather than leaving the old one in use.
fn to_save(gitlab_token: String, jira: Option<JiraConfig>, before: Option<JiraConfig>) -> Config {
    Config {
        gitlab: Some(GitlabConfig {
            access_token: gitlab_token,
        }),
        jira: jira.or_else(|| {
            before.map(|j| JiraConfig {
                access_token: String::new(),
                ..j
            })
        }),
    }
}

/// Keep the tokens in the keyring when there is one, `before` being how
/// each stood before the prompts, and save the rest.
fn save(cfg: &mut Config, before: &[Before]) -> Result<(), Box<dyn std::error::Error>> {
    let kept = secret::save_with(
        &config::config_file(),
        crate::secrets::keyring(),
        "cpu-sig-tracker",
        cfg,
        before,
        crate::secrets::slots,
    )?;
    let path = config::config_path();
    if kept {
        eprintln!("Saved to {}", path.display());
    } else {
        eprintln!("Nothing else to keep in {}; removed it.", path.display());
    }
    Ok(())
}

fn prompt_gitlab_token(
    existing: Option<&GitlabConfig>,
    all: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    let saved = existing.map(|c| c.access_token.clone()).unwrap_or_default();
    let have = sandogasa_config::ask(
        &sandogasa_config::Setting::new("GitLab", "token")
            .sensitive()
            .hint(
                "A GitLab legacy (classic) personal access token with the `api` scope is \
                 needed, created at https://gitlab.com/-/user_settings/personal_access_tokens.",
            ),
        &saved,
        all,
    )?;
    let token =
        sandogasa_cli::credential::validated_key_blocking("GitLab", "token", have, |token| {
            match sandogasa_gitlab::validate_token(&gitlab_base(), token) {
                Ok(true) => sandogasa_cli::credential::KeyVerdict::Accepted("valid.".to_string()),
                Ok(false) => {
                    sandogasa_cli::credential::KeyVerdict::Refused("not accepted.".to_string())
                }
                Err(e) => {
                    sandogasa_cli::credential::KeyVerdict::Unknown(format!("could not check: {e}"))
                }
            }
        })
        .key;
    if token.is_empty() {
        return Err("no GitLab token provided".into());
    }
    Ok(token)
}

/// Red Hat's tracker is an Atlassian Cloud site, so the credentials
/// are an API token from id.atlassian.com beside the account's
/// email; both are checked against `myself` before being kept.
///
/// A token turned off for the tool (skipped at an earlier run) is left
/// off, unasked, on a plain run; `--all` offers it again.
fn prompt_jira(
    existing: Option<&JiraConfig>,
    all: bool,
) -> Result<Option<JiraConfig>, Box<dyn std::error::Error>> {
    if let Some(cfg) = existing.filter(|c| c.access_token == secret::OFF && !all) {
        eprintln!("JIRA: off for cpu-sig-tracker (`config --all` to set it up).");
        return Ok(Some(JiraConfig {
            email: cfg.email.clone(),
            access_token: secret::OFF.to_string(),
        }));
    }
    // The credentials are a pair, so a refusal may be either half —
    // which is why this falls back to asking for both rather than
    // replacing the token alone.
    if let Some(cfg) = existing.filter(|_| !all) {
        let email = cfg.email.clone();
        let checked = sandogasa_cli::credential::validated_key_blocking(
            "JIRA",
            "API token",
            cfg.access_token.clone(),
            |token| match crate::jira::whoami(email.clone(), token.to_string()) {
                Ok(me) => sandogasa_cli::credential::KeyVerdict::Accepted(format!(
                    "valid ({}).",
                    me.display_name
                )),
                Err(e) => sandogasa_cli::credential::KeyVerdict::Refused(format!(
                    "{e} — the token, or the account it belongs to, is not the one JIRA has."
                )),
            },
        );
        if checked.accepted {
            return Ok(Some(JiraConfig {
                email: cfg.email.clone(),
                access_token: checked.key,
            }));
        }
    }

    let hint = format!(
        "JIRA at {} is an Atlassian Cloud site. Create an API token at\n  \
         https://id.atlassian.com/manage-profile/security/api-tokens\n\
         (a scoped token needs read:jira-user and read:jira-work, and\n  \
         write:jira-work to comment)\n\
         and enter it with the email of the Atlassian account it belongs to.\n\
         Empty to skip; anonymous access works for public issues.",
        jira_base(),
    );
    // With --all, or after a refusal, the saved pair is offered as the
    // default: Enter keeps each half.
    let review = existing.is_some();
    let saved_email = existing.and_then(|c| c.email.clone()).unwrap_or_default();
    let email = sandogasa_config::ask(
        &sandogasa_config::Setting::new("Atlassian account", "email (empty to skip)")
            .optional()
            .hint(&hint)
            .validated(&sandogasa_config::validate_email),
        &saved_email,
        review,
    )?;
    if email.is_empty() {
        return Ok(None);
    }
    let saved_token = existing.map(|c| c.access_token.clone()).unwrap_or_default();
    let token = sandogasa_config::ask(
        &sandogasa_config::Setting::new("JIRA", "API token").sensitive(),
        &saved_token,
        review,
    )?;

    if !secret::is_set(&token) {
        return Err("no JIRA API token provided".into());
    }
    eprint!("Validating JIRA credentials... ");
    let me = crate::jira::whoami(Some(email.clone()), token.clone())?;
    eprintln!("valid ({}).", me.display_name);
    Ok(Some(JiraConfig {
        email: Some(email),
        access_token: token,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;
    use sandogasa_config::secret::{OFF, never};

    /// The config file in a temp dir, `/etc` an empty one beside it,
    /// and the keyring entries pointed at test hosts.
    fn env(dir: &std::path::Path) -> EnvGuard {
        EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.to_string_lossy()),
            ("SANDOGASA_ETC", &dir.join("etc").to_string_lossy()),
            (
                "CPU_SIG_TRACKER_GITLAB_BASE",
                "https://gitlab.configure.test",
            ),
            ("CPU_SIG_TRACKER_JIRA_BASE", "https://jira.configure.test"),
        ])
        .unset(&["GITLAB_TOKEN", "JIRA_TOKEN", "JIRA_EMAIL"])
    }

    fn write_config(text: &str) {
        let path = config::config_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn read_config() -> String {
        std::fs::read_to_string(config::config_path()).unwrap()
    }

    fn clear_keyring() {
        let k = crate::secrets::keyring();
        k.clear(&crate::secrets::gitlab()).unwrap();
        k.clear(&crate::secrets::jira(Some("me@example.com")))
            .unwrap();
    }

    /// Tokens in an old file are offered keyring-first, then moved to
    /// the keyring, the file keeping the Jira account's email.
    #[test]
    #[serial_test::serial]
    fn saved_tokens_are_offered_from_the_keyring_and_moved_there() {
        let dir = tempfile::tempdir().unwrap();
        let _env = env(dir.path());
        clear_keyring();
        write_config(
            "[gitlab]\naccess_token = \"glpat-stale\"\n\n\
             [jira]\nemail = \"me@example.com\"\naccess_token = \"jira-in-file\"\n",
        );
        let keyring = crate::secrets::keyring();
        keyring
            .store(&crate::secrets::gitlab(), "glpat-newer")
            .unwrap();

        let (mut existing, before) = saved(false, &never);
        assert_eq!(
            before,
            [
                Before::Set {
                    keyring: Some("glpat-newer".into())
                },
                Before::Set { keyring: None },
            ]
        );
        assert_eq!(
            existing.gitlab.as_ref().unwrap().access_token,
            "glpat-newer"
        );
        assert_eq!(existing.jira.as_ref().unwrap().access_token, "jira-in-file");

        // Enter at every prompt keeps what was offered.
        let jira = existing.jira.take();
        let mut cfg = to_save("glpat-newer".into(), jira, None);
        save(&mut cfg, &before).unwrap();
        let text = read_config();
        assert!(!text.contains("access_token"), "{text}");
        assert!(text.contains("email = \"me@example.com\""), "{text}");
        assert_eq!(
            keyring
                .lookup(&crate::secrets::jira(Some("me@example.com")))
                .unwrap()
                .as_deref(),
            Some("jira-in-file")
        );
        assert_eq!(crate::gitlab::load_token().unwrap(), "glpat-newer");
        clear_keyring();
    }

    /// Skipping Jira that was set up turns it off for the tool, the
    /// keyring entry kept; a run without `--all` keeps it off without
    /// asking; `--all` can take the keyring's entry back.
    #[test]
    #[serial_test::serial]
    fn skipping_jira_turns_it_off_and_all_can_turn_it_back_on() {
        let dir = tempfile::tempdir().unwrap();
        let _env = env(dir.path());
        clear_keyring();
        write_config("[jira]\nemail = \"me@example.com\"\n");
        let keyring = crate::secrets::keyring();
        let entry = crate::secrets::jira(Some("me@example.com"));
        keyring.store(&entry, "jira-shared").unwrap();
        let unasked = |q: &str, _: bool| -> bool { panic!("asked {q}") };

        let (existing, before) = saved(false, &unasked);
        assert_eq!(before[0], Before::Absent);
        // The email cleared at its prompt: no Jira.
        let mut cfg = to_save("glpat-1".into(), None, existing.jira);
        save(&mut cfg, &before).unwrap();
        assert!(read_config().contains("access_token = \"-\""));
        assert_eq!(
            keyring.lookup(&entry).unwrap().as_deref(),
            Some("jira-shared")
        );
        // Off: the keyring's token is not used.
        assert!(!crate::jira::logged_in());

        // A plain run: left off, nothing asked, and saved off again.
        let (existing, before) = saved(false, &unasked);
        assert_eq!(before[1], Before::Off);
        let jira = prompt_jira(existing.jira.as_ref(), false).unwrap().unwrap();
        assert_eq!(jira.access_token, OFF);
        assert_eq!(jira.email.as_deref(), Some("me@example.com"));
        let mut cfg = to_save("glpat-1".into(), Some(jira), None);
        save(&mut cfg, &before).unwrap();
        assert!(read_config().contains("access_token = \"-\""));

        // With --all it is offered: asked, defaulting to no, and a no
        // offers it blank.
        let (existing, before) = saved(true, &|q, default_yes| {
            assert!(q.contains("JIRA API token: off for cpu-sig-tracker"), "{q}");
            assert!(!default_yes, "optional: not the default");
            false
        });
        assert_eq!(before[1], Before::Off);
        assert_eq!(existing.jira.as_ref().unwrap().access_token, "");

        // A yes takes the entry back, and Enter keeps it.
        let (mut existing, before) = saved(true, &|_, _| true);
        assert_eq!(existing.jira.as_ref().unwrap().access_token, "jira-shared");
        let mut cfg = to_save("glpat-1".into(), existing.jira.take(), None);
        save(&mut cfg, &before).unwrap();
        let text = read_config();
        assert!(!text.contains("access_token"), "{text}");
        assert!(crate::jira::logged_in());
        assert_eq!(
            keyring.lookup(&entry).unwrap().as_deref(),
            Some("jira-shared")
        );
        clear_keyring();
    }

    /// The GitLab token is required: turned off, it is offered again on
    /// a plain run, using the keyring's entry being the default.
    #[test]
    #[serial_test::serial]
    fn an_off_gitlab_token_is_asked_about_with_yes_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let _env = env(dir.path());
        clear_keyring();
        write_config("[gitlab]\naccess_token = \"-\"\n");
        let asked = std::cell::Cell::new(false);
        let ask = |q: &str, default_yes: bool| {
            assert!(q.contains("GitLab token: off for cpu-sig-tracker"), "{q}");
            assert!(default_yes, "required: the default");
            asked.set(true);
            true
        };
        // Nothing in the keyring to take back: offered blank, unasked.
        let (existing, before) = saved(false, &ask);
        assert!(!asked.get());
        assert_eq!(before[0], Before::Off);
        assert_eq!(existing.gitlab.unwrap().access_token, "");
        crate::secrets::keyring()
            .store(&crate::secrets::gitlab(), "glpat-shared")
            .unwrap();
        let (existing, before) = saved(false, &ask);
        assert!(asked.get());
        assert_eq!(existing.gitlab.unwrap().access_token, "glpat-shared");
        assert_eq!(
            before[0],
            Before::Set {
                keyring: Some("glpat-shared".into())
            }
        );
        clear_keyring();
    }

    /// `config` saves what the user's file holds, never what `/etc`
    /// ships, which the tool still reads beneath it.
    #[test]
    #[serial_test::serial]
    fn an_etc_value_is_not_copied_into_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        let _env = env(dir.path());
        clear_keyring();
        let etc = dir.path().join("etc/cpu-sig-tracker");
        std::fs::create_dir_all(&etc).unwrap();
        std::fs::write(
            etc.join("config.toml"),
            "[jira]\nemail = \"etc@example.com\"\n",
        )
        .unwrap();
        write_config("[gitlab]\naccess_token = \"glpat-1\"\n");
        assert_eq!(
            config::load().unwrap().jira.unwrap().email.as_deref(),
            Some("etc@example.com"),
            "the tool reads /etc"
        );

        let (mut existing, before) = saved(false, &never);
        assert!(existing.jira.is_none(), "config does not");
        let mut cfg = to_save("glpat-1".into(), existing.jira.take(), None);
        save(&mut cfg, &before).unwrap();
        // The token went to the keyring, and /etc's Jira email was not
        // saved in its place, so nothing was left to keep.
        assert!(!config::config_path().exists());
        assert_eq!(
            crate::gitlab::load_token().unwrap(),
            "glpat-1",
            "from the keyring"
        );
        clear_keyring();
    }
}
