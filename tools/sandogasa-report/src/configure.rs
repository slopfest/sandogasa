// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively populate the per-user
//! overlay at `~/.config/sandogasa-report/config.toml` with
//! per-domain GitLab username overrides. The overlay file is
//! edited in-place as a `toml::Value`, so unknown keys the user
//! may have added manually are preserved.

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use crate::config;

#[derive(clap::Args)]
pub struct ConfigArgs {
    /// Main config file to enumerate domains from. If omitted,
    /// uses the same lookup logic as `report` (no `-c` → defaults
    /// only, which means no domains to configure).
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<String>,
}

pub fn run(args: &ConfigArgs) -> ExitCode {
    match run_inner(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(args: &ConfigArgs) -> Result<(), Box<dyn std::error::Error>> {
    // Discover GitLab-enabled domains via the merged config view
    // (main + any existing overlay). This lets `config` prompt for
    // whichever domains are visible to `report`.
    let merged = config::load_config(args.config.as_deref())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let gitlab_domains: Vec<(String, String, String)> = merged
        .domains
        .iter()
        .filter_map(|(n, d)| {
            d.gitlab.as_ref().map(|g| {
                (
                    n.clone(),
                    g.instance.clone(),
                    g.user.clone().unwrap_or_default(),
                )
            })
        })
        .collect();

    if gitlab_domains.is_empty() {
        eprintln!(
            "No GitLab-enabled domains found. Pass `-c` pointing at a main \
             config whose domains declare `[domains.<name>.gitlab]`."
        );
        return Ok(());
    }

    // Load existing overlay as a toml::Value so arbitrary keys the
    // user may have authored by hand survive round-tripping.
    let overlay_cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let overlay_path = overlay_cf.path().to_path_buf();
    let mut overlay = if overlay_path.exists() {
        let text = std::fs::read_to_string(&overlay_path)?;
        text.parse::<toml::Value>()?
    } else {
        toml::Value::Table(Default::default())
    };

    eprintln!(
        "Configuring per-domain GitLab usernames.\n\
         Overlay file: {}\n\
         Press Enter at any prompt to leave that value unchanged.\n",
        overlay_path.display()
    );

    let mut changed = false;
    for (name, instance, current) in &gitlab_domains {
        let line = read_line(&format!("  {name} on {instance} [{current}]: "))?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        overlay_set_str(&mut overlay, &["domains", name, "gitlab", "user"], trimmed);
        changed = true;
    }

    // Token round: one prompt per unique instance, validated
    // before saving. Existing tokens are checked first so a
    // re-run doesn't force re-entry unless they've been revoked.
    let mut instances: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (_, instance, _) in &gitlab_domains {
        instances.insert(instance.clone());
    }
    if !instances.is_empty() {
        eprintln!("\nGitLab API tokens (per instance):");
    }
    for instance in &instances {
        let host = crate::gitlab::instance_host(instance);
        let existing = overlay_get_str(&overlay, &["gitlab_tokens", &host]);
        match prompt_gitlab_token(instance, existing.as_deref())? {
            TokenChoice::Saved(t) => {
                overlay_set_str(&mut overlay, &["gitlab_tokens", &host], &t);
                changed = true;
            }
            TokenChoice::KeepExisting | TokenChoice::Skipped => {}
        }
    }

    if !changed {
        eprintln!("\nNo changes.");
        return Ok(());
    }

    overlay_cf.save(&overlay)?;
    eprintln!("\nSaved overlay to {}", overlay_path.display());
    Ok(())
}

enum TokenChoice {
    Saved(String),
    KeepExisting,
    Skipped,
}

/// Interactively collect a GitLab API token for `instance`.
/// Validates existing tokens first (and keeps them if still
/// valid), then prompts for a new one via `rpassword`. An empty
/// response skips the instance — useful when a shell env var is
/// providing the token and the user doesn't want to persist it.
fn prompt_gitlab_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    if let Some(tok) = existing {
        eprint!("  Validating existing {instance} token... ");
        match sandogasa_gitlab::validate_token(instance, tok) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(TokenChoice::KeepExisting);
            }
            Ok(false) => eprintln!("invalid — re-prompting."),
            Err(e) => eprintln!("check failed ({e}); re-prompting."),
        }
    }
    let token = rpassword::prompt_password(format!(
        "  Paste a personal access token for {instance} with 'api' scope \
         (enter to skip): "
    ))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(TokenChoice::Skipped);
    }
    eprint!("  Validating... ");
    match sandogasa_gitlab::validate_token(instance, &token) {
        Ok(true) => {
            eprintln!("valid.");
            Ok(TokenChoice::Saved(token))
        }
        Ok(false) => Err(format!("token rejected by {instance}").into()),
        Err(e) => Err(format!("validation failed for {instance}: {e}").into()),
    }
}

fn read_line(prompt: &str) -> io::Result<String> {
    eprint!("{prompt}");
    io::stderr().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(line)
}

/// Read a string leaf at `path` from a `toml::Value`. Returns
/// `None` if any intermediate key is missing or the leaf isn't a
/// string.
fn overlay_get_str(value: &toml::Value, path: &[&str]) -> Option<String> {
    let mut cur = value;
    for segment in path {
        cur = cur.get(*segment)?;
    }
    cur.as_str().map(|s| s.to_string())
}

/// Deep-set a string value at `path` in a `toml::Value`, creating
/// intermediate tables as needed. Panics if any existing node
/// along the path is non-table — which should never happen for
/// our own overlay shape.
fn overlay_set_str(value: &mut toml::Value, path: &[&str], v: &str) {
    assert!(!path.is_empty(), "path must be non-empty");
    let mut cur = value;
    for segment in &path[..path.len() - 1] {
        let table = cur
            .as_table_mut()
            .expect("overlay structure must be a table");
        if !table.contains_key(*segment) {
            table.insert(
                (*segment).to_string(),
                toml::Value::Table(Default::default()),
            );
        }
        cur = table.get_mut(*segment).unwrap();
    }
    let last = path.last().unwrap();
    cur.as_table_mut()
        .expect("overlay parent must be a table")
        .insert((*last).to_string(), toml::Value::String(v.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_set_creates_nested_tables() {
        let mut v = toml::Value::Table(Default::default());
        overlay_set_str(
            &mut v,
            &["domains", "hyperscale", "gitlab", "user"],
            "michel-slm",
        );
        assert_eq!(
            v["domains"]["hyperscale"]["gitlab"]["user"].as_str(),
            Some("michel-slm")
        );
    }

    #[test]
    fn overlay_set_preserves_siblings() {
        let mut v: toml::Value = toml::from_str(
            r#"
[domains.hyperscale.gitlab]
instance = "https://gitlab.com"
"#,
        )
        .unwrap();
        overlay_set_str(
            &mut v,
            &["domains", "hyperscale", "gitlab", "user"],
            "alice",
        );
        assert_eq!(
            v["domains"]["hyperscale"]["gitlab"]["instance"].as_str(),
            Some("https://gitlab.com")
        );
        assert_eq!(
            v["domains"]["hyperscale"]["gitlab"]["user"].as_str(),
            Some("alice")
        );
    }

    #[test]
    fn overlay_set_overwrites_existing_value() {
        let mut v: toml::Value = toml::from_str(
            r#"[domains.hyperscale.gitlab]
user = "old"
"#,
        )
        .unwrap();
        overlay_set_str(&mut v, &["domains", "hyperscale", "gitlab", "user"], "new");
        assert_eq!(
            v["domains"]["hyperscale"]["gitlab"]["user"].as_str(),
            Some("new")
        );
    }
}
