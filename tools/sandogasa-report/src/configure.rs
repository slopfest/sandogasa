// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `config` subcommand — interactively populate user profiles
//! in the overlay at `~/.config/sandogasa-report/config.toml`,
//! and forge tokens (GitLab, GitHub, Forgejo/Gitea, Sourcehut) in the
//! desktop keyring, one entry per instance (the overlay when no
//! keyring can be used). A profile binds a
//! single logical person to their per-service identities (FAS
//! login, Bugzilla email, per-instance GitLab/GitHub usernames)
//! so a single `--user <profile>` CLI flag can drive a
//! multi-forge report. The overlay is edited in-place as a
//! `toml::Value` so unknown keys the user added by hand survive
//! round-tripping.

use std::collections::BTreeSet;
use std::process::ExitCode;

use sandogasa_config::{Setting, ask_with};

use crate::config;
use crate::forge;

#[derive(clap::Args)]
pub struct ConfigArgs {
    /// Also go over the instances whose token is turned off ("-"),
    /// offering the keyring's again where it has one.
    #[arg(long)]
    pub all: bool,
    /// Main config file to enumerate domains / forge instances
    /// from. Without one, there are no instances to configure.
    #[arg(short, long, value_name = "PATH")]
    pub config: Option<String>,
    /// Ask only for what is not set yet: a field with a value, and an
    /// instance with a token, are kept as they are, unchecked.
    #[arg(long)]
    pub missing: bool,
}

/// Where a prompt's answer is read from: the terminal, or a test's
/// answers ([`sandogasa_config::ask_with`]'s `read`).
type Reader<'a> = dyn FnMut(&str, bool) -> std::io::Result<String> + 'a;

/// Asks for each setting, or, with `--missing`, only for those not set
/// yet, counting the ones it kept.
struct Asker<'r, 'a> {
    read: &'r mut Reader<'a>,
    missing: bool,
    kept: usize,
}

impl Asker<'_, '_> {
    /// The answer for `setting`, defaulting to `current`; with
    /// `--missing`, `current` itself when `have` (a value is set).
    fn field(
        &mut self,
        setting: &Setting<'_>,
        current: &str,
        show: bool,
        have: bool,
    ) -> Result<String, Box<dyn std::error::Error>> {
        if self.missing && have {
            self.kept += 1;
            return Ok(current.to_string());
        }
        ask_with(setting, current, show, self.read)
    }
}

pub fn run(args: &ConfigArgs) -> ExitCode {
    let mut terminal = sandogasa_config::read_line_from_terminal;
    match run_inner(args, &mut terminal) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `config`, reading each answer through `read`.
fn run_inner(args: &ConfigArgs, read: &mut Reader<'_>) -> Result<(), Box<dyn std::error::Error>> {
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
    let pagure_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.pagure.as_ref().map(|p| p.instance.clone()))
        .collect();
    let bts = merged.domains.values().any(|d| d.bts);
    // Bugzilla instances other than Red Hat's, whose email FASJSON
    // cannot supply.
    let bugzilla_instances: BTreeSet<String> = merged
        .domains
        .values()
        .filter_map(|d| d.bugzilla())
        .filter(|b| !b.is_redhat())
        .map(|b| b.instance)
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
    // As read, to tell which settings the answers clear.
    let as_read = overlay.clone();

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
    // With --missing and one profile, that one: it is what there is to
    // complete.
    let only = (args.missing && existing_keys.len() == 1).then(|| existing_keys[0].to_string());
    let profile_key = match only {
        Some(key) => {
            eprintln!("Completing profile {key}.");
            key
        }
        None => ask_with(
            &Setting::new("profile", "to configure (an existing name or a new one)").optional(),
            "",
            false,
            read,
        )?,
    };
    let mut ask = Asker {
        read,
        missing: args.missing,
        kept: 0,
    };
    if profile_key.is_empty() {
        eprintln!("No profile name given, aborting.");
        return Ok(());
    }
    let existing_profile = merged.users.get(&profile_key).cloned();
    let mut changed = prompt_profile(
        &mut overlay,
        &profile_key,
        existing_profile.as_ref(),
        bts,
        &gitlab_instances,
        &github_instances,
        &forgejo_instances,
        &sourcehut_instances,
        &pagure_instances,
        &bugzilla_instances,
        &mut ask,
    )?;

    // Where `note` writes notes, and `notes` and `report --notes` read
    // them (#167).
    // With --missing, a heading only over something asked.
    if !(ask.missing && notes_dir_set(&overlay)) {
        eprintln!("\nNotes:");
    }
    let notes_dir = ask.field(
        &Setting::new("notes", "directory")
            .optional()
            .hint(
                "Where `note` writes notes: an absolute path or one under ~/; \
                 empty for ~/.local/share/sandogasa-report/notes",
            )
            .validated(&crate::notes::check_dir),
        merged.notes.dir.as_deref().unwrap_or(""),
        true,
        // Set in the user's own file: one from /etc is not theirs.
        notes_dir_set(&overlay),
    )?;
    let notes_dir_changed = overlay_update(
        &mut overlay,
        &["notes", "dir"],
        merged.notes.dir.as_deref(),
        &notes_dir,
    );
    changed |= notes_dir_changed;
    // The profile is saved before the tokens are asked for, so a token
    // that cannot be stored does not lose the answers above.
    let mut saved_profile = false;
    if changed {
        if write_overlay(&overlay_cf, &as_read, &overlay, &[])? {
            eprintln!(
                "\nNothing left to keep in {}; removed it.",
                overlay_path.display()
            );
        } else {
            eprintln!("\nSaved the profile to {}", overlay_path.display());
        }
        saved_profile = true;
        changed = false;
    }

    // Token section: one prompt per unique instance, per forge.
    // Existing tokens are validated first so re-runs don't force
    // re-entry unless they've been revoked. Each goes in the keyring
    // when there is one, as its own entry (#79).
    let forges = [
        (
            &gitlab_instances,
            "GitLab API tokens (per instance):",
            GITLAB,
        ),
        (
            &github_instances,
            "GitHub API tokens (per instance):",
            GITHUB,
        ),
        (
            &forgejo_instances,
            "Forgejo/Gitea API tokens (per instance):",
            FORGEJO,
        ),
        (
            &sourcehut_instances,
            "Sourcehut personal access tokens (per instance):",
            SOURCEHUT,
        ),
    ];
    let mut stored = false;
    let mut out_of_file: Vec<(&str, String)> = Vec::new();

    for (instances, heading, kind) in forges {
        // With --missing, the heading waits for a token to ask for.
        let mut headed = instances.is_empty() || ask.missing;
        if !headed {
            eprintln!("\n{heading}");
            headed = true;
        }
        for instance in instances {
            let host = (kind.host)(instance);
            let outcome = instance_token(
                &mut overlay,
                &kind,
                instance,
                &host,
                args.all,
                &sandogasa_cli::ask_to_reenable,
                |existing| match existing {
                    // A stored token is kept, unchecked, with --missing.
                    Some(_) if ask.missing => {
                        ask.kept += 1;
                        Ok(TokenChoice::KeepExisting)
                    }
                    _ => {
                        if ask.missing && !std::mem::replace(&mut headed, true) {
                            eprintln!("\n{heading}");
                        }
                        (kind.prompt)(instance, existing)
                    }
                },
            )?;
            match outcome {
                TokenOutcome::Unchanged => {}
                TokenOutcome::InOverlay => changed = true,
                TokenOutcome::InKeyring { was_in_file } => {
                    stored = true;
                    if was_in_file {
                        out_of_file.push((kind.table, host));
                        changed = true;
                    }
                }
            }
        }
    }

    if ask.missing {
        eprintln!("\nKept {} setting(s) already set.", ask.kept);
    }
    if !changed {
        if !stored && !saved_profile {
            eprintln!("\nNo changes.");
        }
        return Ok(());
    }

    if write_overlay(&overlay_cf, &as_read, &overlay, &out_of_file)? {
        eprintln!(
            "\nNothing else to keep in {}; removed it.",
            overlay_path.display()
        );
    } else {
        eprintln!("\nSaved overlay to {}", overlay_path.display());
    }
    Ok(())
}

