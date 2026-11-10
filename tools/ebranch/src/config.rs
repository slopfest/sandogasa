// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for ebranch.
//!
//! Keeps the Bugzilla, NVD, Forgejo and RPM Fusion credentials in the
//! desktop keyring when there is one (see [`crate::secrets`]), else in
//! `~/.config/ebranch/config.toml` with restricted permissions (dir
//! 700, file 600), and carries standing preferences such as the crates
//! `check-crate` ignores.

use serde::{Deserialize, Serialize};

/// Top-level config structure.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct EbranchConfig {
    #[serde(default)]
    pub bugzilla: BugzillaConfig,
    /// `[nvd]` table: an API key lifts NVD's pace from one request
    /// per 6 s to one per 0.6 s when CVE trackers are judged.
    #[serde(default)]
    pub nvd: NvdConfig,
    /// `[check-crate]` table.
    #[serde(default, rename = "check-crate")]
    pub check_crate: CheckCrateConfig,
    /// `[packager]` table: who is asking, and what they may do.
    #[serde(default)]
    pub packager: PackagerConfig,
    /// `[rpmfusion-bugzilla]` table: RPM Fusion runs its own
    /// Bugzilla, so coordinating with them needs its own account.
    #[serde(default, rename = "rpmfusion-bugzilla")]
    pub rpmfusion_bugzilla: RpmfusionBugzillaConfig,
    /// `[forgejo]` table: Fedora's releng tracker moved to Forgejo,
    /// and a stalled branch request ends there.
    #[serde(default)]
    pub forgejo: ForgejoConfig,
    /// `[matrix-rooms]` table: FAS group → Matrix room, added to the
    /// built-in list or overriding its entries.
    #[serde(
        default,
        rename = "matrix-rooms",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub matrix_rooms: std::collections::BTreeMap<String, String>,
}

/// Fedora's Forgejo, where the releng ticket for a stalled request
/// goes. The URL is configurable so the same code works against a
/// test instance, but it defaults to the real one.
#[derive(Debug, Deserialize, Serialize)]
pub struct ForgejoConfig {
    #[serde(default)]
    pub token: String,
    #[serde(default = "default_forge_url")]
    pub url: String,
}

fn default_forge_url() -> String {
    "https://forge.fedoraproject.org".to_string()
}

impl Default for ForgejoConfig {
    fn default() -> Self {
        Self {
            token: String::new(),
            url: default_forge_url(),
        }
    }
}

/// RPM Fusion's Bugzilla, where counterpart rebuild requests go.
///
/// A separate instance from Red Hat's, running a stock Bugzilla 5
/// whose REST API hangs off `rest.cgi` rather than `rest`, so it needs
/// its own URL, path and key rather than borrowing `[bugzilla]`.
#[derive(Debug, Deserialize, Serialize)]
pub struct RpmfusionBugzillaConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default = "default_rpmfusion_url")]
    pub url: String,
    #[serde(default = "default_rpmfusion_rest_path")]
    pub rest_path: String,
}

fn default_rpmfusion_url() -> String {
    "https://bugzilla.rpmfusion.org".to_string()
}

fn default_rpmfusion_rest_path() -> String {
    "rest.cgi".to_string()
}

impl Default for RpmfusionBugzillaConfig {
    fn default() -> Self {
        RpmfusionBugzillaConfig {
            api_key: String::new(),
            url: default_rpmfusion_url(),
            rest_path: default_rpmfusion_rest_path(),
        }
    }
}

/// RPM Fusion's Bugzilla as configured, when a key is set. `None`
/// means "do not file", which is how an unconfigured run stays
/// read-only rather than failing.
pub fn rpmfusion_bugzilla() -> Option<RpmfusionBugzillaConfig> {
    let mut cfg = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .unwrap_or_default()
        .rpmfusion_bugzilla;
    cfg.api_key = crate::secrets::resolve(None, &crate::secrets::bugzilla(&cfg.url), &cfg.api_key);
    (!cfg.api_key.is_empty()).then_some(cfg)
}

/// Who is running this, and what they may do beyond what a project's
/// own access list says.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct PackagerConfig {
    /// FAS username, the default for `--fas`. It is what dist-git is
    /// asked about when deciding whether a branch request is a
    /// question for somebody else, and what a co-maintainer offer
    /// names.
    #[serde(default)]
    pub fas: String,
    /// Whether they hold provenpackager.
    ///
    /// Declared rather than looked up, and deliberately: a
    /// provenpackager may build any package but may not branch one,
    /// and Fedora's convention is that the build right is for
    /// emergencies rather than for getting on with things. So this
    /// only ever adds a note to a request for a build that is already
    /// branched, naming it as the exception it is. It never offers,
    /// never acts, and has nothing to do with branching.
    #[serde(default)]
    pub provenpackager: bool,
}

/// NVD configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct NvdConfig {
    #[serde(default)]
    pub api_key: String,
}

/// The NVD API key, empty when there is none: the keyring's, else the
/// layered config's, so a key shipped system-wide counts.
pub fn nvd_api_key() -> String {
    let file = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.nvd.api_key)
        .unwrap_or_default();
    crate::secrets::resolve(None, &crate::secrets::nvd(), &file)
}

