// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use std::collections::HashSet;

use hs_relmon::cbs;
use hs_relmon::check_latest::{self, Distros, TrackRef};
use hs_relmon::config;
use hs_relmon::gitlab;
use hs_relmon::list_issues;
use hs_relmon::manifest;
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
        #[arg(
            short,
            long,
            long_help = "\
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
  hs10             Hyperscale EL10 only"
        )]
        distros: Option<String>,

        /// Override Repology project name.
        #[arg(
            long,
            value_name = "PROJECT",
            long_help = "\
Override Repology project name when it differs
from the package (e.g. linux for perf)."
        )]
        repology_name: Option<String>,

        /// Reference distribution.
        #[arg(
            long,
            default_value = "upstream",
            long_help = "\
Distribution to compare Hyperscale builds against.

Valid names:
  upstream         Newest version across all repos (default)
  fedora-rawhide   Fedora Rawhide
  fedora-stable    Latest stable Fedora
  centos           Latest CentOS Stream
  centos-stream    Latest CentOS Stream"
        )]
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

    /// Check all packages listed in a manifest file.
    CheckManifest {
        /// Path to the TOML manifest file.
        manifest: PathBuf,

        /// Output as JSON instead of tables.
        #[arg(long)]
        json: bool,

        /// Only show packages whose issue matches this status.
        #[arg(
            long,
            value_name = "STATUS",
            long_help = "\
Only show packages whose GitLab issue matches this
work-item status. Packages without an issue are
excluded. Issues that do not match are not updated.

Default statuses:
  To do          Planned but not started
  In progress    Currently being worked on
  Done           Completed
  Canceled       Will not be done"
        )]
        issue_status: Option<String>,

        /// Only show packages whose issue is assigned to this user.
        #[arg(
            long,
            value_name = "USERNAME",
            long_help = "\
Only show packages whose GitLab issue is assigned
to this username. Use \"none\" to match unassigned
issues. Packages without an issue or without a
matching assignee are excluded."
        )]
        issue_assignee: Option<String>,
    },

    /// List GitLab issues labeled rfe::new-version.
    ListIssues {
        /// GitLab group URL to search.
        #[arg(long, default_value = "https://gitlab.com/CentOS/Hyperscale/rpms")]
        group: String,

        /// Only show issues matching this status.
        #[arg(
            long,
            value_name = "STATUS",
            long_help = "\
Only show issues matching this work-item status.

Default statuses:
  To do          Planned but not started
  In progress    Currently being worked on
  Done           Completed
  Canceled       Will not be done"
        )]
        issue_status: Option<String>,

        /// Only show issues assigned to this user.
        #[arg(
            long,
            value_name = "USERNAME",
            long_help = "\
Only show issues assigned to this username.
Use \"none\" to match unassigned issues."
        )]
        issue_assignee: Option<String>,

        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,

        /// TOML manifest to compare against.
        #[arg(
            long,
            value_name = "PATH",
            long_help = "\
Path to a TOML manifest file. When provided, the
output shows which packages with rfe::new-version
issues are missing from the manifest."
        )]
        manifest: Option<PathBuf>,

        /// Add missing packages to the manifest.
        #[arg(
            long,
            requires = "manifest",
            long_help = "\
Add packages that have rfe::new-version issues but
are missing from the manifest. Requires --manifest.
Packages are inserted in sorted order."
        )]
        add_missing: bool,
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
            let mut result = check_latest::check(
                &repology_client,
                &cbs_client,
                &package,
                repology_name,
                &distros,
                &track,
            )?;

            if let Some(url_override) = &file_issue {
                let project_url = if url_override.is_empty() {
                    default_issue_url(&package)
                } else {
                    url_override.clone()
                };
                if result.is_outdated() {
                    let issue_ref = maybe_file_issue(&package, &result, &project_url)?;
                    result.issue = Some(issue_ref);
                } else {
                    result.issue = lookup_issue(&project_url)?;
                }
                if result.is_in_testing() {
                    let mut errors = 0u32;
                    maybe_set_in_progress(&mut result, &project_url, &package, &mut errors);
                    if errors > 0 {
                        return Err("failed to set issue status".into());
                    }
                }
                if result.is_released() {
                    let mut errors = 0u32;
                    maybe_close_issue(&mut result, &project_url, &package, &mut errors);
                    if errors > 0 {
                        return Err("failed to close issue".into());
                    }
                }
            }

            if json {
                check_latest::print_json(&result)?;
            } else {
                check_latest::print_table(&result);
            }
        }
        Command::CheckManifest {
            manifest,
            json,
            issue_status,
            issue_assignee,
        } => {
            let m = manifest::Manifest::load(&manifest)?;
            let packages = m.resolve()?;
            let filtering = issue_status.is_some() || issue_assignee.is_some();

            let repology_client = repology::Client::new();
            let cbs_client = cbs::Client::new();
            let mut results = Vec::new();
            let mut errors = 0u32;

            for pkg in &packages {
                let repology_name = pkg.repology_name.as_deref().unwrap_or(&pkg.name);

                let result = check_latest::check(
                    &repology_client,
                    &cbs_client,
                    &pkg.name,
                    repology_name,
                    &pkg.distros,
                    &pkg.track,
                );
                let mut result = match result {
                    Ok(r) => r,
                    Err(e) => {
                        eprintln!("{}: {e}", pkg.name);
                        errors += 1;
                        continue;
                    }
                };

                if pkg.file_issue {
                    let project_url = pkg
                        .issue_url
                        .as_deref()
                        .map(String::from)
                        .unwrap_or_else(|| default_issue_url(&pkg.name));
                    if result.is_outdated() && !filtering {
                        match maybe_file_issue(&pkg.name, &result, &project_url) {
                            Ok(issue_ref) => {
                                result.issue = Some(issue_ref);
                            }
                            Err(e) => {
                                eprintln!("{}: filing issue: {e}", pkg.name);
                                errors += 1;
                            }
                        }
                    } else {
                        // Look up existing issue first so
                        // we can check filters before
                        // deciding whether to file/update.
                        match lookup_issue(&project_url) {
                            Ok(found) => {
                                result.issue = found;
                            }
                            Err(e) => {
                                eprintln!(
                                    "{}: looking up issue: \
                                    {e}",
                                    pkg.name
                                );
                                errors += 1;
                            }
                        }
                        if result.is_outdated()
                            && result.matches_issue_filter(
                                issue_status.as_deref(),
                                issue_assignee.as_deref(),
                            )
                        {
                            match maybe_file_issue(&pkg.name, &result, &project_url) {
                                Ok(issue_ref) => {
                                    result.issue = Some(issue_ref);
                                }
                                Err(e) => {
                                    eprintln!(
                                        "{}: filing issue: \
                                        {e}",
                                        pkg.name
                                    );
                                    errors += 1;
                                }
                            }
                        }
                    }
                    // When the build is in testing,
                    // transition the issue to
                    // "In progress" so it is not
                    // mistaken for un-started work.
                    if result.is_in_testing() {
                        maybe_set_in_progress(&mut result, &project_url, &pkg.name, &mut errors);
                    }
                    // When every distro has an
                    // up-to-date release build, close
                    // the issue with a comment.
                    if result.is_released() {
                        maybe_close_issue(&mut result, &project_url, &pkg.name, &mut errors);
                    }
                }

                results.push(result);
            }

            let results: Vec<_> = if filtering {
                results
                    .into_iter()
                    .filter(|r| {
                        r.matches_issue_filter(issue_status.as_deref(), issue_assignee.as_deref())
                    })
                    .collect()
            } else {
                results
            };

            if json {
                check_latest::print_json_array(&results)?;
            } else {
                for (i, r) in results.iter().enumerate() {
                    if i > 0 {
                        println!();
                    }
                    check_latest::print_table(r);
                }
            }

            if errors > 0 {
                return Err(format!("{errors} package(s) had errors").into());
            }
        }
        Command::ListIssues {
            group,
            issue_status,
            issue_assignee,
            json,
            manifest,
            add_missing,
        } => {
            let client = gitlab::GroupClient::from_group_url(&group)?;
            let issues = client.list_issues(ISSUE_LABEL, None)?;

            let manifest_names = match &manifest {
                Some(path) => {
                    let m = manifest::Manifest::load(path)?;
                    let names: HashSet<String> =
                        m.packages.iter().map(|p| p.name.clone()).collect();
                    Some(names)
                }
                None => None,
            };

            let entries = list_issues::build_entries(
                &client,
                &issues,
                issue_status.as_deref(),
                issue_assignee.as_deref(),
                manifest_names.as_ref(),
            );

            if json {
                list_issues::print_json(&entries)?;
            } else {
                list_issues::print_table(&entries);
            }

            if add_missing {
                let path = manifest.as_ref().unwrap();
                let missing: Vec<String> = entries
                    .iter()
                    .filter(|e| e.in_manifest == Some(false))
                    .map(|e| e.package.clone())
                    .collect();
                if missing.is_empty() {
                    eprintln!("No missing packages to add.");
                } else {
                    manifest::add_packages_to_file(path, &missing)?;
                    eprintln!("Added {} package(s) to {}", missing.len(), path.display());
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

fn default_issue_url(package: &str) -> String {
    format!(
        "https://gitlab.com/CentOS/\
        Hyperscale/rpms/{package}"
    )
}

const IN_PROGRESS: &str = "In progress";

/// Transition an existing issue to "In progress" if it is not already.
///
/// When there is no issue yet this is a no-op.
fn maybe_set_in_progress(
    result: &mut check_latest::CheckResult,
    project_url: &str,
    package: &str,
    errors: &mut u32,
) {
    let issue = match &result.issue {
        Some(i) => i,
        None => return,
    };
    if issue.status == IN_PROGRESS {
        return;
    }
    let client = match gitlab::Client::from_project_url(project_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{package}: setting status: {e}");
            *errors += 1;
            return;
        }
    };
    match client.set_work_item_status(issue.iid, IN_PROGRESS) {
        Ok(()) => {
            eprintln!("Set issue #{} to {IN_PROGRESS}: {}", issue.iid, issue.url);
            if let Some(ref mut issue) = result.issue {
                issue.status = IN_PROGRESS.to_string();
            }
        }
        Err(e) => {
            eprintln!("{package}: setting status: {e}");
            *errors += 1;
        }
    }
}

/// Close an open issue with a comment linking to the
/// CBS release build(s).
///
/// No-op when there is no issue or the issue is already
/// closed.
fn maybe_close_issue(
    result: &mut check_latest::CheckResult,
    project_url: &str,
    package: &str,
    errors: &mut u32,
) {
    let issue = match &result.issue {
        Some(i) => i,
        None => return,
    };
    if issue.state != "opened" {
        return;
    }
    let client = match gitlab::Client::from_project_url(project_url) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{package}: closing issue: {e}");
            *errors += 1;
            return;
        }
    };
    let comment = result.close_comment();
    if let Err(e) = client.add_note(issue.iid, &comment) {
        eprintln!("{package}: adding comment: {e}");
        *errors += 1;
        return;
    }
    let updates = gitlab::IssueUpdate {
        state_event: Some("close".to_string()),
        ..Default::default()
    };
    match client.edit_issue(issue.iid, &updates) {
        Ok(closed) => {
            eprintln!("Closed issue #{}: {}", closed.iid, closed.web_url);
            if let Some(ref mut issue) = result.issue {
                issue.status = "closed".to_string();
            }
        }
        Err(e) => {
            eprintln!("{package}: closing issue: {e}");
            *errors += 1;
        }
    }
}

