// SPDX-License-Identifier: MPL-2.0

use clap::{Parser, Subcommand};
use hs_relmon::cbs;
use hs_relmon::check_latest::{self, Distros, TrackRef};
use hs_relmon::config;
use hs_relmon::gitlab;
use hs_relmon::repology;

#[derive(Parser)]
#[command(name = "hs-relmon", about = "Hyperscale release monitoring")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the latest version of a package across distributions.
    CheckLatest {
        /// Source package name (e.g. ethtool).
        package: String,

        /// Comma-separated list of distros to check.
        #[arg(short, long, long_help = "\
Comma-separated list of distros to check.

Valid names:
  upstream         Newest version across all repos
  fedora           Fedora Rawhide + latest stable
  fedora-rawhide   Fedora Rawhide only
  fedora-stable    Latest stable Fedora only
  centos           Latest CentOS Stream
  centos-stream    Latest CentOS Stream
  hyperscale / hs  Hyperscale EL9 + EL10
  hs9              Hyperscale EL9 only
  hs10             Hyperscale EL10 only")]
        distros: Option<String>,

        /// Override Repology project name.
        #[arg(long, value_name = "PROJECT", long_help = "\
Override Repology project name when it differs
from the package (e.g. linux for perf).")]
        repology_name: Option<String>,

        /// Reference distribution.
        #[arg(long, default_value = "upstream", long_help = "\
Distribution to compare Hyperscale builds against.

Valid names:
  upstream         Newest version across all repos (default)
  fedora-rawhide   Fedora Rawhide
  fedora-stable    Latest stable Fedora
  centos           Latest CentOS Stream
  centos-stream    Latest CentOS Stream")]
        track: String,

        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,

        /// File a GitLab issue if outdated.
        #[arg(long, num_args = 0..=1, default_missing_value = "",
            value_name = "URL", long_help = "\
File or update a GitLab issue if the package is
outdated. Searches for an open issue labeled
rfe::new-version and updates its title, or creates
a new one. Defaults to
https://gitlab.com/CentOS/Hyperscale/rpms/PKG.
Uses the token from 'hs-relmon config', or
GITLAB_TOKEN env var as an override.")]
        file_issue: Option<String>,
    },

    /// Configure GitLab authentication.
    Config,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::CheckLatest {
            package,
            distros,
            repology_name,
            track,
            json,
            file_issue,
        } => {
            let distros = match distros {
                Some(s) => Distros::parse(&s)?,
                None => Distros::all(),
            };
            let track = TrackRef::parse(&track)?;
            let repology_name = repology_name.as_deref().unwrap_or(&package);

            let repology_client = repology::Client::new();
            let cbs_client = cbs::Client::new();
            let result = check_latest::check(
                &repology_client,
                &cbs_client,
                &package,
                repology_name,
                &distros,
                &track,
            )?;

            if json {
                check_latest::print_json(&result)?;
            } else {
                check_latest::print_table(&result);
            }

            if let Some(url_override) = &file_issue {
                if result.is_outdated() {
                    let project_url = if url_override.is_empty() {
                        format!(
                            "https://gitlab.com/CentOS/\
                            Hyperscale/rpms/{package}"
                        )
                    } else {
                        url_override.clone()
                    };
                    let ref_ver = result.ref_version().ok_or(
                        "no reference version available",
                    )?;
                    let title = format!(
                        "{package}-{ref_ver} is available"
                    );
                    let description = format!(
                        "```\n{}```",
                        check_latest::format_table(&result)
                    );
                    file_or_update_issue(
                        &project_url, &title, &description,
                    )?;
                }
            }
        }
        Command::Config => {
            configure_gitlab()?;
        }
    }

    Ok(())
}

const GITLAB_BASE: &str = "https://gitlab.com";

fn configure_gitlab() -> Result<(), Box<dyn std::error::Error>> {
    let existing_token = config::load()
        .ok()
        .and_then(|c| c.gitlab.map(|g| g.access_token));

    if let Some(token) = &existing_token {
        eprint!("Validating existing token... ");
        if gitlab::validate_token(GITLAB_BASE, token)? {
            eprintln!("valid.");
            return Ok(());
        }
        eprintln!("invalid.");
    }

    let token = rpassword::prompt_password(
        "Paste a GitLab personal access token \
        with 'api' scope: ",
    )?;
    if token.is_empty() {
        return Err("no token provided".into());
    }

    eprint!("Validating token... ");
    if !gitlab::validate_token(GITLAB_BASE, &token)? {
        return Err("token is invalid".into());
    }
    eprintln!("valid.");

    let cfg = config::Config {
        gitlab: Some(config::GitlabConfig {
            access_token: token,
        }),
    };
    let path = config::config_path()?;
    config::save(&cfg)?;
    eprintln!("Saved to {}", path.display());

    Ok(())
}

const ISSUE_LABEL: &str = "rfe::new-version";

fn file_or_update_issue(
    project_url: &str,
    title: &str,
    description: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = gitlab::Client::from_project_url(project_url)?;
    let issues = client.list_issues(ISSUE_LABEL, "opened")?;

    if let Some(existing) = issues.first() {
        let title_changed = existing.title != title;
        let desc_changed = existing
            .description
            .as_deref()
            != Some(description);
        if title_changed || desc_changed {
            let updates = gitlab::IssueUpdate {
                title: if title_changed {
                    Some(title.to_string())
                } else {
                    None
                },
                description: if desc_changed {
                    Some(description.to_string())
                } else {
                    None
                },
                ..Default::default()
            };
            let updated =
                client.edit_issue(existing.iid, &updates)?;
            eprintln!(
                "Updated issue #{}: {}",
                updated.iid, updated.web_url
            );
        } else {
            eprintln!(
                "Issue #{} already up to date: {}",
                existing.iid, existing.web_url
            );
        }
    } else {
        let issue = client.create_issue(
            title,
            Some(description),
            Some(ISSUE_LABEL),
        )?;
        eprintln!(
            "Created issue #{}: {}",
            issue.iid, issue.web_url
        );
    }

    Ok(())
}