/// Standing `check-crate` preferences.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct CheckCrateConfig {
    /// Crates to ignore in every run — direct or transitive — as if
    /// they were not dependencies at all. Fedora almost always drops
    /// some upstream dependencies (benchmark harnesses like
    /// `criterion`, say), so listing them here keeps every report
    /// honest about what will actually be packaged. Merged with
    /// `--exclude`. Unset, [`DEFAULT_EXCLUDES`] applies; a list set
    /// here replaces it (`exclude = []` excludes nothing), and the
    /// entry [`DEFAULT_SENTINEL`] inside it stands for that set, so
    /// crates can be added without retyping it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<String>>,
    /// Per-crate `--in-tree` lists, keyed by crate name: the globs
    /// (and `@repository`) naming the crates a package builds from
    /// its own tree, so `check-crate coreutils` knows about `uu_*`
    /// without being told each run. Merged with `--in-tree`.
    #[serde(
        default,
        rename = "in-tree",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub in_tree: std::collections::BTreeMap<String, Vec<String>>,
    /// Per-crate staging COPR (`owner/project`), keyed by crate name:
    /// `--copr` without typing it, for a crate whose update is
    /// staged there.
    #[serde(
        default,
        rename = "copr",
        skip_serializing_if = "std::collections::BTreeMap::is_empty"
    )]
    pub copr: std::collections::BTreeMap<String, String>,
}

/// The benchmark harnesses `check-crate` ignores unless the config
/// file sets its own `exclude` list: Fedora drops them from every
/// build, so they are never something to package.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "codspeed",
    "codspeed-bencher-compat",
    "codspeed-criterion-compat",
    "codspeed-divan-compat",
    "count_instructions",
    "criterion",
    "criterion2",
    "divan",
    "iai",
    "iai-callgrind",
];

/// The list entry that expands to [`DEFAULT_EXCLUDES`]: TOML has no
/// `+=`, so `exclude = ["@default", "pretty_assertions"]` is how a
/// config adds to the built-in set instead of replacing it. `@` cannot
/// start a crate name, so it cannot collide.
pub const DEFAULT_SENTINEL: &str = "@default";

/// The effective exclude list and whether the config file set it
/// (`false`: [`DEFAULT_EXCLUDES`] is in force on its own).
pub fn resolve_excludes(configured: Option<Vec<String>>) -> (Vec<String>, bool) {
    let defaults = || DEFAULT_EXCLUDES.iter().map(|s| s.to_string());
    match configured {
        Some(list) => (
            list.into_iter()
                .flat_map(|e| {
                    if e == DEFAULT_SENTINEL {
                        defaults().collect::<Vec<_>>()
                    } else {
                        vec![e]
                    }
                })
                .collect(),
            true,
        ),
        None => (defaults().collect(), false),
    }
}

/// The `[check-crate]` table as configured (system file beneath user
/// file, as usual); default when there is no file.
fn load_check_crate() -> CheckCrateConfig {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.check_crate)
        .unwrap_or_default()
}

/// The crates `check-crate` ignores: the config file's list, or
/// [`DEFAULT_EXCLUDES`] when no file sets one. The flag says which.
pub fn check_crate_excludes() -> (Vec<String>, bool) {
    resolve_excludes(load_check_crate().exclude)
}

/// The configured staging COPR for a crate, if any.
pub fn check_crate_copr(crate_name: &str) -> Option<String> {
    load_check_crate().copr.remove(crate_name)
}

/// The configured `--in-tree` list for a crate; empty when none.
pub fn check_crate_in_tree(crate_name: &str) -> Vec<String> {
    load_check_crate()
        .in_tree
        .remove(crate_name)
        .unwrap_or_default()
}

/// Bugzilla configuration.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BugzillaConfig {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub url: String,
    /// The Bugzilla account to assign a request to when it is the
    /// asker's own bookkeeping rather than something to ask of a
    /// maintainer.
    #[serde(default)]
    pub email: String,
}

/// Fedora's Forgejo, for the releng ticket a stalled request ends in.
/// An absent config is not an error: the token is only needed for the
/// last step of the escalation, and `escalate` reports that step
/// rather than failing when it cannot take it.
pub fn forgejo() -> ForgejoConfig {
    let mut forge = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.forgejo)
        .unwrap_or_default();
    forge.token = crate::secrets::resolve(None, &crate::secrets::forgejo(&forge.url), &forge.token);
    forge
}

/// Everything `escalate` needs of a new Forgejo token, asked of the
/// releng tracker without writing anything there: reading and writing
/// its issues, and resolving the labels a ticket is filed with, some
/// of which belong to the organisation and need its scope. The first
/// refusal is the answer.
fn new_token_access(url: &str, token: &str) -> Result<sandogasa_forgejo::IssueAccess, String> {
    use sandogasa_forgejo::IssueAccess;
    let (owner, repo) = crate::branch_request::RELENG_REPO;
    let issues = sandogasa_forgejo::check_issue_access(url, token, owner, repo)
        .map_err(|e| e.to_string())?;
    if issues != IssueAccess::Granted {
        return Ok(issues);
    }
    let org = sandogasa_forgejo::check_org_read(url, token, owner).map_err(|e| e.to_string())?;
    if org != IssueAccess::Granted {
        return Ok(org);
    }
    // The labels themselves, resolved the way filing resolves them: a
    // name the tracker no longer has is not the token's fault, but it
    // would fail every ticket, so it is said now.
    sandogasa_forgejo::Client::new(url, token)
        .and_then(|c| c.label_ids(owner, repo, &crate::branch_request::RELENG_LABELS))
        .map_err(|e| e.to_string())?;
    Ok(IssueAccess::Granted)
}

