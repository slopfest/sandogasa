// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively populate user profiles
//! and forge tokens (GitLab + GitHub) in the overlay at
//! `~/.config/sandogasa-report/config.toml`. A profile binds a
//! single logical person to their per-service identities (FAS
//! login, Bugzilla email, per-instance GitLab/GitHub usernames)
//! so a single `--user <profile>` CLI flag can drive a
//! multi-forge report. The overlay is edited in-place as a
//! `toml::Value` so unknown keys the user added by hand survive
//! round-tripping.

use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use crate::config;

#[derive(clap::Args)]
pub struct ConfigArgs {
    /// Main config file to enumerate domains / forge instances
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

    // Unique forge instances across the enabled domains, kept
    // separate so we can prompt for the right credential type
    // (GitLab vs GitHub) per instance.
    let gitlab_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.gitlab.as_ref().map(|g| g.instance.clone()))
        .collect();
    let github_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.github.as_ref().map(|g| g.instance.clone()))
        .collect();
    let forgejo_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.forgejo.as_ref().map(|f| f.instance.clone()))
        .collect();
    let sourcehut_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.sourcehut.as_ref().map(|s| s.instance.clone()))
        .collect();

    // Load the raw overlay toml::Value so hand-authored keys are
    // preserved across the round trip.
    let overlay_cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
    let overlay_path = overlay_cf.path().to_path_buf();
    let mut overlay = if overlay_path.exists() {
        let text = std::fs::read_to_string(&overlay_path)?;
        parse_overlay(&text)?
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
        &gitlab_instances,
        &github_instances,
        &forgejo_instances,
        &sourcehut_instances,
    )?;

    // Token section: one prompt per unique instance, per forge.
    // Existing tokens are validated first so re-runs don't force
    // re-entry unless they've been revoked.
    if !gitlab_instances.is_empty() {
        eprintln!("\nGitLab API tokens (per instance):");
    }
    for instance in &gitlab_instances {
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
    if !github_instances.is_empty() {
        eprintln!("\nGitHub API tokens (per instance):");
    }
    for instance in &github_instances {
        let host = crate::github::instance_host(instance);
        let existing = overlay_get_str(&overlay, &["github_tokens", &host]);
        match prompt_github_token(instance, existing.as_deref())? {
            TokenChoice::Saved(t) => {
                overlay_set_str(&mut overlay, &["github_tokens", &host], &t);
                changed = true;
            }
            TokenChoice::KeepExisting | TokenChoice::Skipped => {}
        }
    }
    if !forgejo_instances.is_empty() {
        eprintln!("\nForgejo API tokens (per instance):");
    }
    for instance in &forgejo_instances {
        let host = crate::forgejo::instance_host(instance);
        let existing = overlay_get_str(&overlay, &["forgejo_tokens", &host]);
        match prompt_forgejo_token(instance, existing.as_deref())? {
            TokenChoice::Saved(t) => {
                overlay_set_str(&mut overlay, &["forgejo_tokens", &host], &t);
                changed = true;
            }
            TokenChoice::KeepExisting | TokenChoice::Skipped => {}
        }
    }
    if !sourcehut_instances.is_empty() {
        eprintln!("\nSourcehut personal access tokens (per instance):");
    }
    for instance in &sourcehut_instances {
        let host = crate::sourcehut::instance_host(instance);
        let existing = overlay_get_str(&overlay, &["sourcehut_tokens", &host]);
        match prompt_sourcehut_token(instance, existing.as_deref())? {
            TokenChoice::Saved(t) => {
                overlay_set_str(&mut overlay, &["sourcehut_tokens", &host], &t);
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
    gitlab_instances: &BTreeSet<String>,
    github_instances: &BTreeSet<String>,
    forgejo_instances: &BTreeSet<String>,
    sourcehut_instances: &BTreeSet<String>,
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

    // Git author emails for Sourcehut commit attribution — only relevant
    // when a domain enables Sourcehut.
    if !sourcehut_instances.is_empty() {
        let existing_emails = existing.map(|p| p.git_emails.clone()).unwrap_or_default();
        let emails_default = existing_emails.join(", ");
        let answer = prompt_default(
            &format!(
                "  Git emails for Sourcehut commit attribution \
                 (comma-separated, * for all) [{emails_default}]: "
            ),
            &emails_default,
        )?;
        let emails: Vec<String> = answer
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if emails != existing_emails {
            overlay_set_array(overlay, &["users", profile_key, "git_emails"], &emails);
            changed = true;
        }
    }

    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        gitlab_instances,
        "GitLab",
        "gitlab",
        |u, host| u.gitlab.get(host),
        crate::gitlab::instance_host,
    )? {
        changed = true;
    }
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        github_instances,
        "GitHub",
        "github",
        |u, host| u.github.get(host),
        crate::github::instance_host,
    )? {
        changed = true;
    }
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        forgejo_instances,
        "Forgejo",
        "forgejo",
        |u, host| u.forgejo.get(host),
        crate::forgejo::instance_host,
    )? {
        changed = true;
    }
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        sourcehut_instances,
        "Sourcehut",
        "sourcehut",
        |u, host| u.sourcehut.get(host),
        crate::sourcehut::instance_host,
    )? {
        changed = true;
    }
    Ok(changed)
}

