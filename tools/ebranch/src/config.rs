// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Configuration management for ebranch.
//!
//! Stores the Bugzilla API key at `~/.config/ebranch/config.toml`
//! with restricted permissions (dir 700, file 600), and carries
//! standing preferences such as the crates `check-crate` ignores.

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
    let cfg = sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .ok()?
        .rpmfusion_bugzilla;
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

/// The configured NVD API key, empty when there is none — the layered
/// config, so a key shipped system-wide counts.
pub fn nvd_api_key() -> String {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .map(|c| c.nvd.api_key)
        .unwrap_or_default()
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

/// The Bugzilla account to claim bookkeeping requests as, if set.
pub fn bugzilla_email() -> Option<String> {
    sandogasa_config::ConfigFile::for_tool("ebranch")
        .load::<EbranchConfig>()
        .ok()
        .map(|c| c.bugzilla.email)
        .filter(|e| !e.is_empty())
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

/// Load the Bugzilla API key, checking (in order):
/// 1. `--api-key` CLI flag
/// 2. `BUGZILLA_API_KEY` environment variable
/// 3. `~/.config/ebranch/config.toml`
///
/// Returns an error with setup instructions if none found.
pub fn resolve_api_key(cli_key: Option<&str>) -> Result<String, String> {
    if let Some(key) = cli_key
        && !key.is_empty()
    {
        return Ok(key.to_string());
    }

    if let Ok(key) = std::env::var("BUGZILLA_API_KEY")
        && !key.is_empty()
    {
        return Ok(key);
    }

    if let Ok(config) = sandogasa_config::ConfigFile::for_tool("ebranch").load::<EbranchConfig>()
        && !config.bugzilla.api_key.is_empty()
    {
        return Ok(config.bugzilla.api_key);
    }

    Err("Bugzilla API key not found.\n\
         Set it up with: ebranch config\n\
         Or pass --api-key or set BUGZILLA_API_KEY."
        .to_string())
}

/// What a credential check concluded.
enum KeyVerdict {
    /// The service took it.
    Accepted(String),
    /// The service refused it. Worth offering to replace.
    Refused(String),
    /// The check could not be made — a network error, a service that
    /// is down. Not the key's fault, so nothing is offered.
    Unknown(String),
}

/// Validate a credential, offering to replace one that is refused.
///
/// Every key this command checks goes through here, so a refusal
/// always offers the same way out rather than each check inventing
/// one — or, as all three did until now, simply reporting the problem
/// and saving the bad value anyway.
///
/// Declining is the point of the prompt rather than an afterthought:
/// a key edited by hand to watch the check fail must not be replaced
/// by something that assumes a typo. The offer defaults to yes, and
/// is skipped where stdin is not a terminal and nobody can answer.
async fn validated_key<F>(label: &str, key: String, check: F) -> String
where
    F: Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = KeyVerdict>>>,
{
    let mut key = key;
    loop {
        print!("Validating {label} key... ");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        match check(key.clone()).await {
            KeyVerdict::Accepted(note) => {
                println!("{note}");
                return key;
            }
            KeyVerdict::Unknown(note) => {
                println!("{note}");
                return key;
            }
            KeyVerdict::Refused(note) => {
                println!("{note}");
                if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                    eprintln!("warning: the {label} key was saved but will not work.");
                    return key;
                }
                match sandogasa_cli::confirm(&format!("enter a new {label} key?"), true) {
                    Ok(true) => {}
                    _ => {
                        eprintln!("warning: keeping the {label} key as it is; it will not work.");
                        return key;
                    }
                }
                match sandogasa_config::prompt_field(label, "API key", true, None) {
                    Ok(new) if !new.trim().is_empty() => key = new.trim().to_string(),
                    _ => {
                        eprintln!("warning: nothing entered; keeping the {label} key as it is.");
                        return key;
                    }
                }
            }
        }
    }
}

/// Interactive config setup.
pub async fn cmd_config() -> Result<(), String> {
    let cf = sandogasa_config::ConfigFile::for_tool("ebranch");
    let mut config: EbranchConfig = cf.load_user().unwrap_or_default();

    println!("ebranch configuration\n");
    println!("Config file: {}\n", cf.path().display());

    // Bugzilla URL.
    if config.bugzilla.url.is_empty() {
        config.bugzilla.url = "https://bugzilla.redhat.com".to_string();
    }
    println!("Bugzilla URL: {}", config.bugzilla.url);

    // API key.
    if config.bugzilla.api_key.is_empty() {
        println!(
            "\nGenerate an API key at:\n  \
             https://bugzilla.redhat.com/userprefs.cgi?tab=apikey\n"
        );
        let key = sandogasa_config::prompt_field("Bugzilla", "API key", true, None)
            .map_err(|e| format!("failed to read API key: {e}"))?;
        config.bugzilla.api_key = key;
    } else {
        println!("Bugzilla API key: (set)");
    }

    let url = config.bugzilla.url.clone();
    config.bugzilla.api_key =
        validated_key("Bugzilla", config.bugzilla.api_key.clone(), move |k| {
            let url = url.clone();
            Box::pin(async move {
                let bz = match sandogasa_bugzilla::BzClient::new(&url).with_api_key(k) {
                    Ok(bz) => bz,
                    Err(e) => return KeyVerdict::Unknown(format!("could not check: {e}")),
                };
                match bz.api_key_accepted().await {
                    Ok(true) => KeyVerdict::Accepted("accepted.".to_string()),
                    Ok(false) => KeyVerdict::Refused("refused.".to_string()),
                    Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
                }
            })
        })
        .await;

    // Bugzilla account: optional, and only ever used to assign a
    // request the asker files about their own work to themselves.
    if config.bugzilla.email.is_empty()
        && let Some(email) = sandogasa_config::prompt_optional_field(
            "Bugzilla",
            "account e-mail (optional, to claim your own requests)",
            false,
        )
        .map_err(|e| format!("failed to read e-mail: {e}"))?
    {
        config.bugzilla.email = email;
    }

    // FAS: who is asking. Everything dist-git is asked about a
    // package needs this, so it is worth having in the file rather
    // than on every command line.
    if config.packager.fas.is_empty()
        && let Some(fas) = sandogasa_config::prompt_optional_field(
            "FAS",
            "username (optional, the default for --fas)",
            false,
        )
        .map_err(|e| format!("failed to read FAS username: {e}"))?
    {
        config.packager.fas = fas;
    }

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
    if config.nvd.api_key.is_empty() {
        println!(
            "\nAn NVD API key raises the rate limit from 5 to 50 requests per 30 s when \
             check-update judges CVE bugs. Request one free at \
             https://nvd.nist.gov/developers/request-an-api-key, or leave empty to skip."
        );
        if let Some(key) = sandogasa_config::prompt_optional_field(
            "NVD",
            "API key (optional, empty to skip)",
            true,
        )
        .map_err(|e| format!("failed to read NVD API key: {e}"))?
        {
            config.nvd.api_key = key;
        }
    } else {
        println!("NVD API key: (set)");
    }
    if !config.nvd.api_key.is_empty() {
        config.nvd.api_key = validated_key("NVD", config.nvd.api_key.clone(), |k| {
            Box::pin(async move {
                use sandogasa_cve::cache::KeyCheck;
                match sandogasa_cve::cache::check_api_key(&k).await {
                    KeyCheck::Accepted => KeyVerdict::Accepted(
                        "valid; NVD lookups pace at 0.6 s instead of 6 s.".to_string(),
                    ),
                    KeyCheck::Refused => KeyVerdict::Refused(
                        "not accepted (404): NVD does not know this key — it is mistyped, \
                         or not yet activated; the activation link arrives by mail after \
                         the request."
                            .to_string(),
                    ),
                    // Rate limiting says nothing about the key, so
                    // offering to replace it would be wrong.
                    KeyCheck::Throttled(status) => KeyVerdict::Unknown(format!(
                        "NVD refused the request ({status}) — rate limited; try again in a minute"
                    )),
                    KeyCheck::Unreachable(e) => {
                        KeyVerdict::Unknown(format!("could not check: {e}"))
                    }
                }
            })
        })
        .await;
    }

    // RPM Fusion's Bugzilla: a separate instance with a separate
    // account, asked for last because it is only needed by someone
    // who maintains a package RPM Fusion also builds. Skipping it
    // leaves check-update holding auto-push and filing nothing,
    // which is the safe half of the guard.
    if config.rpmfusion_bugzilla.api_key.is_empty() {
        println!(
            "\nRPM Fusion runs its own Bugzilla. A key there lets check-update \
             file the rebuild request for a counterpart (libheif -> \
             libheif-freeworld) instead of only holding the update's auto-push. \
             Generate one at {}/userprefs.cgi?tab=apikey, or leave empty to skip.",
            config.rpmfusion_bugzilla.url
        );
        if let Some(key) = sandogasa_config::prompt_optional_field(
            "RPM Fusion Bugzilla",
            "API key (optional, empty to skip)",
            true,
        )
        .map_err(|e| format!("failed to read RPM Fusion API key: {e}"))?
        {
            config.rpmfusion_bugzilla.api_key = key;
        }
    } else {
        println!("RPM Fusion Bugzilla API key: (set)");
    }
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
        config.rpmfusion_bugzilla.api_key = validated_key("RPM Fusion Bugzilla", key, move |k| {
            let (url, rest) = (url.clone(), rest.clone());
            Box::pin(async move {
                let bz = match sandogasa_bugzilla::BzClient::new(&url)
                    .with_rest_path(&rest)
                    .with_stock_auth()
                    .with_api_key(k)
                {
                    Ok(bz) => bz,
                    Err(e) => return KeyVerdict::Unknown(format!("could not check: {e}")),
                };
                match bz.api_key_accepted().await {
                    Ok(true) => KeyVerdict::Accepted("accepted.".to_string()),
                    Ok(false) => KeyVerdict::Refused("refused.".to_string()),
                    Err(e) => KeyVerdict::Unknown(format!("could not check: {e}")),
                }
            })
        })
        .await;
    }

    cf.save(&config)
        .map_err(|e| format!("failed to save config: {e}"))?;
    println!("\nConfig saved to {}", cf.path().display());

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A check that answers whatever the test says, without a
    /// network or a terminal.
    fn answering(
        verdict: fn(String) -> KeyVerdict,
    ) -> impl Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = KeyVerdict>>> {
        move |k| Box::pin(async move { verdict(k) })
    }

    #[tokio::test]
    async fn a_key_the_service_takes_is_kept() {
        let kept = validated_key(
            "Test",
            "good".to_string(),
            answering(|_| KeyVerdict::Accepted("accepted.".into())),
        )
        .await;
        assert_eq!(kept, "good");
    }

    #[tokio::test]
    async fn a_key_is_kept_when_the_check_could_not_be_made() {
        // A rate limit or an unreachable service says nothing about
        // the key, so it must not be treated as a refusal — nothing
        // is offered and nothing is discarded.
        let kept = validated_key(
            "Test",
            "maybe".to_string(),
            answering(|_| KeyVerdict::Unknown("could not check.".into())),
        )
        .await;
        assert_eq!(kept, "maybe");
    }

    #[tokio::test]
    async fn a_refused_key_is_kept_when_there_is_nobody_to_ask() {
        // Non-interactively the offer cannot be made, so the value
        // stands and the run says so rather than prompting into a
        // pipe or discarding what the user configured.
        let kept = validated_key(
            "Test",
            "bad".to_string(),
            answering(|_| KeyVerdict::Refused("refused.".into())),
        )
        .await;
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