/// Ask for the Forgejo token `escalate` files releng tickets with,
/// check it against `forge.url`, and keep the answer in `forge`.
/// Optional: an empty answer leaves escalate reporting the step
/// instead of taking it.
pub async fn ask_forgejo_token(forge: &mut ForgejoConfig, review: bool) -> Result<(), String> {
    use sandogasa_cli::credential::{KeyVerdict, validated_key};
    use sandogasa_config::{Setting, ask};
    let hint = format!(
        "\nescalate files a releng ticket for a branch request whose maintainers \
         have not answered. Create a token at {}/user/settings/applications with \
         the issue scope at Read and Write (to search and file tickets) and the \
         organization scope at Read (for the tracker's organisation-wide labels), \
         or leave empty to skip.",
        forge.url.trim_end_matches('/')
    );
    let saved = forge.token.clone();
    forge.token = ask(
        &Setting::new("Fedora Forgejo", "token (optional, empty to skip)")
            .sensitive()
            .optional()
            .hint(&hint),
        &forge.token,
        review,
    )
    .map_err(|e| format!("failed to read Forgejo token: {e}"))?
    .trim()
    .to_string();
    if forge.token.is_empty() {
        return Ok(());
    }
    let url = forge.url.clone();
    forge.token = validated_key("Fedora Forgejo", "token", forge.token.clone(), move |k| {
        let url = url.clone();
        // A new token is checked for everything escalate needs. One
        // kept from before passed that when it was entered, and scopes
        // do not change, so it is only checked for still working:
        // reading, not the write probe.
        let full = k != saved;
        Box::pin(async move {
            use sandogasa_forgejo::IssueAccess;
            let (owner, repo) = crate::branch_request::RELENG_REPO;
            let checked = tokio::task::spawn_blocking(move || {
                if full {
                    new_token_access(&url, &k)
                } else {
                    sandogasa_forgejo::check_issue_read(&url, &k, owner, repo)
                        .map_err(|e| e.to_string())
                }
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match checked {
                Ok(IssueAccess::Granted) if full => {
                    KeyVerdict::Accepted(format!("may search and file on {owner}/{repo}."))
                }
                Ok(IssueAccess::Granted) => KeyVerdict::Accepted("still accepted.".to_string()),
                Ok(IssueAccess::Refused) => KeyVerdict::Refused(
                    "refused (401): the token is mistyped, expired or revoked.".to_string(),
                ),
                // A scope missing is a fact about the token, not a
                // failure to check it, so it is worth replacing.
                Ok(IssueAccess::MissingScope(why)) => {
                    KeyVerdict::Refused(format!("lacks a scope it needs (403): {why}"))
                }
                Ok(_) => KeyVerdict::Unknown("could not tell.".to_string()),
                Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
            }
        })
    })
    .await
    .key;
    Ok(())
}

/// Ask for the Forgejo token now and keep it, in the keyring when
/// there is one and else in the user's config file, for `escalate`
/// finding a releng ticket due and no token to file it with. `url` is
/// the Forgejo the run is using. The token, or `None` when none was
/// given.
pub async fn ask_and_save_forgejo_token(url: &str) -> Result<Option<String>, String> {
    let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
    let mut config: EbranchConfig = cf.load_user().unwrap_or_default();
    // Checked against the Forgejo the run uses, which may come from
    // the system-wide file; that URL is not copied into the user's.
    let own_url = std::mem::replace(&mut config.forgejo.url, url.to_string());
    ask_forgejo_token(&mut config.forgejo, false).await?;
    let token = std::mem::take(&mut config.forgejo.token);
    if token.is_empty() {
        return Ok(None);
    }
    // Kept for the Forgejo it was checked against.
    config.forgejo.token = crate::secrets::keep_for_file(
        "Forgejo token",
        &crate::secrets::forgejo(url),
        token.clone(),
    );
    config.forgejo.url = own_url;
    save_with_secrets(&cf, &mut config, &["forgejo"])?;
    if !config.forgejo.token.is_empty() {
        println!("Saved the token to {}", cf.path().display());
    }
    Ok(Some(token))
}

/// The Bugzilla account to claim bookkeeping requests as, if set.
pub fn bugzilla_email() -> Option<String> {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .ok()
        .map(|c| c.bugzilla.email)
        .filter(|e| !e.is_empty())
}

/// FAS group → Matrix room: the built-in list from
/// `data/matrix-rooms.toml`, with the config's `[matrix-rooms]` laid
/// over it.
pub fn matrix_rooms() -> std::collections::BTreeMap<String, String> {
    let configured = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.matrix_rooms)
        .unwrap_or_default();
    with_builtin_rooms(configured)
}

/// Whether the config declares the asker a provenpackager.
pub fn is_provenpackager() -> bool {
    packager().provenpackager
}

/// The configured FAS username, if any: the fallback for `--fas`.
pub fn packager_fas() -> Option<String> {
    Some(packager().fas).filter(|f| !f.is_empty())
}

/// The `[packager]` table as configured.
fn packager() -> PackagerConfig {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.packager)
        .unwrap_or_default()
}

/// Whether dist-git lists `fas` in the provenpackager group.
///
/// Read from dist-git's mirror of the FAS group rather than from
/// FASJSON, which would need a Kerberos ticket to answer a question
/// asked once during setup. The mirror can lag, so the answer is
/// offered as the default of a prompt rather than written straight to
/// the file, and `None` means the lookup failed, not that the answer
/// is no.
async fn looks_like_provenpackager(fas: &str) -> Option<bool> {
    sandogasa_distgit::DistGitClient::new()
        .get_group_members("provenpackager")
        .await
        .ok()
        .map(|members| members.iter().any(|m| m == fas))
}

/// Load the API key for the Bugzilla at `bugzilla_url`, checking (in
/// order):
/// 1. `--api-key` CLI flag
/// 2. `BUGZILLA_API_KEY` environment variable
/// 3. the keyring's entry for that Bugzilla
/// 4. `~/.config/ebranch/config.toml`
///
/// Returns an error with setup instructions if none found.
pub fn resolve_api_key(cli_key: Option<&str>, bugzilla_url: &str) -> Result<String, String> {
    if let Some(key) = cli_key
        && !key.is_empty()
    {
        return Ok(key.to_string());
    }

    let file = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.bugzilla.api_key)
        .unwrap_or_default();
    let key = crate::secrets::resolve(
        Some("BUGZILLA_API_KEY"),
        &crate::secrets::bugzilla(bugzilla_url),
        &file,
    );
    if !key.is_empty() {
        return Ok(key);
    }

    Err("Bugzilla API key not found.\n\
         Set it up with: ebranch config\n\
         Or pass --api-key or set BUGZILLA_API_KEY."
        .to_string())
}

