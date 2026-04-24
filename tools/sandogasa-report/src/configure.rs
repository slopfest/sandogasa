// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively populate user profiles
//! and GitLab tokens in the overlay at
//! `~/.config/sandogasa-report/config.toml`. A profile binds a
//! single logical person to their per-service identities (FAS
//! login, Bugzilla email, per-instance GitLab usernames) so a
//! single `--user <profile>` CLI flag can drive a multi-forge
//! report. The overlay is edited in-place as a `toml::Value` so
//! unknown keys the user added by hand survive round-tripping.

use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use crate::config;

#[derive(clap::Args)]
pub struct ConfigArgs {
    /// Main config file to enumerate domains / GitLab instances
    /// from. Without one, there are no instances to configure.
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
    // Merged view (main + existing overlay) drives prompts so we
    // can show current values as defaults.
    let merged = config::load_config(args.config.as_deref())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Unique GitLab instances across the enabled domains.
    let instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.gitlab.as_ref().map(|g| g.instance.clone()))
        .collect();

    // Load the raw overlay toml::Value so hand-authored keys are
    // preserved across the round trip.
    let overlay_cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let overlay_path = overlay_cf.path().to_path_buf();
    let mut overlay = if overlay_path.exists() {
        let text = std::fs::read_to_string(&overlay_path)?;
        text.parse::<toml::Value>()?
    } else {
        toml::Value::Table(Default::default())
    };

    eprintln!(
        "Configuring sandogasa-report overlay.\n\
         File: {}\n",
        overlay_path.display()
    );

    // Profile section.
    let existing_keys: Vec<&str> = merged.users.keys().map(String::as_str).collect();
    eprintln!(
        "Existing profiles: {}",
        if existing_keys.is_empty() {
            "(none)".to_string()
        } else {
            existing_keys.join(", ")
        }
    );
    let profile_key = read_line("Profile to configure (existing name or new): ")?;
    let profile_key = profile_key.trim().to_string();
    if profile_key.is_empty() {
        eprintln!("No profile name given, aborting.");
        return Ok(());
    }
    let existing_profile = merged.users.get(&profile_key).cloned();
    let mut changed = prompt_profile(
        &mut overlay,
        &profile_key,
        existing_profile.as_ref(),
        &instances,
    )?;

    // Token section: one prompt per unique instance. Existing
    // tokens are validated first so re-runs don't force re-entry.
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

/// Prompt for every field of a user profile, writing into the
/// overlay. Returns `true` if any value was changed.
fn prompt_profile(
    overlay: &mut toml::Value,
    profile_key: &str,
    existing: Option<&config::User>,
    instances: &BTreeSet<String>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut changed = false;
    let fas_default = existing
        .and_then(|p| p.fas.clone())
        .unwrap_or_else(|| profile_key.to_string());
    let fas = prompt_default(&format!("  FAS username [{fas_default}]: "), &fas_default)?;
    if existing.and_then(|p| p.fas.as_deref()) != Some(&fas) {
        overlay_set_str(overlay, &["users", profile_key, "fas"], &fas);
        changed = true;
    }

    let bz_default = existing
        .and_then(|p| p.bugzilla_email.clone())
        .unwrap_or_default();
    let bz = prompt_default(
        &format!("  Bugzilla email (empty to skip) [{bz_default}]: "),
        &bz_default,
    )?;
    if !bz.is_empty() && existing.and_then(|p| p.bugzilla_email.as_deref()) != Some(&bz) {
        overlay_set_str(overlay, &["users", profile_key, "bugzilla_email"], &bz);
        changed = true;
    }

    if !instances.is_empty() {
        eprintln!("\n  GitLab usernames (per instance):");
    }
    for instance in instances {
        let host = crate::gitlab::instance_host(instance);
        let current = existing
            .and_then(|p| p.gitlab.get(&host))
            .cloned()
            .unwrap_or_default();
        let u = prompt_default(
            &format!("    {host} (empty to skip) [{current}]: "),
            &current,
        )?;
        if !u.is_empty()
            && existing
                .and_then(|p| p.gitlab.get(&host))
                .map(String::as_str)
                != Some(&u)
        {
            overlay_set_str(overlay, &["users", profile_key, "gitlab", &host], &u);
            changed = true;
        }
    }
    Ok(changed)
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

/// Read a line, returning `default` if the user pressed Enter
/// without typing anything.
fn prompt_default(prompt: &str, default: &str) -> io::Result<String> {
    let line = read_line(prompt)?;
    let trimmed = line.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
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
            &["users", "michel", "gitlab", "gitlab.com"],
            "michel-slm",
        );
        assert_eq!(
            v["users"]["michel"]["gitlab"]["gitlab.com"].as_str(),
            Some("michel-slm")
        );
    }

    #[test]
    fn overlay_set_preserves_siblings() {
        let mut v: toml::Value = toml::from_str(
            r#"
[users.michel]
fas = "salimma"
"#,
        )
        .unwrap();
        overlay_set_str(
            &mut v,
            &["users", "michel", "bugzilla_email"],
            "m@example.com",
        );
        assert_eq!(v["users"]["michel"]["fas"].as_str(), Some("salimma"));
        assert_eq!(
            v["users"]["michel"]["bugzilla_email"].as_str(),
            Some("m@example.com")
        );
    }

    #[test]
    fn overlay_set_overwrites_existing_value() {
        let mut v: toml::Value = toml::from_str("[users.michel]\nfas = \"old\"\n").unwrap();
        overlay_set_str(&mut v, &["users", "michel", "fas"], "new");
        assert_eq!(v["users"]["michel"]["fas"].as_str(), Some("new"));
    }

    #[test]
    fn overlay_get_str_reads_nested_path() {
        let v: toml::Value = toml::from_str(
            r#"
[users.michel.gitlab]
"gitlab.com" = "michel-slm"
"#,
        )
        .unwrap();
        let got = overlay_get_str(&v, &["users", "michel", "gitlab", "gitlab.com"]);
        assert_eq!(got.as_deref(), Some("michel-slm"));
    }

    #[test]
    fn overlay_get_str_missing_path_is_none() {
        let v: toml::Value = toml::from_str(r#"x = 1"#).unwrap();
        assert!(overlay_get_str(&v, &["foo", "bar"]).is_none());
    }
}