fn maybe_file_issue(
    package: &str,
    result: &check_latest::CheckResult,
    project_url: &str,
) -> Result<check_latest::IssueRef, Box<dyn std::error::Error>> {
    let ref_ver = result
        .ref_version()
        .ok_or("no reference version available")?;
    let title = format!("{package}-{ref_ver} is available");
    let description = format!("```\n{}```", check_latest::format_table(result));
    file_or_update_issue(project_url, &title, &description)
}

const ISSUE_LABEL: &str = "rfe::new-version";

fn resolve_issue_ref(
    client: &gitlab::Client,
    issue: &gitlab::Issue,
) -> Result<check_latest::IssueRef, Box<dyn std::error::Error>> {
    let status = client.get_work_item_status(issue.iid)?;
    Ok(check_latest::IssueRef::from_gitlab_issue(issue, status))
}

fn lookup_issue(
    project_url: &str,
) -> Result<Option<check_latest::IssueRef>, Box<dyn std::error::Error>> {
    let client = gitlab::Client::from_project_url(project_url)?;
    let issues = client.list_issues(ISSUE_LABEL, None)?;
    match issues.first() {
        Some(issue) => Ok(Some(resolve_issue_ref(&client, issue)?)),
        None => Ok(None),
    }
}

fn file_or_update_issue(
    project_url: &str,
    title: &str,
    description: &str,
) -> Result<check_latest::IssueRef, Box<dyn std::error::Error>> {
    let client = gitlab::Client::from_project_url(project_url)?;

    // Check for an existing open issue first.
    let open_issues = client.list_issues(ISSUE_LABEL, Some("opened"))?;
    if let Some(existing) = open_issues.first() {
        return update_existing_issue(&client, existing, title, description);
    }

    // Check for a closed issue with the same title that we can reopen.
    let closed_issues = client.list_issues(ISSUE_LABEL, Some("closed"))?;
    if let Some(existing) = closed_issues.iter().find(|i| i.title == title) {
        return reopen_issue(&client, existing, title, description);
    }

    // No existing issue — create a new one.
    let issue = client.create_issue(title, Some(description), Some(ISSUE_LABEL))?;
    eprintln!("Created issue #{}: {}", issue.iid, issue.web_url);
    resolve_issue_ref(&client, &issue)
}