/// Interactive config setup: asks for what is missing, or with `all`
/// for every setting, with the saved value as the default.
pub async fn cmd_config(args: &sandogasa_cli::ConfigArgs) -> Result<(), String> {
    use sandogasa_config::{Setting, ask};
    let review = args.all;
    let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
    let mut config: EbranchConfig = cf.load_user().unwrap_or_default();

    println!("ebranch configuration\n");
    println!("Config file: {}\n", cf.path().display());

    // Bugzilla URL.
    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = "https://bugzilla.redhat.com".to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);
    // Secrets kept in the keyring are the saved ones (#79).
    let had = secrets_from_keyring(&mut config);

    // API key.
    config.bugzilla.api_key = ask(
        &Setting::new("Bugzilla", "API key").sensitive().hint(
            "\nGenerate an API key at:\n  \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey\n",
        ),
        &config.bugzilla.api_key,
        review,
    )
    .map_err(|e| format!("failed to read API key: {e}"))?;

    let url = config.bugzilla.url.clone();
    config.bugzilla.api_key = sandogasa_cli::credential::validated_key(
        "Bugzilla",
        "API key",
        config.bugzilla.api_key.clone(),
        move |k| {
            let url = url.clone();
            Box::pin(async move {
                let bz = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(k) {
                    Ok(bz) => bz,
                    Err(e) => {
                        return sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                            "could not check: {e}"
                        ));
                    }
                };
                match bz.check_api_key().await {
                    Ok(sandogasa_bugzilla::KeyCheck::Accepted) => {
                        sandogasa_cli::credential::KeyVerdict::Accepted("accepted.".to_string())
                    }
                    Ok(sandogasa_bugzilla::KeyCheck::Refused(why)) => {
                        sandogasa_cli::credential::KeyVerdict::Refused(format!("refused: {why}"))
                    }
                    Ok(sandogasa_bugzilla::KeyCheck::Unclear(why)) => {
                        sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                            "could not tell: {why}"
                        ))
                    }
                    Ok(_) => sandogasa_cli::credential::KeyVerdict::Unknown(
                        "could not tell.".to_string(),
                    ),
                    Err(e) => sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                        "could not check: {e}"
                    )),
                }
            })
        },
    )
    .await
    .key;

    // Bugzilla account: optional, and only ever used to assign a
    // request the asker files about their own work to themselves.
    config.bugzilla.email = ask(
        &Setting::new(
            "Bugzilla",
            "account e-mail (optional, to claim your own requests)",
        )
        .optional()
        .validated(&sandogasa_config::validate_email),
        &config.bugzilla.email,
        review,
    )
    .map_err(|e| format!("failed to read e-mail: {e}"))?;

    // FAS: who is asking. Everything dist-git is asked about a
    // package needs this, so it is worth having in the file rather
    // than on every command line.
    config.packager.fas = ask(
        &Setting::new("FAS", "username (optional, the default for --fas)").optional(),
        &config.packager.fas,
        review,
    )
    .map_err(|e| format!("failed to read FAS username: {e}"))?;

    // Provenpackager: looked up rather than asked cold, since few
    // people think of themselves in those terms day to day. The
    // lookup only sets the default; the answer is the packager's.
    if !config.packager.fas.is_empty() {
        print!("Checking provenpackager membership... ");
        let found = looks_like_provenpackager(&config.packager.fas).await;
        match found {
            Some(true) => println!("{} is a provenpackager.", config.packager.fas),
            Some(false) => println!("not a member."),
            None => println!("could not ask dist-git."),
        }
        config.packager.provenpackager = sandogasa_cli::confirm(
            "Note on build requests that you could build a branched package \
             yourself in an emergency?",
            found.unwrap_or(config.packager.provenpackager),
        )
        .map_err(|e| format!("failed to read answer: {e}"))?;
    }

    // NVD API key: optional, for judging CVE trackers against the
    // fix NVD records.
    config.nvd.api_key = ask(
        &Setting::new("NVD", "API key (optional, empty to skip)")
            .sensitive()
            .optional()
            .hint(
                "\nAn NVD API key raises the rate limit from 5 to 50 requests per 30 s \
                 when check-update judges CVE bugs. Request one free at \
                 https://nvd.nist.gov/developers/request-an-api-key, or leave empty to skip.",
            ),
        &config.nvd.api_key,
        review,
    )
    .map_err(|e| format!("failed to read NVD API key: {e}"))?;
    if !config.nvd.api_key.is_empty() {
        config.nvd.api_key = sandogasa_cli::credential::validated_key("NVD", "API key", config.nvd.api_key.clone(), |k| {
            Box::pin(async move {
                use sandogasa_cve::cache::KeyCheck;
                match sandogasa_cve::cache::check_api_key(&k).await {
                    KeyCheck::Accepted => sandogasa_cli::credential::KeyVerdict::Accepted(
                        "valid; NVD lookups pace at 0.6 s instead of 6 s.".to_string(),
                    ),
                    KeyCheck::Refused => sandogasa_cli::credential::KeyVerdict::Refused(
                        "not accepted (404): NVD does not know this key — it is mistyped, \
                         or not yet activated; the activation link arrives by mail after \
                         the request."
                            .to_string(),
                    ),
                    // Rate limiting says nothing about the key, so
                    // offering to replace it would be wrong.
                    KeyCheck::Throttled(status) => sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                        "NVD refused the request ({status}) — rate limited; try again in a minute"
                    )),
                    KeyCheck::Unreachable(e) => {
                        sandogasa_cli::credential::KeyVerdict::Unknown(format!("could not check: {e}"))
                    }
                }
            })
        })
        .await.key;
    }

    // Fedora's Forgejo: only escalate's last step, the releng ticket
    // for a request nobody answered, needs it.
    ask_forgejo_token(&mut config.forgejo, review).await?;

    // RPM Fusion's Bugzilla: a separate instance with a separate
    // account, asked for last because it is only needed by someone
    // who maintains a package RPM Fusion also builds. Skipping it
    // leaves check-update holding auto-push and filing nothing,
    // which is the safe half of the guard.
    let hint = format!(
        "\nRPM Fusion runs its own Bugzilla. A key there lets check-update \
         file the rebuild request for a counterpart (libheif -> \
         libheif-freeworld) instead of only holding the update's auto-push. \
         Generate one at {}/userprefs.cgi?tab=apikey, or leave empty to skip.",
        config.rpmfusion_bugzilla.url
    );
    config.rpmfusion_bugzilla.api_key = ask(
        &Setting::new("RPM Fusion Bugzilla", "API key (optional, empty to skip)")
            .sensitive()
            .optional()
            .hint(&hint),
        &config.rpmfusion_bugzilla.api_key,
        review,
    )
    .map_err(|e| format!("failed to read RPM Fusion API key: {e}"))?;
    // Two checks, because they catch different things. The shape
    // check is local and names the actual mistake: a hidden prompt
    // shows nothing when a paste lands twice, and the result is a key
    // of exactly double length. The server check that follows is what
    // the old one should have been — searching proves nothing on an
    // instance that serves searches to anyone, which is how a key
    // pasted three times was reported "valid".
    let key = config.rpmfusion_bugzilla.api_key.trim().to_string();
    if !key.is_empty() && !sandogasa_bugzilla::looks_like_api_key(&key) {
        eprintln!(
            "warning: that does not look like a Bugzilla API key — they are 40 \
             alphanumeric characters, this is {}. A hidden prompt shows nothing \
             when a paste lands twice. Saving it anyway; rerun to replace it.",
            key.len()
        );
    }
    config.rpmfusion_bugzilla.api_key = key.clone();
    if !key.is_empty() {
        let url = config.rpmfusion_bugzilla.url.clone();
        let rest = config.rpmfusion_bugzilla.rest_path.clone();
        config.rpmfusion_bugzilla.api_key = sandogasa_cli::credential::validated_key(
            "RPM Fusion Bugzilla",
            "API key",
            key,
            move |k| {
                let (url, rest) = (url.clone(), rest.clone());
                Box::pin(async move {
                    let bz = match sandogasa_bugzilla::BzClient::new(&url)
                        .with_rest_path(&rest)
                        .with_stock_auth()
                        .with_api_key(k)
                    {
                        Ok(bz) => bz,
                        Err(e) => {
                            return sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                                "could not check: {e}"
                            ));
                        }
                    };
                    match bz.check_api_key().await {
                        Ok(sandogasa_bugzilla::KeyCheck::Accepted) => {
                            sandogasa_cli::credential::KeyVerdict::Accepted("accepted.".to_string())
                        }
                        Ok(sandogasa_bugzilla::KeyCheck::Refused(why)) => {
                            sandogasa_cli::credential::KeyVerdict::Refused(format!(
                                "refused: {why}"
                            ))
                        }
                        Ok(sandogasa_bugzilla::KeyCheck::Unclear(why)) => {
                            sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                                "could not tell: {why}"
                            ))
                        }
                        Ok(_) => sandogasa_cli::credential::KeyVerdict::Unknown(
                            "could not tell.".to_string(),
                        ),
                        Err(e) => sandogasa_cli::credential::KeyVerdict::Unknown(format!(
                            "could not check: {e}"
                        )),
                    }
                })
            },
        )
        .await
        .key;
    }

    // Into the keyring when there is one, out of the file then.
    secrets_to_keyring(&mut config, &had);
    if save_with_secrets(&cf, &mut config, &ALL_TABLES)? {
        println!("\nConfig saved to {}", cf.path().display());
    } else {
        println!(
            "\nNothing else to keep in {}; removed it.",
            cf.path().display()
        );
    }

    Ok(())
}