/// Whether the user's own file sets the notes directory.
fn notes_dir_set(overlay: &toml::Value) -> bool {
    overlay_get_str(overlay, &["notes", "dir"]).is_some()
}

/// Prompt for every field of a user profile, writing into the
/// overlay. Returns `true` if any value was changed.
#[allow(clippy::too_many_arguments)]
fn prompt_profile(
    overlay: &mut toml::Value,
    profile_key: &str,
    existing: Option<&config::User>,
    bts: bool,
    gitlab_instances: &BTreeSet<String>,
    github_instances: &BTreeSet<String>,
    forgejo_instances: &BTreeSet<String>,
    sourcehut_instances: &BTreeSet<String>,
    pagure_instances: &BTreeSet<String>,
    bugzilla_instances: &BTreeSet<String>,
    ask: &mut Asker<'_, '_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut changed = false;
    let fas_default = existing
        .and_then(|p| p.fas.clone())
        .unwrap_or_else(|| profile_key.to_string());
    let fas = ask.field(
        &Setting::new("FAS", "username"),
        &fas_default,
        true,
        existing.and_then(|p| p.fas.as_ref()).is_some(),
    )?;
    if existing.and_then(|p| p.fas.as_deref()) != Some(&fas) {
        overlay_set_str(overlay, &["users", profile_key, "fas"], &fas);
        changed = true;
    }

    let bz_default = existing
        .and_then(|p| p.bugzilla_email.clone())
        .unwrap_or_default();
    let bz = ask.field(
        &Setting::new("Bugzilla", "email")
            .optional()
            .validated(&sandogasa_config::validate_email),
        &bz_default,
        true,
        !bz_default.is_empty(),
    )?;
    changed |= overlay_update(
        overlay,
        &["users", profile_key, "bugzilla_email"],
        existing.and_then(|p| p.bugzilla_email.as_deref()),
        &bz,
    );
    changed |= prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        bugzilla_instances,
        ("Bugzilla", "email"),
        "bugzilla_emails",
        |p, host| p.bugzilla_emails.get(host),
        forge::instance_host,
        ask,
    )?;

    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        gitlab_instances,
        ("GitLab", "username"),
        "gitlab",
        |u, host| u.gitlab.get(host),
        crate::gitlab::instance_host,
        ask,
    )? {
        changed = true;
    }
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        github_instances,
        ("GitHub", "username"),
        "github",
        |u, host| u.github.get(host),
        crate::github::instance_host,
        ask,
    )? {
        changed = true;
    }
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        forgejo_instances,
        ("Forgejo/Gitea", "username"),
        "forgejo",
        |u, host| u.forgejo.get(host),
        crate::forgejo::instance_host,
        ask,
    )? {
        changed = true;
    }
    // Pagure logins, empty for the FAS name (the login on Fedora's
    // dist-git).
    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        pagure_instances,
        ("Pagure", "username"),
        "pagure",
        |u, host| u.pagure.get(host),
        crate::pagure::instance_host,
        ask,
    )? {
        changed = true;
    }

    if prompt_per_instance_usernames(
        overlay,
        profile_key,
        existing,
        sourcehut_instances,
        ("Sourcehut", "username"),
        "sourcehut",
        |u, host| u.sourcehut.get(host),
        crate::sourcehut::instance_host,
        ask,
    )? {
        changed = true;
    }

    // Git author emails for Sourcehut commit attribution, asked after
    // the Sourcehut usernames they qualify; only when a domain enables
    // Sourcehut.
    if !sourcehut_instances.is_empty() {
        let existing_emails = existing.map(|p| p.git_emails.clone()).unwrap_or_default();
        let emails_default = existing_emails.join(", ");
        let answer = ask.field(
            &Setting::new(
                "Sourcehut",
                "commit attribution git emails (comma-separated, * for all)",
            )
            .optional(),
            &emails_default,
            true,
            !emails_default.is_empty(),
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

    // The addresses the Debian bug tracker knows the person by; only
    // when a domain reads it.
    if bts {
        changed |= prompt_email_list(
            overlay,
            profile_key,
            "debian_emails",
            existing
                .map(|p| p.debian_emails.clone())
                .unwrap_or_default(),
            (
                "Debian",
                "emails, as in changelogs and bug reports (comma-separated)",
            ),
            ask,
        )?;
    }
    Ok(changed)
}

/// Ask for a comma-separated list of email addresses, each checked as
/// one, and record it under `[users.<profile>] <key>`. Whether it
/// changed.
fn prompt_email_list(
    overlay: &mut toml::Value,
    profile_key: &str,
    key: &str,
    existing: Vec<String>,
    (service, what): (&str, &str),
    ask: &mut Asker<'_, '_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let check = |answer: &str| -> Result<(), String> {
        answer
            .split(',')
            .map(str::trim)
            .filter(|e| !e.is_empty())
            .try_for_each(sandogasa_config::validate_email)
    };
    let current = existing.join(", ");
    // An empty list in the file is an answer too: none to give.
    let recorded = overlay
        .get("users")
        .and_then(|u| u.get(profile_key))
        .and_then(|p| p.get(key))
        .is_some();
    let answer = ask.field(
        &Setting::new(service, what).optional().validated(&check),
        &current,
        true,
        recorded || !existing.is_empty(),
    )?;
    let emails: Vec<String> = answer
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if emails == existing && (recorded || !emails.is_empty()) {
        return Ok(false);
    }
    overlay_set_array(overlay, &["users", profile_key, key], &emails);
    Ok(true)
}

/// Prompt for what a profile is on each instance of one service (a
/// forge's username, another Bugzilla's email): "GitLab username on
/// gitlab.com", "Bugzilla email on bugzilla.opensuse.org". Writes each
/// non-empty answer into the overlay under
/// `[users.<profile>.<forge_key>]`; an email is checked as one.
#[allow(clippy::too_many_arguments)]
fn prompt_per_instance_usernames(
    overlay: &mut toml::Value,
    profile_key: &str,
    existing: Option<&config::User>,
    instances: &BTreeSet<String>,
    (service, what): (&str, &str),
    forge_key: &str,
    lookup: impl for<'a> Fn(&'a config::User, &str) -> Option<&'a String>,
    host_of: impl Fn(&str) -> String,
    ask: &mut Asker<'_, '_>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut changed = false;
    for instance in instances {
        let host = host_of(instance);
        let current: String = existing
            .and_then(|p| lookup(p, &host))
            .cloned()
            .unwrap_or_default();
        // An email is checked as one; a username is taken as given.
        // GitHub's instance is its API host; people know it by its web
        // host, which the overlay does not key on.
        let shown = match host.strip_prefix("api.") {
            Some(web) if service == "GitHub" => web,
            _ => host.as_str(),
        };
        let label = format!("{what} on {shown}");
        let setting = Setting::new(service, &label).optional();
        let setting = if what == "email" {
            setting.validated(&sandogasa_config::validate_email)
        } else {
            setting
        };
        let u = ask.field(&setting, &current, true, !current.is_empty())?;
        changed |= overlay_update(
            overlay,
            &["users", profile_key, forge_key, &host],
            existing.and_then(|p| lookup(p, &host)).map(String::as_str),
            &u,
        );
    }
    Ok(changed)
}

enum TokenChoice {
    Saved(String),
    KeepExisting,
    Skipped,
}

