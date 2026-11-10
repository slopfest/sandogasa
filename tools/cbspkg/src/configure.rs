// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand: set up the GitLab token, in the desktop keyring
//! when there is one, else in `~/.config/cbspkg/config.toml`.

use std::process::ExitCode;

use crate::config::{self, Config, GitlabConfig};

pub fn run(args: &sandogasa_cli::ConfigArgs) -> ExitCode {
    match run_inner(args.all) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Ask for the token when it is missing, or with `all` again with the
/// saved one as the default, check it with GitLab, and save it.
fn run_inner(all: bool) -> Result<(), Box<dyn std::error::Error>> {
    let existing: Config = config::load_user().ok().unwrap_or_default();
    // The saved token: the keyring's, else the file's (#79).
    let file = existing.gitlab.as_ref().map(|g| g.access_token.clone());
    let saved = sandogasa_config::secret::resolve(
        config::keyring(),
        None,
        &config::gitlab_secret(),
        file.as_deref(),
    )
    .unwrap_or_default();
    let have = sandogasa_config::ask(
        &sandogasa_config::Setting::new("GitLab", "token")
            .sensitive()
            .hint(
                "A GitLab personal access token with the `api` scope, created at \
                 https://gitlab.com/-/user_settings/personal_access_tokens. Creating a \
                 SIG repo needs the Maintainer role in the SIG's group.",
            ),
        &saved,
        all,
    )?;
    let base = config::gitlab_base();
    let token =
        sandogasa_cli::credential::validated_key_blocking("GitLab", "token", have, |token| {
            match sandogasa_gitlab::validate_token(&base, token) {
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
    // Into the keyring when there is one, out of the file then.
    let token = sandogasa_config::secret::keep_for_file(
        config::keyring(),
        "GitLab token",
        &config::gitlab_secret(),
        token,
    );
    let in_keyring = token.is_empty();
    config::save(&Config {
        gitlab: (!in_keyring).then_some(GitlabConfig {
            access_token: token,
        }),
        // Not written: save() keeps the file's [packages] as they are.
        ..Default::default()
    })?;
    // save() lays the struct over the file and never removes a key, so
    // a token now in the keyring is taken out of the file here.
    let removed = in_keyring && config::remove("gitlab", "access_token")?;
    report_saved(removed, &config::config_path());
    Ok(())
}

/// Where the settings went: the file, or nowhere, when it was left with
/// nothing in it and removed.
fn report_saved(removed: bool, path: &std::path::Path) {
    if removed {
        eprintln!("Nothing else to keep in {}; removed it.", path.display());
    } else {
        eprintln!("Saved to {}", path.display());
    }
}