/// The tables of [`secret_fields`], all of them.
const ALL_TABLES: [&str; 4] = ["bugzilla", "nvd", "forgejo", "rpmfusion-bugzilla"];

/// One secret the config file can hold: its table and key there, what
/// to call it, its keyring entry, and its value.
struct SecretField<'a> {
    table: &'static str,
    key: &'static str,
    what: &'static str,
    entry: sandogasa_config::secret::SecretRef,
    value: &'a mut String,
}

/// Every secret the config file can hold, each with the entry for the
/// service its table points at.
fn secret_fields(config: &mut EbranchConfig) -> [SecretField<'_>; 4] {
    use crate::secrets;
    [
        SecretField {
            table: "bugzilla",
            key: "api_key",
            what: "Bugzilla API key",
            entry: secrets::bugzilla(&config.bugzilla.url),
            value: &mut config.bugzilla.api_key,
        },
        SecretField {
            table: "nvd",
            key: "api_key",
            what: "NVD API key",
            entry: secrets::nvd(),
            value: &mut config.nvd.api_key,
        },
        SecretField {
            table: "forgejo",
            key: "token",
            what: "Forgejo token",
            entry: secrets::forgejo(&config.forgejo.url),
            value: &mut config.forgejo.token,
        },
        SecretField {
            table: "rpmfusion-bugzilla",
            key: "api_key",
            what: "RPM Fusion Bugzilla API key",
            entry: secrets::bugzilla(&config.rpmfusion_bugzilla.url),
            value: &mut config.rpmfusion_bugzilla.api_key,
        },
    ]
}

