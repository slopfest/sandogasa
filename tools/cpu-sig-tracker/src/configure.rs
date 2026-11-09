// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively set up GitLab and JIRA
//! credentials in `~/.config/cpu-sig-tracker/config.toml`.

use std::process::ExitCode;

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
    let existing = config::load().ok().unwrap_or_default();

    let gitlab_token = prompt_gitlab_token(existing.gitlab.as_ref(), all)?;
    let jira = prompt_jira(existing.jira.as_ref(), all)?;

    let cfg = Config {
        gitlab: Some(GitlabConfig {
            access_token: gitlab_token,
        }),
        jira,
    };

    config::save(&cfg)?;
    eprintln!("Saved to {}", config::config_path().display());
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
fn prompt_jira(
    existing: Option<&JiraConfig>,
    all: bool,
) -> Result<Option<JiraConfig>, Box<dyn std::error::Error>> {
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

    eprint!("Validating JIRA credentials... ");
    let me = crate::jira::whoami(Some(email.clone()), token.clone())?;
    eprintln!("valid ({}).", me.display_name);
    Ok(Some(JiraConfig {
        email: Some(email),
        access_token: token,
    }))
}
