// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively set up GitLab and JIRA
//! credentials in `~/.config/cpu-sig-tracker/config.toml`.

use std::process::ExitCode;

use crate::config::{self, Config, GitlabConfig, JiraConfig};
use crate::utils::{gitlab_base, jira_base};

pub fn run() -> ExitCode {
    match run_inner() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner() -> Result<(), Box<dyn std::error::Error>> {
    let existing = config::load().ok().unwrap_or_default();

    let gitlab_token = prompt_gitlab_token(existing.gitlab.as_ref())?;
    let jira = prompt_jira(existing.jira.as_ref())?;

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
) -> Result<String, Box<dyn std::error::Error>> {
    let have = existing.map(|c| c.access_token.clone()).unwrap_or_default();
    if have.is_empty() {
        eprintln!("A GitLab personal access token with 'api' scope is needed.");
    }
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
) -> Result<Option<JiraConfig>, Box<dyn std::error::Error>> {
    // The credentials are a pair, so a refusal may be either half —
    // which is why this falls back to asking for both rather than
    // replacing the token alone.
    if let Some(cfg) = existing {
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

    eprintln!(
        "JIRA at {} is an Atlassian Cloud site. Create an API token at\n  \
         https://id.atlassian.com/manage-profile/security/api-tokens\n\
         (a scoped token needs read:jira-user and read:jira-work, and\n  \
         write:jira-work to comment)\n\
         and enter it with the email of the Atlassian account it belongs to.\n\
         Empty to skip; anonymous access works for public issues.",
        jira_base(),
    );
    use std::io::{BufRead, Write};
    eprint!("Atlassian account email: ");
    std::io::stderr().flush()?;
    let mut email = String::new();
    std::io::stdin().lock().read_line(&mut email)?;
    let email = email.trim().to_string();
    if email.is_empty() {
        return Ok(None);
    }
    let token = rpassword::prompt_password("API token: ")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("no JIRA token provided".into());
    }

    eprint!("Validating JIRA credentials... ");
    let me = crate::jira::whoami(Some(email.clone()), token.clone())?;
    eprintln!("valid ({}).", me.display_name);
    Ok(Some(JiraConfig {
        email: Some(email),
        access_token: token,
    }))
}