/// Fill each secret from the keyring, which wins over the file's
/// value: the saved values `config` offers as defaults. A secret turned
/// off for ebranch ([`sandogasa_config::secret::OFF`]) stays off, so
/// keeping the default keeps it off. Which secrets had a value, so
/// [`secrets_to_keyring`] can tell one cleared from one never set.
fn secrets_from_keyring(config: &mut EbranchConfig) -> Vec<bool> {
    secret_fields(config)
        .into_iter()
        .map(|f| {
            if f.value != sandogasa_config::secret::OFF {
                *f.value = crate::secrets::resolve(None, &f.entry, f.value);
            }
            !f.value.is_empty()
        })
        .collect()
}

/// Keep each secret in the keyring when there is one, leaving the
/// file's value empty then. One that had a value (`had`, from
/// [`secrets_from_keyring`]) and was cleared is turned off for ebranch:
/// the file says [`sandogasa_config::secret::OFF`], and the shared
/// keyring entry is kept for the tools that use it.
fn secrets_to_keyring(config: &mut EbranchConfig, had: &[bool]) {
    for (f, had) in secret_fields(config).into_iter().zip(had) {
        if f.value.is_empty() && *had {
            if let Some(note) = sandogasa_config::secret::kept_on_clear(
                crate::secrets::keyring(),
                env!("CARGO_PKG_NAME"),
                f.what,
                &f.entry,
            ) {
                eprintln!("{note}");
            }
            *f.value = sandogasa_config::secret::OFF.to_string();
            continue;
        }
        *f.value = crate::secrets::keep_for_file(f.what, &f.entry, std::mem::take(f.value));
    }
}

/// Save `config` to the user file, then take out of it every secret
/// of the `tables` named that is empty: kept in the keyring, or
/// cleared. `save` lays the struct over the file and never removes a
/// key, so a token moved to the keyring would otherwise stay there in
/// plain text. Whether the file is still there.
fn save_with_secrets(
    cf: &sandogasa_config::ConfigFile,
    config: &mut EbranchConfig,
    tables: &[&str],
) -> Result<bool, String> {
    cf.save(&*config)
        .map_err(|e| format!("failed to save config: {e}"))?;
    let mut gone = false;
    for f in secret_fields(config) {
        if f.value.is_empty() && tables.contains(&f.table) {
            gone |= cf
                .remove(f.table, f.key)
                .map_err(|e| format!("failed to update config: {e}"))?;
        }
    }
    Ok(!gone)
}