/// Where [`instance_token`] left a token.
#[derive(Debug, PartialEq)]
enum TokenOutcome {
    /// Nothing to do: skipped, or kept where it already was.
    Unchanged,
    /// In the overlay, because no keyring could be used.
    InOverlay,
    /// In the keyring; `was_in_file` when the overlay had it too, and
    /// the file has to be rid of it.
    InKeyring { was_in_file: bool },
}

/// One forge's tokens: the overlay table that held them, what to call
/// one, and how to name an instance's host and keyring entry and
/// prompt for its token.
struct TokenKind {
    table: &'static str,
    what: &'static str,
    host: fn(&str) -> String,
    secret: fn(&str) -> sandogasa_config::secret::SecretRef,
    prompt: PromptFn,
}

/// How a forge asks for an instance's token, given the saved one.
type PromptFn = fn(&str, Option<&str>) -> Result<TokenChoice, Box<dyn std::error::Error>>;

/// Every forge's `validate_token` has this shape: `Ok(true)` valid,
/// `Ok(false)` rejected, `Err` couldn't reach the API.
type ValidateFn = fn(&str, &str) -> Result<bool, Box<dyn std::error::Error>>;

/// Interactively collect an API token for `instance` on one forge.
/// Validates an existing token first (and keeps it if still valid),
/// then asks for a new one, hidden. An empty response
/// skips the instance — useful when a shell env var is providing
/// the token and the user doesn't want to persist it.
///
/// `prompt` is shown before asking (it names the scope the
/// forge needs). A failed check keeps the existing token for every
/// forge: an unreachable API says nothing about a credential, and
/// discarding one over a transient outage is guesswork.
fn prompt_token(
    instance: &str,
    existing: Option<&str>,
    prompt: &str,
    validate: ValidateFn,
    read: &mut Reader<'_>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    if let Some(tok) = existing {
        // An unreachable API keeps the token for every forge now, not
        // only those that asked for it: a failed check says nothing
        // about the credential, and re-prompting on one is guesswork
        // that throws away a token that may be perfectly good.
        let checked = sandogasa_cli::credential::validated_key_blocking(
            instance,
            "token",
            tok.to_string(),
            |token| {
                use sandogasa_cli::credential::KeyVerdict;
                match validate(instance, token) {
                    Ok(true) => KeyVerdict::Accepted("valid.".to_string()),
                    Ok(false) => KeyVerdict::Refused(format!("rejected by {instance}.")),
                    Err(e) => KeyVerdict::Unknown(format!("check failed ({e}); keeping it.")),
                }
            },
        );
        if checked.key == tok {
            return Ok(TokenChoice::KeepExisting);
        }
        if checked.key.is_empty() {
            return Ok(TokenChoice::Skipped);
        }
        return Ok(TokenChoice::Saved(checked.key));
    }
    let hint = prompt
        .trim_end()
        .trim_end_matches(':')
        .trim_end_matches("(enter to skip)")
        .trim_end();
    // A token the forge rejects is asked for again, not the end of the
    // run: the answers before it would be lost with it.
    let setting = Setting::new(instance, "token (empty to skip)")
        .sensitive()
        .optional()
        .hint(hint);
    loop {
        let token = ask_with(&setting, "", false, read)?;
        // Checked here, not by `validated_key_blocking`: neither an empty
        // answer nor the off marker is sent to the forge as a token.
        if !sandogasa_config::secret::is_set(&token) {
            return Ok(TokenChoice::Skipped);
        }
        eprint!("  Validating... ");
        match validate(instance, &token) {
            Ok(true) => {
                eprintln!("valid.");
                return Ok(TokenChoice::Saved(token));
            }
            Ok(false) => {
                eprintln!("rejected by {instance}; enter another, or nothing to skip.");
            }
            // As for a saved token: a check that could not run says
            // nothing about the token, so it is kept.
            Err(e) => {
                eprintln!("check failed ({e}); keeping it.");
                return Ok(TokenChoice::Saved(token));
            }
        }
    }
}

/// GitLab tokens. An unreachable API re-prompts rather than
/// keeping the existing token.
fn prompt_gitlab_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    prompt_token(
        instance,
        existing,
        &format!(
            "  Paste a personal access token for {instance} with 'api' scope \
             (enter to skip): "
        ),
        sandogasa_gitlab::validate_token,
        &mut sandogasa_config::read_line_from_terminal,
    )
}

/// GitHub tokens. An `Ok(false)` means the saved token is actually
/// invalid (re-prompt), while `Err` means we couldn't reach the API
/// (warn and keep the existing token so the user can retry).
fn prompt_github_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    prompt_token(
        instance,
        existing,
        &format!(
            "  Paste a personal access token for {instance} with 'repo' scope \
             (enter to skip): "
        ),
        sandogasa_github::validate_token,
        &mut sandogasa_config::read_line_from_terminal,
    )
}

/// Forgejo / Gitea tokens. Treats an unreachable API (`Err`) as
/// "keep the existing token" so a transient outage doesn't force
/// re-entry.
fn prompt_forgejo_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    prompt_token(
        instance,
        existing,
        &format!(
            "  Paste a personal access token for {instance} \
             (enter to skip): "
        ),
        sandogasa_forgejo::validate_token,
        &mut sandogasa_config::read_line_from_terminal,
    )
}

/// Sourcehut tokens — a personal access token from
/// meta.sr.ht/oauth2/personal-token (which grants read access
/// across services by default).
fn prompt_sourcehut_token(
    instance: &str,
    existing: Option<&str>,
) -> Result<TokenChoice, Box<dyn std::error::Error>> {
    prompt_token(
        instance,
        existing,
        &format!(
            "  Paste a personal access token for {instance} \
             (from meta.sr.ht/oauth2/personal-token; enter to skip): "
        ),
        sandogasa_sourcehut::validate_token,
        &mut sandogasa_config::read_line_from_terminal,
    )
}

const GITLAB: TokenKind = TokenKind {
    table: "gitlab_tokens",
    what: "GitLab token",
    host: crate::gitlab::instance_host,
    secret: crate::gitlab::secret,
    prompt: prompt_gitlab_token,
};

const GITHUB: TokenKind = TokenKind {
    table: "github_tokens",
    what: "GitHub token",
    host: crate::github::instance_host,
    secret: crate::github::secret,
    prompt: prompt_github_token,
};

const FORGEJO: TokenKind = TokenKind {
    table: "forgejo_tokens",
    what: "Forgejo/Gitea token",
    host: crate::forgejo::instance_host,
    secret: crate::forgejo::secret,
    prompt: prompt_forgejo_token,
};

const SOURCEHUT: TokenKind = TokenKind {
    table: "sourcehut_tokens",
    what: "Sourcehut token",
    host: crate::sourcehut::instance_host,
    secret: crate::sourcehut::secret,
    prompt: prompt_sourcehut_token,
};

/// What to call `host`'s token in messages ("GitLab token for
/// gitlab.com").
fn token_what(kind: &TokenKind, host: &str) -> String {
    format!("{} for {host}", kind.what)
}

/// Settle `instance`'s token through
/// [`fill`](sandogasa_config::secret::fill) and
/// [`settle`](sandogasa_config::secret::settle), by their rules for an
/// optional secret, which every instance is: the overlay is a
/// `toml::Value`, not a struct, so the token at `[<table>] <host>` is
/// taken out as a string, driven through them as one slot, and written
/// back, the overlay keeping everything else as it is.
///
/// One turned off for sandogasa-report is left off on a plain run, with
/// nothing asked and the keyring not read; `review` (`--all`) offers it
/// again, and a yes to using the keyring's entry again settles it with
/// no prompt. Otherwise `choose` is asked for it, offered the saved one
/// (the keyring's, else the overlay's).
fn instance_token(
    overlay: &mut toml::Value,
    kind: &TokenKind,
    instance: &str,
    host: &str,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
    choose: impl FnOnce(Option<&str>) -> Result<TokenChoice, Box<dyn std::error::Error>>,
) -> Result<TokenOutcome, Box<dyn std::error::Error>> {
    instance_token_in(
        crate::secrets::keyring(),
        overlay,
        kind,
        instance,
        host,
        review,
        ask,
        choose,
    )
}