fn update_existing_issue(
    client: &gitlab::Client,
    existing: &gitlab::Issue,
    title: &str,
    description: &str,
) -> Result<check_latest::IssueRef, Box<dyn std::error::Error>> {
    let title_changed = existing.title != title;
    let desc_changed = existing.description.as_deref() != Some(description);
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
        let updated = client.edit_issue(existing.iid, &updates)?;
        eprintln!("Updated issue #{}: {}", updated.iid, updated.web_url);
        resolve_issue_ref(client, &updated)
    } else {
        eprintln!(
            "Issue #{} already up to date: {}",
            existing.iid, existing.web_url
        );
        resolve_issue_ref(client, existing)
    }
}

fn reopen_issue(
    client: &gitlab::Client,
    existing: &gitlab::Issue,
    title: &str,
    description: &str,
) -> Result<check_latest::IssueRef, Box<dyn std::error::Error>> {
    let updates = gitlab::IssueUpdate {
        title: Some(title.to_string()),
        description: Some(description.to_string()),
        add_labels: Some("reopened".to_string()),
        state_event: Some("reopen".to_string()),
        ..Default::default()
    };
    let updated = client.edit_issue(existing.iid, &updates)?;
    eprintln!("Reopened issue #{}: {}", updated.iid, updated.web_url);
    resolve_issue_ref(client, &updated)
}