/// Prompt for per-instance usernames on a single forge, writing
/// each non-empty answer into the overlay under
/// `[users.<profile>.<forge_key>]`. Factored out because the
/// gitlab and github passes are identical except for the
/// label, the overlay key, the profile-lookup function, and
/// the host-derivation function.
#[allow(clippy::too_many_arguments)]
fn prompt_per_instance_usernames(
    overlay: &mut toml::Value,
    profile_key: &str,
    existing: Option<&config::User>,
    instances: &BTreeSet<String>,
    forge_label: &str,
    forge_key: &str,
    lookup: impl for<'a> Fn(&'a config::User, &str) -> Option<&'a String>,
    host_of: impl Fn(&str) -> String,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut changed = false;
    if !instances.is_empty() {
        eprintln!("\n  {forge_label} usernames (per instance):");
    }
    for instance in instances {
        let host = host_of(instance);
        let current: String = existing
            .and_then(|p| lookup(p, &host))
            .cloned()
            .unwrap_or_default();
        let u = prompt_default(
            &format!("    {host} (empty to skip) [{current}]: "),
            &current,
        )?;
        if !u.is_empty() && existing.and_then(|p| lookup(p, &host)).map(String::as_str) != Some(&u)
        {
            overlay_set_str(overlay, &["users", profile_key, forge_key, &host], &u);
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

/// Same flow as `prompt_gitlab_token` but for GitHub. Uses
/// `sandogasa-github`'s three-state `validate_token`: an
/// `Ok(false)` means the saved token is actually invalid (re-
/// prompt), while `Err` means we couldn't reach the API (warn
/// and keep the existing token so the user can retry).
fn prompt_github_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    if let Some(tok) = existing {
        eprint!("  Validating existing {instance} token... ");
        match sandogasa_github::validate_token(instance, tok) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(TokenChoice::KeepExisting);
            }
            Ok(false) => eprintln!("invalid — re-prompting."),
            Err(e) => {
                eprintln!("check failed ({e}); keeping existing token.");
                return Ok(TokenChoice::KeepExisting);
            }
        }
    }
    let token = rpassword::prompt_password(format!(
        "  Paste a personal access token for {instance} with 'repo' scope \
         (enter to skip): "
    ))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(TokenChoice::Skipped);
    }
    eprint!("  Validating... ");
    match sandogasa_github::validate_token(instance, &token) {
        Ok(true) => {
            eprintln!("valid.");
            Ok(TokenChoice::Saved(token))
        }
        Ok(false) => Err(format!("token rejected by {instance}").into()),
        Err(e) => Err(format!("validation failed for {instance}: {e}").into()),
    }
}