/// [`instance_token`] with the keyring passed in.
#[allow(clippy::too_many_arguments)]
fn instance_token_in(
    store: &dyn sandogasa_config::secret::SecretStore,
    overlay: &mut toml::Value,
    kind: &TokenKind,
    instance: &str,
    host: &str,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
    choose: impl FnOnce(Option<&str>) -> Result<TokenChoice, Box<dyn std::error::Error>>,
) -> Result<TokenOutcome, Box<dyn std::error::Error>> {
    use sandogasa_config::secret::{Before, OFF, Slot};
    const TOOL: &str = "sandogasa-report";
    let path = [kind.table, host];
    let file = overlay_get_str(overlay, &path);
    let what = token_what(kind, host);
    let mut value = file.clone().unwrap_or_default();
    fn slot<'a>(
        kind: &TokenKind,
        host: &'a str,
        what: &'a str,
        instance: &str,
        value: &'a mut String,
    ) -> Slot<'a> {
        Slot {
            table: kind.table,
            key: host,
            what,
            entry: (kind.secret)(instance),
            value,
            required: false,
        }
    }
    let before = sandogasa_config::secret::fill(
        store,
        TOOL,
        vec![slot(kind, host, &what, instance, &mut value)],
        review,
        ask,
    );
    let mut saved_new = false;
    match before.first() {
        // Left off, or turned back on with the keyring's entry: nothing
        // to ask.
        Some(Before::Off) if value == OFF => return Ok(TokenOutcome::Unchanged),
        Some(Before::Set { .. }) if file.as_deref() == Some(OFF) => {}
        _ => {
            let existing = sandogasa_config::secret::is_set(&value).then(|| value.clone());
            match choose(existing.as_deref())? {
                TokenChoice::Saved(t) => {
                    saved_new = true;
                    value = t;
                }
                TokenChoice::KeepExisting => {}
                TokenChoice::Skipped => value.clear(),
            }
        }
    }
    sandogasa_config::secret::settle(
        store,
        TOOL,
        vec![slot(kind, host, &what, instance, &mut value)],
        &before,
    );
    if value.is_empty() {
        // In the keyring: out of the overlay.
        let was_in_file = overlay_remove(overlay, &path);
        return Ok(if was_in_file || saved_new {
            TokenOutcome::InKeyring { was_in_file }
        } else {
            TokenOutcome::Unchanged
        });
    }
    if file.as_deref() == Some(value.as_str()) {
        return Ok(TokenOutcome::Unchanged);
    }
    // No keyring to keep it in, or turned off ("-").
    overlay_set_str(overlay, &path, &value);
    Ok(TokenOutcome::InOverlay)
}

