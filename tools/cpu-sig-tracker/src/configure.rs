// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively set up GitLab and JIRA
//! authentication tokens in `~/.config/cpu-sig-tracker/config.toml`.

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
    let jira_token = prompt_jira_token(existing.jira.as_ref())?;

    let cfg = Config {
        gitlab: Some(GitlabConfig {
            access_token: gitlab_token,
        }),
        jira: jira_token.map(|t| JiraConfig { access_token: t }),
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

fn prompt_jira_token(
    existing: Option<&JiraConfig>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if existing.is_some() {
        eprintln!("JIRA token already configured at {}.", jira_base(),);
        eprint!("Replace it? [y/N]: ");
        use std::io::{BufRead, Write};
        std::io::stderr().flush()?;
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if !line.trim().eq_ignore_ascii_case("y") {
            return Ok(existing.map(|c| c.access_token.clone()));
        }
    }

    eprintln!(
        "Paste a JIRA personal access token for {} (empty to skip; \
        anonymous access works for public issues).",
        jira_base(),
    );
    let token = rpassword::prompt_password("JIRA token: ")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}