/// The built-in rooms with `configured` laid over them.
fn with_builtin_rooms(
    configured: std::collections::BTreeMap<String, String>,
) -> std::collections::BTreeMap<String, String> {
    let mut rooms: std::collections::BTreeMap<String, String> =
        toml::from_str(include_str!("../data/matrix-rooms.toml"))
            .expect("the built-in Matrix room list parses");
    rooms.extend(configured);
    rooms
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_rooms_add_to_and_override_the_built_in_ones() {
        let rooms = with_builtin_rooms(std::collections::BTreeMap::from([
            ("kde-sig".to_string(), "#kde-pim:example.org".to_string()),
            (
                "perl-sig".to_string(),
                "#perl:fedoraproject.org".to_string(),
            ),
        ]));
        assert_eq!(rooms["gnome-sig"], "#workstation:fedoraproject.org");
        assert_eq!(rooms["kde-sig"], "#kde-pim:example.org");
        assert_eq!(rooms["perl-sig"], "#perl:fedoraproject.org");
    }

    #[test]
    fn a_matrix_rooms_table_is_read_from_the_config() {
        let config: EbranchConfig =
            toml::from_str("[matrix-rooms]\nperl-sig = \"#perl:fedoraproject.org\"\n").unwrap();
        assert_eq!(config.matrix_rooms["perl-sig"], "#perl:fedoraproject.org");
    }

    /// A check that answers whatever the test says, without a
    /// network or a terminal.
    fn answering(
        verdict: fn(String) -> sandogasa_cli::credential::KeyVerdict,
    ) -> impl Fn(
        String,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = sandogasa_cli::credential::KeyVerdict>>,
    > {
        move |k| Box::pin(async move { verdict(k) })
    }

    #[tokio::test]
    async fn a_key_the_service_takes_is_kept() {
        let kept = sandogasa_cli::credential::validated_key(
            "Test",
            "API key",
            "good".to_string(),
            answering(|_| sandogasa_cli::credential::KeyVerdict::Accepted("accepted.".into())),
        )
        .await
        .key;
        assert_eq!(kept, "good");
    }

    #[tokio::test]
    async fn a_key_is_kept_when_the_check_could_not_be_made() {
        // A rate limit or an unreachable service says nothing about
        // the key, so it must not be treated as a refusal — nothing
        // is offered and nothing is discarded.
        let kept = sandogasa_cli::credential::validated_key(
            "Test",
            "API key",
            "maybe".to_string(),
            answering(|_| {
                sandogasa_cli::credential::KeyVerdict::Unknown("could not check.".into())
            }),
        )
        .await
        .key;
        assert_eq!(kept, "maybe");
    }

    #[tokio::test]
    async fn a_refused_key_is_kept_when_there_is_nobody_to_ask() {
        // Non-interactively the offer cannot be made, so the value
        // stands and the run says so rather than prompting into a
        // pipe or discarding what the user configured.
        let kept = sandogasa_cli::credential::validated_key(
            "Test",
            "API key",
            "bad".to_string(),
            answering(|_| sandogasa_cli::credential::KeyVerdict::Refused("refused.".into())),
        )
        .await
        .key;
        assert_eq!(kept, "bad");
    }

    #[test]
    fn rpm_fusion_bugzilla_defaults_to_their_instance_and_rest_cgi() {
        // An absent table still points at RPM Fusion rather than at
        // Red Hat's Bugzilla, so only the key has to be configured.
        let bare: EbranchConfig = toml::from_str("").unwrap();
        assert_eq!(
            bare.rpmfusion_bugzilla.url,
            "https://bugzilla.rpmfusion.org"
        );
        assert_eq!(bare.rpmfusion_bugzilla.rest_path, "rest.cgi");
        assert!(bare.rpmfusion_bugzilla.api_key.is_empty());
        // A key alone is enough; the other two keep their defaults.
        let keyed: EbranchConfig = toml::from_str(
            r#"
            [rpmfusion-bugzilla]
            api_key = "secret"
            "#,
        )
        .unwrap();
        assert_eq!(keyed.rpmfusion_bugzilla.api_key, "secret");
        assert_eq!(keyed.rpmfusion_bugzilla.rest_path, "rest.cgi");
        // And a stock instance elsewhere can override both.
        let other: EbranchConfig = toml::from_str(
            r#"
            [rpmfusion-bugzilla]
            api_key = "k"
            url = "https://bz.example"
            rest_path = "rest"
            "#,
        )
        .unwrap();
        assert_eq!(other.rpmfusion_bugzilla.url, "https://bz.example");
        assert_eq!(other.rpmfusion_bugzilla.rest_path, "rest");
        // The table is spelled with a dash, not an underscore: a
        // mis-spelled table is silently ignored rather than applied,
        // which would leave filing unconfigured with no sign why.
        let dashed = toml::to_string(&other).unwrap();
        assert!(dashed.contains("[rpmfusion-bugzilla]"), "{dashed}");
    }

    /// A user config file in a fresh `$XDG_CONFIG_HOME`, holding every
    /// secret for test hosts no other test uses.
    fn config_with_secrets(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("ebranch/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"
            [bugzilla]
            api_key = "bz-from-file"
            url = "https://bugzilla.keyring.test"
            [nvd]
            api_key = "nvd-from-file"
            [forgejo]
            token = "forge-from-file"
            url = "https://forge.keyring.test"
            [rpmfusion-bugzilla]
            api_key = "rpmfusion-from-file"
            url = "https://bugzilla.rpmfusion.keyring.test"
            [packager]
            fas = "someone"
            "#,
        )
        .unwrap();
        path
    }

    /// Clearing the RPM Fusion key at the prompt turns it off for
    /// ebranch, which goes back to reading RPM Fusion only, though the
    /// shared keyring entry stays for the tools that use it; and the
    /// next `config` run keeps it off.
    #[test]
    #[serial_test::serial]
    fn a_secret_cleared_at_the_prompt_is_turned_off_for_ebranch() {
        use crate::secrets::test_support::EnvGuard;
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[("XDG_CONFIG_HOME", &dir.path().to_string_lossy())]);
        let path = config_with_secrets(dir.path());
        let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
        // A first run moves the RPM Fusion key into the keyring.
        let mut config: EbranchConfig = cf.load_user().unwrap();
        let had = secrets_from_keyring(&mut config);
        secrets_to_keyring(&mut config, &had);
        save_with_secrets(&cf, &mut config, &ALL_TABLES).unwrap();
        assert_eq!(rpmfusion_bugzilla().unwrap().api_key, "rpmfusion-from-file");

        // Cleared at the next run's prompt.
        let mut config: EbranchConfig = cf.load_user().unwrap();
        let had = secrets_from_keyring(&mut config);
        config.rpmfusion_bugzilla.api_key.clear();
        secrets_to_keyring(&mut config, &had);
        save_with_secrets(&cf, &mut config, &ALL_TABLES).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("api_key = \"-\""), "{text}");
        assert!(rpmfusion_bugzilla().is_none(), "read-only again");
        let entry = crate::secrets::bugzilla("https://bugzilla.rpmfusion.keyring.test");
        assert_eq!(
            crate::secrets::keyring().lookup(&entry).unwrap().as_deref(),
            Some("rpmfusion-from-file"),
            "the shared entry is kept"
        );

        // Kept as the default at a later run, it stays off.
        let mut config: EbranchConfig = cf.load_user().unwrap();
        let had = secrets_from_keyring(&mut config);
        assert_eq!(
            config.rpmfusion_bugzilla.api_key,
            sandogasa_config::secret::OFF
        );
        secrets_to_keyring(&mut config, &had);
        save_with_secrets(&cf, &mut config, &ALL_TABLES).unwrap();
        assert!(rpmfusion_bugzilla().is_none());
        for e in [
            crate::secrets::bugzilla("https://bugzilla.keyring.test"),
            crate::secrets::nvd(),
            crate::secrets::forgejo("https://forge.keyring.test"),
            entry,
        ] {
            crate::secrets::keyring().clear(&e).unwrap();
        }
    }

    /// Secrets `config` keeps in the keyring are the ones the tool then
    /// uses, the file is left without them, and the environment still
    /// wins.
    #[test]
    #[serial_test::serial]
    fn secrets_kept_in_the_keyring_are_the_ones_used() {
        use crate::secrets::test_support::EnvGuard;
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            // Empty is unset, to resolve().
            ("BUGZILLA_API_KEY", ""),
        ]);
        let path = config_with_secrets(dir.path());
        let bz = "https://bugzilla.keyring.test";
        // Before: the file's, as ever.
        assert_eq!(resolve_api_key(None, bz).unwrap(), "bz-from-file");
        assert_eq!(nvd_api_key(), "nvd-from-file");
        assert_eq!(forgejo().token, "forge-from-file");
        assert_eq!(rpmfusion_bugzilla().unwrap().api_key, "rpmfusion-from-file");

        // What `config` does once it has the answers.
        let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
        let mut config: EbranchConfig = cf.load_user().unwrap();
        config.bugzilla.api_key = "bz-new".into();
        config.nvd.api_key = "nvd-new".into();
        config.forgejo.token = "forge-new".into();
        config.rpmfusion_bugzilla.api_key = "rpmfusion-new".into();
        // Nothing cleared: each was replaced.
        secrets_to_keyring(&mut config, &[true; 4]);
        assert!(save_with_secrets(&cf, &mut config, &ALL_TABLES).unwrap());

        let text = std::fs::read_to_string(&path).unwrap();
        for gone in ["api_key", "token", "-new", "-from-file", "[nvd]"] {
            assert!(!text.contains(gone), "{gone} left in:\n{text}");
        }
        // What is not a secret stays.
        assert!(text.contains("https://forge.keyring.test"), "{text}");
        assert!(text.contains("fas = \"someone\""), "{text}");

        assert_eq!(resolve_api_key(None, bz).unwrap(), "bz-new");
        assert_eq!(nvd_api_key(), "nvd-new");
        assert_eq!(forgejo().token, "forge-new");
        assert_eq!(rpmfusion_bugzilla().unwrap().api_key, "rpmfusion-new");
        // And `config` offers them as the saved values.
        let mut again: EbranchConfig = cf.load_user().unwrap();
        secrets_from_keyring(&mut again);
        assert_eq!(again.bugzilla.api_key, "bz-new");
        assert_eq!(again.rpmfusion_bugzilla.api_key, "rpmfusion-new");

        // The environment still wins, and the flag over that.
        let _env2 = EnvGuard::new(&[("BUGZILLA_API_KEY", "bz-from-env")]);
        assert_eq!(resolve_api_key(None, bz).unwrap(), "bz-from-env");
        assert_eq!(resolve_api_key(Some("bz-flag"), bz).unwrap(), "bz-flag");

        for f in secret_fields(&mut again) {
            crate::secrets::keyring().clear(&f.entry).unwrap();
        }
    }

    /// The keyring entry is the one for the Bugzilla the run talks to,
    /// so a key kept for another one is not sent there.
    #[test]
    #[serial_test::serial]
    fn a_bugzilla_key_is_read_for_the_bugzilla_in_use() {
        use crate::secrets::test_support::EnvGuard;
        let dir = tempfile::tempdir().unwrap();
        let _env = EnvGuard::new(&[
            ("XDG_CONFIG_HOME", &dir.path().to_string_lossy()),
            ("BUGZILLA_API_KEY", ""),
        ]);
        let entry = crate::secrets::bugzilla("https://bugzilla.one.test");
        crate::secrets::keyring().store(&entry, "one").unwrap();
        assert_eq!(
            resolve_api_key(None, "https://bugzilla.one.test").unwrap(),
            "one"
        );
        let err = resolve_api_key(None, "https://bugzilla.two.test").unwrap_err();
        assert!(err.contains("ebranch config"), "{err}");
        // No key anywhere: RPM Fusion stays read-only.
        assert!(rpmfusion_bugzilla().is_none());
        crate::secrets::keyring().clear(&entry).unwrap();
    }

    #[test]
    fn the_packager_table_carries_who_is_asking() {
        let cfg: EbranchConfig = toml::from_str(
            r#"
            [packager]
            fas = "salimma"
            provenpackager = true
            "#,
        )
        .unwrap();
        assert_eq!(cfg.packager.fas, "salimma");
        assert!(cfg.packager.provenpackager);
        // Absent is the same as unset: no FAS to fall back to, and no
        // claim of provenpackager.
        let bare: EbranchConfig = toml::from_str("").unwrap();
        assert!(bare.packager.fas.is_empty());
        assert!(!bare.packager.provenpackager);
    }

    #[test]
    fn check_crate_excludes_parse_from_their_table_and_default_empty() {
        let cfg: EbranchConfig = toml::from_str(
            r#"
            [bugzilla]
            url = "https://bugzilla.example"
            [check-crate]
            exclude = ["criterion", "pretty_assertions"]
            "#,
        )
        .unwrap();
        let (list, set) = resolve_excludes(cfg.check_crate.exclude);
        assert_eq!(list, ["criterion", "pretty_assertions"]);
        assert!(set);
        // No list: the built-in benchmark set applies.
        let bare: EbranchConfig = toml::from_str("").unwrap();
        let (list, set) = resolve_excludes(bare.check_crate.exclude);
        assert_eq!(list, DEFAULT_EXCLUDES);
        assert!(!set);
        // An explicit empty list replaces it: someone packaging criterion.
        let none: EbranchConfig = toml::from_str("[check-crate]\nexclude = []").unwrap();
        let (list, set) = resolve_excludes(none.check_crate.exclude);
        assert!(list.is_empty());
        assert!(set);
        // Per-crate in-tree lists and staging COPRs live in their own tables.
        let cfg: EbranchConfig = toml::from_str(
            r#"
            [check-crate.in-tree]
            coreutils = ["uu_*", "uucore*", "uutests"]
            [check-crate.copr]
            coreutils = "@rust/uutils-and-nushell"
            phf = "@rust/uutils-and-nushell"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.check_crate.copr["phf"], "@rust/uutils-and-nushell");
        assert!(!cfg.check_crate.copr.contains_key("serde"));
        assert_eq!(
            cfg.check_crate.in_tree["coreutils"],
            ["uu_*", "uucore*", "uutests"]
        );
        assert!(!cfg.check_crate.in_tree.contains_key("phf"));
        assert!(bare.check_crate.in_tree.is_empty());
        // "@default" inside the list stands for the built-in set.
        let (list, set) = resolve_excludes(Some(
            ["@default", "pretty_assertions"].map(String::from).to_vec(),
        ));
        let mut want: Vec<String> = DEFAULT_EXCLUDES.iter().map(|s| s.to_string()).collect();
        want.push("pretty_assertions".to_string());
        assert_eq!(list, want);
        assert!(set);
    }
}