/// Same flow as `prompt_github_token` but for Forgejo / Gitea.
/// Treats an unreachable API (`Err`) as "keep the existing token"
/// so a transient outage doesn't force re-entry.
fn prompt_forgejo_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    if let Some(tok) = existing {
        eprint!("  Validating existing {instance} token... ");
        match sandogasa_forgejo::validate_token(instance, tok) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(TokenChoice::KeepExisting);
            }
            Ok(false) => eprintln!("invalid — re-prompting."),
            Err(e) => {
                eprintln!("check failed ({e}); keeping existing token.");
                return Ok(TokenChoice::KeepExisting);
            }
        }
    }
    let token = rpassword::prompt_password(format!(
        "  Paste a personal access token for {instance} \
         (enter to skip): "
    ))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(TokenChoice::Skipped);
    }
    eprint!("  Validating... ");
    match sandogasa_forgejo::validate_token(instance, &token) {
        Ok(true) => {
            eprintln!("valid.");
            Ok(TokenChoice::Saved(token))
        }
        Ok(false) => Err(format!("token rejected by {instance}").into()),
        Err(e) => Err(format!("validation failed for {instance}: {e}").into()),
    }
}

/// Same flow as `prompt_forgejo_token` but for Sourcehut. The token is a
/// personal access token from meta.sr.ht/oauth2/personal-token (which
/// grants read access across services by default).
fn prompt_sourcehut_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    if let Some(tok) = existing {
        eprint!("  Validating existing {instance} token... ");
        match sandogasa_sourcehut::validate_token(instance, tok) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(TokenChoice::KeepExisting);
            }
            Ok(false) => eprintln!("invalid — re-prompting."),
            Err(e) => {
                eprintln!("check failed ({e}); keeping existing token.");
                return Ok(TokenChoice::KeepExisting);
            }
        }
    }
    let token = rpassword::prompt_password(format!(
        "  Paste a personal access token for {instance} \
         (from meta.sr.ht/oauth2/personal-token; enter to skip): "
    ))?;
    let token = token.trim().to_string();
    if token.is_empty() {
        return Ok(TokenChoice::Skipped);
    }
    eprint!("  Validating... ");
    match sandogasa_sourcehut::validate_token(instance, &token) {
        Ok(true) => {
            eprintln!("valid.");
            Ok(TokenChoice::Saved(token))
        }
        Ok(false) => Err(format!("token rejected by {instance}").into()),
        Err(e) => Err(format!("validation failed for {instance}: {e}").into()),
    }
}

/// Parse an overlay file's text as a TOML *document*. Deliberately uses
/// `toml::from_str`, not `str::parse::<toml::Value>()`: in toml 1.x the
/// `FromStr` impl parses a value *expression*, so a real config that
/// opens with a `[table]` header is misread as an array literal and
/// fails right after the `]`. `from_str` parses it as a document, the
/// same way the report loader does.
fn parse_overlay(text: &str) -> Result<toml::Value, Box<dyn std::error::Error>> {
    Ok(toml::from_str::<toml::Value>(text)?)
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
/// Set a path to an array of strings, creating intermediate tables like
/// [`overlay_set_str`].
fn overlay_set_array(value: &mut toml::Value, path: &[&str], items: &[String]) {
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
    let arr = items
        .iter()
        .map(|s| toml::Value::String(s.clone()))
        .collect();
    cur.as_table_mut()
        .expect("overlay parent must be a table")
        .insert((*last).to_string(), toml::Value::Array(arr));
}

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
    fn parse_overlay_reads_document_with_leading_table_header() {
        // Regression: a real overlay opens with a `[table]` header.
        // `str::parse::<toml::Value>()` misreads that as an array in
        // toml 1.x; `parse_overlay` must treat it as a document.
        let text = "[github_tokens]\n\"api.github.com\" = \"ghp\"\n";
        let v = parse_overlay(text).unwrap();
        assert_eq!(v["github_tokens"]["api.github.com"].as_str(), Some("ghp"));
    }

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
