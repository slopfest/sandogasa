// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand: set up the GitLab token, in the desktop keyring
//! when there is one, else in `~/.config/cbspkg/config.toml`.

use std::process::ExitCode;

use crate::config;

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
    let secret = config::gitlab_token_secret();
    let cf = config::config_file();
    let (saved, before) =
        secret.saved(&cf, config::keyring(), all, &sandogasa_cli::ask_to_reenable)?;
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
    // Into the keyring when there is one, out of the file then; the
    // file's [packages] stay as they are.
    let kept = secret.save(&cf, config::keyring(), token, &before)?;
    report_saved(!kept, cf.path());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::EnvGuard;

    /// A GitLab answering `status` for the token check, and the env
    /// pointing the tool and its config at it and a temp dir.
    fn gitlab(
        dir: &std::path::Path,
        status: usize,
    ) -> (mockito::ServerGuard, mockito::Mock, EnvGuard) {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", "/api/v4/user")
            .with_status(status)
            .create();
        let config = dir.display().to_string();
        let env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &config),
            ("CBSPKG_GITLAB_BASE", &server.url()),
        ])
        .unset(&["GITLAB_TOKEN"]);
        (server, mock, env)
    }

    /// The token saved in the file before the keyring (#79) moves to
    /// the keyring, and the file keeps the rest.
    #[test]
    #[serial_test::serial]
    fn a_saved_token_is_checked_and_moved_to_the_keyring() {
        let tmp = tempfile::tempdir().unwrap();
        let (_server, mock, _env) = gitlab(tmp.path(), 200);
        std::fs::create_dir_all(tmp.path().join("cbspkg")).unwrap();
        std::fs::write(
            config::config_path(),
            "[gitlab]\naccess_token = \"glpat-x\"\n\n[packages.perf]\nrhel = [\"facebook\"]\n",
        )
        .unwrap();
        // ConfigArgs is non-exhaustive: made the way the CLI makes it.
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            config: sandogasa_cli::ConfigArgs,
        }
        let cli = <Cli as clap::Parser>::parse_from(["cbspkg"]);
        assert_eq!(run(&cli.config), ExitCode::SUCCESS);
        mock.assert();
        assert_eq!(
            config::keyring()
                .lookup(&config::gitlab_secret())
                .unwrap()
                .as_deref(),
            Some("glpat-x")
        );
        let left: config::Config = config::config_file().load_user().unwrap();
        assert!(left.gitlab.is_none());
        assert_eq!(left.packages["perf"].rhel, ["facebook"]);
        config::keyring().clear(&config::gitlab_secret()).unwrap();
    }

    /// The keyring's token, kept when GitLab cannot be asked; a file
    /// with nothing else in it is removed.
    #[test]
    #[serial_test::serial]
    fn a_token_that_cannot_be_checked_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (_server, _mock, _env) = gitlab(tmp.path(), 200);
        // Nobody listening: the check cannot be made.
        let _down = EnvGuard::new(&[("CBSPKG_GITLAB_BASE", "http://127.0.0.1:9")]);
        config::keyring()
            .store(&config::gitlab_secret(), "glpat-y")
            .unwrap();
        std::fs::create_dir_all(tmp.path().join("cbspkg")).unwrap();
        std::fs::write(config::config_path(), "[gitlab]\naccess_token = \"old\"\n").unwrap();
        run_inner(false).unwrap();
        assert!(!config::config_path().exists());
        assert_eq!(
            config::keyring()
                .lookup(&config::gitlab_secret())
                .unwrap()
                .as_deref(),
            Some("glpat-y")
        );
        config::keyring().clear(&config::gitlab_secret()).unwrap();
    }

    /// The token is required, so one turned off for cbspkg is asked
    /// about by name, with yes the default.
    /// Serial: the keyring entry follows `CBSPKG_GITLAB_BASE`, which
    /// other tests set.
    #[test]
    #[serial_test::serial]
    fn an_off_token_is_asked_about_for_cbspkg() {
        use sandogasa_config::secret::{MemoryStore, SecretStore};
        let _env = EnvGuard::new(&[("CBSPKG_GITLAB_BASE", "http://127.0.0.1:9")]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gitlab]\naccess_token = \"-\"\n").unwrap();
        let cf = sandogasa_config::ConfigFile::from_path(path);
        let store = MemoryStore::default();
        store
            .store(&config::gitlab_secret(), "glpat-shared")
            .unwrap();
        let (saved, _) = config::gitlab_token_secret()
            .saved(&cf, &store, false, &|q, default_yes| {
                assert!(q.contains("GitLab token: off for cbspkg"), "{q}");
                assert!(default_yes);
                true
            })
            .unwrap();
        assert_eq!(saved, "glpat-shared");
    }

    #[test]
    fn where_the_settings_went_is_reported_either_way() {
        let path = std::path::Path::new("/nonexistent/cbspkg/config.toml");
        report_saved(true, path);
        report_saved(false, path);
    }
}
