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
    if let Some(cfg) = existing {
        eprint!("Validating existing GitLab token... ");
        match sandogasa_gitlab::validate_token(&gitlab_base(), &cfg.access_token) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(cfg.access_token.clone());
            }
            Ok(false) => eprintln!("invalid."),
            Err(e) => eprintln!("check failed ({e}); re-prompting."),
        }
    }

    let token =
        rpassword::prompt_password("Paste a GitLab personal access token with 'api' scope: ")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err("no GitLab token provided".into());
    }

    eprint!("Validating GitLab token... ");
    if !sandogasa_gitlab::validate_token(&gitlab_base(), &token)? {
        return Err("GitLab token is invalid".into());
    }
    eprintln!("valid.");
    Ok(token)
}

/// Red Hat's tracker is an Atlassian Cloud site, so the credentials
/// are an API token from id.atlassian.com beside the account's
/// email; both are checked against `myself` before being kept.
fn prompt_jira(
    existing: Option<&JiraConfig>,
) -> Result<Option<JiraConfig>, Box<dyn std::error::Error>> {
    if let Some(cfg) = existing {
        eprint!("Validating existing JIRA credentials... ");
        match crate::jira::whoami(cfg.email.clone(), cfg.access_token.clone()) {
            Ok(me) => {
                eprintln!("valid ({}).", me.display_name);
                return Ok(Some(JiraConfig {
                    email: cfg.email.clone(),
                    access_token: cfg.access_token.clone(),
                }));
            }
            Err(e) => eprintln!("{e}; re-prompting."),
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