/// The paths of the settings in `read` that `now` no longer has, the
/// innermost one for each, under `prefix`.
fn cleared_paths(read: &toml::Value, now: &toml::Value, prefix: &[String]) -> Vec<Vec<String>> {
    let (Some(read), Some(now)) = (read.as_table(), now.as_table()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (key, value) in read {
        let path: Vec<String> = prefix.iter().cloned().chain([key.clone()]).collect();
        match now.get(key) {
            None => out.push(path),
            Some(kept) => out.extend(cleared_paths(value, kept, &path)),
        }
    }
    out
}

/// Save `overlay` to `cf`, then take out of the file the settings the
/// answers cleared, which `save` (laying the overlay over the file)
/// would leave there: those in `read`, the overlay as it was read, that
/// it no longer has. Then the `(table, key)` tokens now in the keyring.
/// Only those go, so an edit made to the file meanwhile is kept. Whether
/// the file, left with nothing in it, was removed.
fn write_overlay(
    cf: &sandogasa_config::ConfigFile,
    read: &toml::Value,
    overlay: &toml::Value,
    out_of_file: &[(&str, String)],
) -> Result<bool, Box<dyn std::error::Error>> {
    cf.save(overlay)?;
    let mut removed = false;
    for path in cleared_paths(read, overlay, &[]) {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        removed |= cf.remove_path(&path)?;
    }
    for (table, key) in out_of_file {
        removed |= cf.remove(table, key)?;
    }
    Ok(removed)
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

/// Read a string leaf at `path` from a `toml::Value`. Returns
/// `None` if any intermediate key is missing or the leaf isn't a
/// string.
/// Record an answer for the string at `path`: set it when it differs
/// from what the profile had, remove it when the answer clears it
/// (empty, or `-` at the prompt). Whether anything changed.
fn overlay_update(value: &mut toml::Value, path: &[&str], had: Option<&str>, answer: &str) -> bool {
    match (had, answer.is_empty()) {
        (Some(old), false) if old == answer => false,
        (_, false) => {
            overlay_set_str(value, path, answer);
            true
        }
        (Some(_), true) => overlay_remove(value, path),
        (None, true) => false,
    }
}

/// Remove the leaf at `path` from the overlay; whether it was there.
/// Only the overlay is touched: a value from the main config stays.
fn overlay_remove(value: &mut toml::Value, path: &[&str]) -> bool {
    let Some((leaf, parents)) = path.split_last() else {
        return false;
    };
    let mut cur = value;
    for key in parents {
        match cur.get_mut(*key) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    cur.as_table_mut()
        .is_some_and(|t| t.remove(*leaf).is_some())
}

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
    use sandogasa_config::secret::never;
    use sandogasa_config::test_support::EnvGuard;

    const GITLAB_I: &str = "https://gitlab.keyring.test";
    const GITHUB_I: &str = "https://api.github.keyring.test";
    const FORGEJO_I: &str = "https://forgejo.keyring.test";
    const SOURCEHUT_I: &str = "sr.keyring.test";

    /// A config directory of its own, with no token exported that
    /// would answer before the keyring.
    fn isolated(dir: &tempfile::TempDir) -> EnvGuard {
        EnvGuard::new(&[("XDG_CONFIG_HOME", &dir.path().to_string_lossy())]).unset(&[
            "FORGEJO_TOKEN",
            "GITHUB_TOKEN",
            "GITLAB_TOKEN",
            "SOURCEHUT_TOKEN",
        ])
    }

    fn write_user_overlay(text: &str) -> sandogasa_config::ConfigFile {
        let cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
        std::fs::create_dir_all(cf.path().parent().unwrap()).unwrap();
        std::fs::write(cf.path(), text).unwrap();
        cf
    }

    /// Each forge's token for its test instance, as a report finds it.
    fn tokens_found() -> [String; 4] {
        let cfg = config::load_config(None).unwrap();
        [
            crate::gitlab::find_token(GITLAB_I, &cfg.gitlab_tokens).unwrap(),
            crate::github::find_token(GITHUB_I, &cfg.github_tokens).unwrap(),
            crate::forgejo::find_token(FORGEJO_I, &cfg.forgejo_tokens).unwrap(),
            crate::sourcehut::find_token(SOURCEHUT_I, &cfg.sourcehut_tokens).unwrap(),
        ]
    }

    /// A field cleared at its prompt leaves the file, not only the
    /// overlay in memory, nested ones included, and the next report no
    /// longer reads it (#177).
    #[test]
    #[serial_test::serial]
    fn a_cleared_field_leaves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let text = "[notes]\ndir = \"~/notes\"\n\n\
            [users.me]\nfas = \"me\"\nbugzilla_email = \"old@example.org\"\n\n\
            [users.me.gitlab]\n\"gitlab.com\" = \"me-gl\"\n";
        let cf = write_user_overlay(text);
        let mut overlay = parse_overlay(text).unwrap();
        assert!(overlay_update(
            &mut overlay,
            &["users", "me", "bugzilla_email"],
            Some("old@example.org"),
            ""
        ));
        assert!(overlay_update(
            &mut overlay,
            &["users", "me", "gitlab", "gitlab.com"],
            Some("me-gl"),
            ""
        ));
        assert!(overlay_update(
            &mut overlay,
            &["notes", "dir"],
            Some("~/notes"),
            ""
        ));
        // Meanwhile, a table added to the file by hand: kept.
        let mut edited = std::fs::read_to_string(cf.path()).unwrap();
        edited.push_str("\n[defaults]\nexplain = true\n");
        std::fs::write(cf.path(), edited).unwrap();
        let read = parse_overlay(text).unwrap();
        assert!(!write_overlay(&cf, &read, &overlay, &[]).unwrap());
        let left = std::fs::read_to_string(cf.path()).unwrap();
        assert!(!left.contains("old@example.org"), "{left}");
        assert!(!left.contains("me-gl"), "{left}");
        assert!(!left.contains("notes"), "{left}");
        assert!(left.contains("fas = \"me\""), "{left}");
        assert!(left.contains("[defaults]\nexplain = true"), "{left}");
        let cfg: config::ReportConfig = cf.load_user().unwrap();
        assert_eq!(cfg.users["me"].bugzilla_email, None);
    }

    /// The tokens `config` keeps in the keyring are what a report then
    /// uses, the overlay is left without them, and the environment
    /// still wins.
    #[test]
    #[serial_test::serial]
    fn tokens_kept_in_the_keyring_are_the_ones_used() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let text = "[users.me]\nfas = \"me\"\n\n\
            [gitlab_tokens]\n\"gitlab.keyring.test\" = \"glpat-saved\"\n\n\
            [github_tokens]\n\"api.github.keyring.test\" = \"ghp-saved\"\n\n\
            [forgejo_tokens]\n\"forgejo.keyring.test\" = \"fj-saved\"\n\n\
            [sourcehut_tokens]\n\"sr.keyring.test\" = \"sh-saved\"\n";
        let cf = write_user_overlay(text);
        let saved = ["glpat-saved", "ghp-saved", "fj-saved", "sh-saved"];
        // An existing setup keeps working.
        assert_eq!(tokens_found(), saved);

        // `config` run again, every token kept as it was: each moves
        // to the keyring.
        let mut overlay = parse_overlay(text).unwrap();
        let mut out_of_file = Vec::new();
        for (kind, instance, token) in [
            (GITLAB, GITLAB_I, saved[0]),
            (GITHUB, GITHUB_I, saved[1]),
            (FORGEJO, FORGEJO_I, saved[2]),
            (SOURCEHUT, SOURCEHUT_I, saved[3]),
        ] {
            let host = (kind.host)(instance);
            let outcome = instance_token(
                &mut overlay,
                &kind,
                instance,
                &host,
                false,
                &never,
                |existing| {
                    assert_eq!(existing, Some(token));
                    Ok(TokenChoice::KeepExisting)
                },
            )
            .unwrap();
            assert_eq!(outcome, TokenOutcome::InKeyring { was_in_file: true });
            out_of_file.push((kind.table, host));
        }
        let read = overlay.clone();
        assert!(!write_overlay(&cf, &read, &overlay, &out_of_file).unwrap());
        let left = std::fs::read_to_string(cf.path()).unwrap();
        assert!(
            !left.contains("saved"),
            "the file is left without them: {left}"
        );
        assert!(!left.contains("_tokens"), "{left}");
        assert!(left.contains("fas = \"me\""), "{left}");

        // From the keyring now: the overlay no longer has them.
        assert_eq!(tokens_found(), saved);
        // Run again, nothing is left to move.
        let outcome = instance_token(
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            "gitlab.keyring.test",
            false,
            &never,
            |e| {
                assert_eq!(e, Some("glpat-saved"));
                Ok(TokenChoice::KeepExisting)
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);

        // The environment still wins, instance-specific or generic.
        let _env2 = EnvGuard::new(&[
            ("GITLAB_TOKEN_GITLAB_KEYRING_TEST", "glpat-env"),
            ("GITHUB_TOKEN", "ghp-env"),
        ]);
        let found = tokens_found();
        assert_eq!(found[0], "glpat-env");
        assert_eq!(found[1], "ghp-env");
        assert_eq!(found[2], "fj-saved");

        let keyring = crate::secrets::keyring();
        keyring.clear(&crate::gitlab::secret(GITLAB_I)).unwrap();
        keyring.clear(&crate::github::secret(GITHUB_I)).unwrap();
        keyring.clear(&crate::forgejo::secret(FORGEJO_I)).unwrap();
        keyring
            .clear(&crate::sourcehut::secret(SOURCEHUT_I))
            .unwrap();
    }

    /// Two instances of one forge are two services, so two entries.
    #[test]
    #[serial_test::serial]
    fn two_instances_of_a_forge_keep_separate_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let one = "https://gitlab.one.test";
        let two = "https://salsa.two.test/";
        let mut overlay = toml::Value::Table(Default::default());
        for (instance, token) in [(one, "glpat-one"), (two, "glpat-two")] {
            let host = crate::gitlab::instance_host(instance);
            let outcome = instance_token(
                &mut overlay,
                &GITLAB,
                instance,
                &host,
                false,
                &never,
                |existing| {
                    assert_eq!(existing, None);
                    Ok(TokenChoice::Saved(token.to_string()))
                },
            )
            .unwrap();
            assert_eq!(outcome, TokenOutcome::InKeyring { was_in_file: false });
        }
        assert_eq!(overlay, toml::Value::Table(Default::default()));
        assert_ne!(crate::gitlab::secret(one), crate::gitlab::secret(two));
        let none = std::collections::BTreeMap::new();
        assert_eq!(crate::gitlab::find_token(one, &none).unwrap(), "glpat-one");
        assert_eq!(crate::gitlab::find_token(two, &none).unwrap(), "glpat-two");
        // A forge skipped at the prompt keeps nothing.
        let outcome = instance_token(
            &mut overlay,
            &FORGEJO,
            FORGEJO_I,
            "forgejo.keyring.test",
            false,
            &never,
            |_| Ok(TokenChoice::Skipped),
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
        assert!(crate::forgejo::find_token(FORGEJO_I, &none).is_err());

        let keyring = crate::secrets::keyring();
        keyring.clear(&crate::gitlab::secret(one)).unwrap();
        keyring.clear(&crate::gitlab::secret(two)).unwrap();
    }

    /// The question `--all` asks for an instance turned off, recording
    /// each question with its default and answering `answer`.
    fn recording(
        asked: &std::cell::RefCell<Vec<(String, bool)>>,
        answer: bool,
    ) -> impl Fn(&str, bool) -> bool + '_ {
        move |q, default_yes| {
            asked.borrow_mut().push((q.to_string(), default_yes));
            answer
        }
    }

    const OFF_TEXT: &str =
        "[users.me]\nfas = \"me\"\n\n[gitlab_tokens]\n\"gitlab.keyring.test\" = \"-\"\n";
    const QUESTION: &str = "GitLab token for gitlab.keyring.test: off for sandogasa-report \
                            (the keyring has one). Use it again?";

    /// A plain run does not read the keyring for an instance turned off:
    /// a kind whose entry cannot even be named still settles.
    #[test]
    fn a_plain_run_does_not_read_the_keyring_for_a_token_turned_off() {
        struct Unread;
        impl sandogasa_config::secret::SecretStore for Unread {
            fn lookup(
                &self,
                _: &sandogasa_config::secret::SecretRef,
            ) -> Result<Option<String>, sandogasa_config::secret::SecretError> {
                panic!("read the keyring")
            }
            fn store(
                &self,
                _: &sandogasa_config::secret::SecretRef,
                _: &str,
            ) -> Result<(), sandogasa_config::secret::SecretError> {
                panic!("wrote to the keyring")
            }
            fn clear(
                &self,
                _: &sandogasa_config::secret::SecretRef,
            ) -> Result<(), sandogasa_config::secret::SecretError> {
                panic!("cleared the keyring")
            }
        }
        let mut overlay = parse_overlay(OFF_TEXT).unwrap();
        let outcome = instance_token_in(
            &Unread,
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            "gitlab.keyring.test",
            false,
            &|q, _| panic!("asked {q}"),
            |_| panic!("prompted for a token turned off"),
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
    }

    /// A plain run leaves an instance turned off as it is: nothing is
    /// asked, no token prompted for, and it stays "-", with or without
    /// an entry in the keyring.
    #[test]
    #[serial_test::serial]
    fn a_plain_run_leaves_a_token_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let host = "gitlab.keyring.test";
        let asked = std::cell::RefCell::new(Vec::new());
        for stored in [false, true] {
            if stored {
                crate::secrets::keyring()
                    .store(&crate::gitlab::secret(GITLAB_I), "glpat-kept")
                    .unwrap();
            }
            let mut overlay = parse_overlay(OFF_TEXT).unwrap();
            let outcome = instance_token(
                &mut overlay,
                &GITLAB,
                GITLAB_I,
                host,
                false,
                &recording(&asked, true),
                |_| panic!("prompted for a token turned off"),
            )
            .unwrap();
            assert_eq!(outcome, TokenOutcome::Unchanged);
            assert_eq!(overlay, parse_overlay(OFF_TEXT).unwrap(), "still off");
        }
        assert!(asked.borrow().is_empty());
        crate::secrets::keyring()
            .clear(&crate::gitlab::secret(GITLAB_I))
            .unwrap();
    }

    /// `--all` offers an instance turned off again: with an entry in the
    /// keyring it asks, defaulting to no, and a yes uses the entry again,
    /// the marker leaving the overlay and the file.
    #[test]
    #[serial_test::serial]
    fn a_review_can_use_the_keyring_again() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let cf = write_user_overlay(OFF_TEXT);
        let mut overlay = parse_overlay(OFF_TEXT).unwrap();
        let keyring = crate::secrets::keyring();
        keyring
            .store(&crate::gitlab::secret(GITLAB_I), "glpat-kept")
            .unwrap();
        let off = config::load_config(None).unwrap().gitlab_tokens;
        assert!(crate::gitlab::find_token(GITLAB_I, &off).is_err());

        let host = "gitlab.keyring.test";
        let asked = std::cell::RefCell::new(Vec::new());
        let outcome = instance_token(
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            true,
            &recording(&asked, true),
            |_| panic!("prompted after a yes"),
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::InKeyring { was_in_file: true });
        // Word for word what `sandogasa_config::secret::fill` asks.
        assert_eq!(asked.into_inner(), [(QUESTION.to_string(), false)]);
        assert!(
            !write_overlay(
                &cf,
                &overlay.clone(),
                &overlay,
                &[("gitlab_tokens", host.to_string())]
            )
            .unwrap()
        );
        let left = std::fs::read_to_string(cf.path()).unwrap();
        assert!(!left.contains("gitlab_tokens"), "{left}");
        let none = std::collections::BTreeMap::new();
        assert_eq!(
            crate::gitlab::find_token(GITLAB_I, &none).unwrap(),
            "glpat-kept"
        );
        keyring.clear(&crate::gitlab::secret(GITLAB_I)).unwrap();
    }

    /// `--all` with a no, or with no entry to go back to (not asked
    /// about), offers the token blank, so "-" is never checked against
    /// the forge: skipping keeps it off, and a new token turns it back on.
    #[test]
    #[serial_test::serial]
    fn a_review_otherwise_offers_a_token_turned_off_blank() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let host = "gitlab.keyring.test";
        let mut overlay = parse_overlay(OFF_TEXT).unwrap();
        let outcome = instance_token(
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            true,
            &|q, _| panic!("asked {q}"),
            |existing| {
                assert_eq!(existing, None);
                Ok(TokenChoice::Skipped)
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);

        let keyring = crate::secrets::keyring();
        keyring
            .store(&crate::gitlab::secret(GITLAB_I), "glpat-kept")
            .unwrap();
        let asked = std::cell::RefCell::new(Vec::new());
        let outcome = instance_token(
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            true,
            &recording(&asked, false),
            |existing| {
                assert_eq!(existing, None);
                Ok(TokenChoice::Skipped)
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
        assert_eq!(overlay, parse_overlay(OFF_TEXT).unwrap(), "still off");
        assert_eq!(asked.into_inner(), [(QUESTION.to_string(), false)]);
        // Keeping "the saved one" when none was offered keeps it off too.
        let outcome = instance_token(
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            true,
            &|_, _| false,
            |existing| {
                assert_eq!(existing, None);
                Ok(TokenChoice::KeepExisting)
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
        assert_eq!(overlay, parse_overlay(OFF_TEXT).unwrap(), "still off");

        let outcome = instance_token(&mut overlay, &GITLAB, GITLAB_I, host, true, &never, |_| {
            Ok(TokenChoice::Saved("glpat-new".into()))
        })
        .unwrap();
        assert_eq!(outcome, TokenOutcome::InKeyring { was_in_file: true });
        let none = std::collections::BTreeMap::new();
        assert_eq!(
            crate::gitlab::find_token(GITLAB_I, &none).unwrap(),
            "glpat-new"
        );
        keyring.clear(&crate::gitlab::secret(GITLAB_I)).unwrap();
    }

    /// A saved token cleared at the prompt is turned off for
    /// sandogasa-report ("-" in the overlay), the keyring's entry left
    /// for the other tools, as every tool's `config` does; the next plain
    /// run asks nothing about it.
    #[test]
    #[serial_test::serial]
    fn a_cleared_token_is_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let host = "gitlab.keyring.test";
        let keyring = crate::secrets::keyring();
        keyring
            .store(&crate::gitlab::secret(GITLAB_I), "glpat-kept")
            .unwrap();
        let mut overlay = parse_overlay("[users.me]\nfas = \"me\"\n").unwrap();
        let outcome = instance_token(&mut overlay, &GITLAB, GITLAB_I, host, false, &never, |e| {
            assert_eq!(e, Some("glpat-kept"));
            Ok(TokenChoice::Skipped)
        })
        .unwrap();
        assert_eq!(outcome, TokenOutcome::InOverlay);
        assert_eq!(
            overlay_get_str(&overlay, &["gitlab_tokens", host]).as_deref(),
            Some("-")
        );
        assert_eq!(
            keyring
                .lookup(&crate::gitlab::secret(GITLAB_I))
                .unwrap()
                .as_deref(),
            Some("glpat-kept")
        );
        let outcome = instance_token(&mut overlay, &GITLAB, GITLAB_I, host, false, &never, |_| {
            panic!("prompted for a token turned off")
        })
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
        keyring.clear(&crate::gitlab::secret(GITLAB_I)).unwrap();
    }

    /// Without a keyring, a token is kept in the overlay, and kept as it
    /// is when run again.
    #[test]
    fn without_a_keyring_the_token_stays_in_the_overlay() {
        let none = sandogasa_config::secret::NoKeyring;
        let host = "gitlab.keyring.test";
        let mut overlay = toml::Value::Table(Default::default());
        let outcome = instance_token_in(
            &none,
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            false,
            &never,
            |e| {
                assert_eq!(e, None);
                Ok(TokenChoice::Saved("glpat-file".into()))
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::InOverlay);
        assert_eq!(
            overlay_get_str(&overlay, &["gitlab_tokens", host]).as_deref(),
            Some("glpat-file")
        );
        let outcome = instance_token_in(
            &none,
            &mut overlay,
            &GITLAB,
            GITLAB_I,
            host,
            false,
            &never,
            |e| {
                assert_eq!(e, Some("glpat-file"));
                Ok(TokenChoice::KeepExisting)
            },
        )
        .unwrap();
        assert_eq!(outcome, TokenOutcome::Unchanged);
    }

    /// A reader answering each prompt from `answers` in turn, recording
    /// the prompts; one asked for past the last answer fails the test.
    fn scripted<'a>(
        answers: &'a [&'a str],
        asked: &'a std::cell::RefCell<Vec<String>>,
    ) -> impl FnMut(&str, bool) -> std::io::Result<String> + 'a {
        let mut next = answers.iter();
        move |prompt, _hidden| {
            asked.borrow_mut().push(prompt.to_string());
            next.next()
                .map(|a| a.to_string())
                .ok_or_else(|| std::io::Error::other(format!("no answer for {prompt:?}")))
        }
    }

    /// `config` end to end, from answers: a new profile with its FAS
    /// login, Bugzilla email and the email on another Bugzilla, saved
    /// to the overlay; run again with the same answers, nothing
    /// changes; with no profile named, nothing is asked.
    #[test]
    #[serial_test::serial]
    fn config_saves_a_profile_from_its_answers() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let main = dir.path().join("main.toml");
        std::fs::write(
            &main,
            "[domains.opensuse.bugzilla]\ninstance = \"https://bugzilla.opensuse.org\"\n",
        )
        .unwrap();
        let args = ConfigArgs {
            all: false,
            config: Some(main.display().to_string()),
            missing: false,
        };
        let asked = std::cell::RefCell::new(Vec::new());
        // Profile, FAS (keeping the profile's name), Bugzilla email,
        // openSUSE's email, notes directory.
        let answers = [
            "me",
            "",
            "me@example.com",
            "me@suse.example",
            "~/Nextcloud/notes",
        ];
        run_inner(&args, &mut scripted(&answers, &asked)).unwrap();
        let cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
        let saved = parse_overlay(&std::fs::read_to_string(cf.path()).unwrap()).unwrap();
        assert_eq!(
            overlay_get_str(&saved, &["users", "me", "fas"]).as_deref(),
            Some("me")
        );
        assert_eq!(
            overlay_get_str(&saved, &["users", "me", "bugzilla_email"]).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(
            overlay_get_str(
                &saved,
                &["users", "me", "bugzilla_emails", "bugzilla.opensuse.org"]
            )
            .as_deref(),
            Some("me@suse.example")
        );
        assert_eq!(
            overlay_get_str(&saved, &["notes", "dir"]).as_deref(),
            Some("~/Nextcloud/notes")
        );
        assert_eq!(asked.borrow().len(), 5, "{:?}", asked.borrow());
        // One service and one thing asked per instance.
        assert_eq!(
            asked.borrow()[3],
            "Enter Bugzilla email on bugzilla.opensuse.org: "
        );

        // The same answers again, empty ones keeping what is saved.
        let before = std::fs::read_to_string(cf.path()).unwrap();
        run_inner(&args, &mut scripted(&["me", "", "", "", ""], &asked)).unwrap();
        assert_eq!(std::fs::read_to_string(cf.path()).unwrap(), before);

        // A relative directory is refused and asked again; `-` clears it.
        run_inner(
            &args,
            &mut scripted(&["me", "", "", "", "notes", "-"], &asked),
        )
        .unwrap();
        let saved = parse_overlay(&std::fs::read_to_string(cf.path()).unwrap()).unwrap();
        assert_eq!(overlay_get_str(&saved, &["notes", "dir"]), None);

        // No profile named: nothing else asked, nothing written.
        asked.borrow_mut().clear();
        run_inner(&args, &mut scripted(&[""], &asked)).unwrap();
        assert_eq!(asked.borrow().len(), 1);
    }

    #[test]
    #[serial_test::serial]
    fn missing_asks_only_for_what_is_not_set() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let main = dir.path().join("main.toml");
        std::fs::write(
            &main,
            "[domains.opensuse.bugzilla]\ninstance = \"https://bugzilla.opensuse.org\"\n",
        )
        .unwrap();
        let mut args = ConfigArgs {
            all: false,
            config: Some(main.display().to_string()),
            missing: false,
        };
        let asked = std::cell::RefCell::new(Vec::new());
        let answers = ["me", "", "me@example.com", "me@suse.example", "~/notes"];
        run_inner(&args, &mut scripted(&answers, &asked)).unwrap();

        // Everything set, and one profile: nothing asked, not even its name.
        args.missing = true;
        asked.borrow_mut().clear();
        let cf = sandogasa_config::ConfigFile::for_tool("sandogasa-report");
        let before = std::fs::read_to_string(cf.path()).unwrap();
        run_inner(&args, &mut scripted(&[], &asked)).unwrap();
        assert!(asked.borrow().is_empty(), "{:?}", asked.borrow());
        assert_eq!(std::fs::read_to_string(cf.path()).unwrap(), before);

        // A new instance in the main config: only its email is asked.
        std::fs::write(
            &main,
            "[domains.opensuse.bugzilla]\ninstance = \"https://bugzilla.opensuse.org\"\n\
             [domains.other.bugzilla]\ninstance = \"https://bugzilla.example.org\"\n",
        )
        .unwrap();
        run_inner(&args, &mut scripted(&["me@other.example"], &asked)).unwrap();
        assert_eq!(
            *asked.borrow(),
            ["Enter Bugzilla email on bugzilla.example.org: "]
        );
        let saved = parse_overlay(&std::fs::read_to_string(cf.path()).unwrap()).unwrap();
        assert_eq!(
            overlay_get_str(
                &saved,
                &["users", "me", "bugzilla_emails", "bugzilla.example.org"]
            )
            .as_deref(),
            Some("me@other.example")
        );
        assert_eq!(
            overlay_get_str(&saved, &["users", "me", "bugzilla_email"]).as_deref(),
            Some("me@example.com")
        );
    }

    /// Every forge's username is asked per instance and recorded under
    /// its host, the Sourcehut commit emails as a list; an answer that
    /// clears a username removes it.
    #[test]
    fn a_profile_records_each_forges_usernames() {
        let set =
            |items: &[&str]| -> BTreeSet<String> { items.iter().map(|s| s.to_string()).collect() };
        let mut overlay = toml::Value::Table(Default::default());
        let asked = std::cell::RefCell::new(Vec::new());
        // Sourcehut's commit emails come after its username.
        let answers = [
            "me-fas",
            "",
            "me-gl",
            "me-gh",
            "me-fj",
            "me-pg",
            "me-sh",
            "me@example.com, *",
            // Debian's, checked as emails: the bad one asked again.
            "me@debian.example, not-an-email",
            "me@debian.example, me@example.org",
        ];
        let changed = prompt_profile(
            &mut overlay,
            "me",
            None,
            true,
            &set(&["https://gitlab.com"]),
            &set(&["https://api.github.com"]),
            &set(&["https://codeberg.org"]),
            &set(&["sr.ht"]),
            &set(&["https://code.example.org"]),
            &BTreeSet::new(),
            &mut Asker {
                read: &mut scripted(&answers, &asked),
                missing: false,
                kept: 0,
            },
        )
        .unwrap();
        assert!(changed);
        // GitHub by its web host, keyed by its API host as before.
        assert!(
            asked
                .borrow()
                .iter()
                .any(|p| p == "Enter GitHub username on github.com: "),
            "{:?}",
            asked.borrow()
        );
        let get = |path: &[&str]| overlay_get_str(&overlay, path);
        assert_eq!(get(&["users", "me", "fas"]).as_deref(), Some("me-fas"));
        assert_eq!(get(&["users", "me", "bugzilla_email"]), None);
        assert_eq!(
            get(&["users", "me", "gitlab", "gitlab.com"]).as_deref(),
            Some("me-gl")
        );
        assert_eq!(
            get(&["users", "me", "github", "api.github.com"]).as_deref(),
            Some("me-gh")
        );
        assert_eq!(
            get(&["users", "me", "forgejo", "codeberg.org"]).as_deref(),
            Some("me-fj")
        );
        assert_eq!(
            get(&["users", "me", "pagure", "code.example.org"]).as_deref(),
            Some("me-pg")
        );
        assert_eq!(
            get(&["users", "me", "sourcehut", "sr.ht"]).as_deref(),
            Some("me-sh")
        );
        assert_eq!(
            overlay["users"]["me"]["git_emails"],
            toml::Value::Array(vec!["me@example.com".into(), "*".into()])
        );
        assert_eq!(
            overlay["users"]["me"]["debian_emails"],
            toml::Value::Array(vec!["me@debian.example".into(), "me@example.org".into()])
        );
    }

    /// A blank answer for a list of emails is written as an empty list,
    /// so `--missing` takes it as answered and does not ask again.
    #[test]
    fn a_blank_email_list_is_recorded_as_answered() {
        let mut overlay = toml::Value::Table(Default::default());
        let asked = std::cell::RefCell::new(Vec::new());
        let changed = prompt_email_list(
            &mut overlay,
            "me",
            "debian_emails",
            Vec::new(),
            ("Debian", "emails"),
            &mut Asker {
                read: &mut scripted(&[""], &asked),
                missing: false,
                kept: 0,
            },
        )
        .unwrap();
        assert!(changed);
        assert_eq!(
            overlay["users"]["me"]["debian_emails"],
            toml::Value::Array(Vec::new())
        );
        // Asked again with nothing to answer: kept, not asked.
        let none = std::cell::RefCell::new(Vec::new());
        let mut ask = Asker {
            read: &mut scripted(&[], &none),
            missing: true,
            kept: 0,
        };
        let changed = prompt_email_list(
            &mut overlay,
            "me",
            "debian_emails",
            Vec::new(),
            ("Debian", "emails"),
            &mut ask,
        )
        .unwrap();
        assert!(!changed);
        assert_eq!(ask.kept, 1);
        assert!(none.borrow().is_empty());
    }

    /// A new token is checked with the forge before it is kept: a valid
    /// one is kept, an empty answer or "-" skips the instance, and one
    /// the forge refuses is asked for again, and one that cannot be
    /// checked is kept. A saved token the forge accepts is kept without
    /// a question.
    #[test]
    fn a_new_token_is_validated_before_it_is_kept() {
        let asked = std::cell::RefCell::new(Vec::new());
        let ask = |answers: &[&str], validate: ValidateFn| {
            let asked = std::cell::RefCell::new(Vec::new());
            prompt_token(
                "gitlab.example",
                None,
                "  Paste a token (enter to skip): ",
                validate,
                &mut scripted(answers, &asked),
            )
        };
        let ok: ValidateFn = |_, _| Ok(true);
        let refused: ValidateFn = |_, _| Ok(false);
        let unreachable: ValidateFn = |_, _| Err("no route".into());
        assert!(matches!(ask(&["glpat"], ok).unwrap(), TokenChoice::Saved(t) if t == "glpat"));
        assert!(matches!(ask(&[""], ok).unwrap(), TokenChoice::Skipped));
        assert!(matches!(ask(&["-"], ok).unwrap(), TokenChoice::Skipped));
        // A refused token is asked for again, and the next answer stands.
        assert!(matches!(
            ask(&["bad", ""], refused).unwrap(),
            TokenChoice::Skipped
        ));
        // A check that cannot run keeps the token, as for a saved one.
        assert!(
            matches!(ask(&["glpat"], unreachable).unwrap(), TokenChoice::Saved(t) if t == "glpat")
        );
        let kept = prompt_token(
            "gitlab.example",
            Some("glpat-saved"),
            "",
            ok,
            &mut scripted(&[], &asked),
        )
        .unwrap();
        assert!(matches!(kept, TokenChoice::KeepExisting));
        assert!(asked.borrow().is_empty());
    }

    /// The entries are the ones every sandogasa tool reads for the
    /// same service.
    #[test]
    fn entries_are_named_for_the_instance_host() {
        let service = |r: sandogasa_config::secret::SecretRef| r.service().to_string();
        // cpu-sig-tracker, cbspkg and hs-relmon read this one too.
        assert_eq!(
            service(crate::gitlab::secret("https://gitlab.com")),
            "gitlab.com"
        );
        assert_eq!(
            service(crate::github::secret("https://api.github.com")),
            "api.github.com"
        );
        // A GitHub Enterprise API base: its host.
        assert_eq!(
            service(crate::github::secret("https://ghe.example.test/api/v3")),
            "ghe.example.test"
        );
        assert_eq!(
            service(crate::forgejo::secret("https://codeberg.org/")),
            "codeberg.org"
        );
        assert_eq!(service(crate::sourcehut::secret("sr.ht")), "sr.ht");
    }

    /// With every token moved to the keyring, an overlay that held only
    /// tokens is removed rather than left empty.
    #[test]
    #[serial_test::serial]
    fn an_overlay_of_only_tokens_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let _env = isolated(&dir);
        let text = "[gitlab_tokens]\n\"gitlab.keyring.test\" = \"glpat-saved\"\n";
        let cf = write_user_overlay(text);
        let mut overlay = parse_overlay(text).unwrap();
        let host = "gitlab.keyring.test";
        instance_token(&mut overlay, &GITLAB, GITLAB_I, host, false, &never, |_| {
            Ok(TokenChoice::KeepExisting)
        })
        .unwrap();
        assert!(
            write_overlay(
                &cf,
                &overlay.clone(),
                &overlay,
                &[("gitlab_tokens", host.to_string())]
            )
            .unwrap()
        );
        assert!(!cf.path().exists());
        crate::secrets::keyring()
            .clear(&crate::gitlab::secret(GITLAB_I))
            .unwrap();
    }

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
    fn an_answer_sets_keeps_or_clears_the_overlay_value() {
        let mut v = parse_overlay("[users.me]\nbugzilla_email = \"me@example.com\"\n").unwrap();
        let path = ["users", "me", "bugzilla_email"];
        // The same answer changes nothing.
        assert!(!overlay_update(
            &mut v,
            &path,
            Some("me@example.com"),
            "me@example.com"
        ));
        // A new one replaces it.
        assert!(overlay_update(
            &mut v,
            &path,
            Some("me@example.com"),
            "new@example.com"
        ));
        assert_eq!(
            overlay_get_str(&v, &path).as_deref(),
            Some("new@example.com")
        );
        // Cleared at the prompt, it is removed.
        assert!(overlay_update(&mut v, &path, Some("new@example.com"), ""));
        assert_eq!(overlay_get_str(&v, &path), None);
        // Nothing there and nothing given: nothing to do.
        assert!(!overlay_update(&mut v, &path, None, ""));
        // A value only the main config has is not the overlay's to remove.
        assert!(!overlay_remove(&mut v, &["users", "other", "fas"]));
    }

    #[test]
    fn overlay_get_str_missing_path_is_none() {
        let v: toml::Value = toml::from_str(r#"x = 1"#).unwrap();
        assert!(overlay_get_str(&v, &["foo", "bar"]).is_none());
    }
}
