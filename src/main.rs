// SPDX-License-Identifier: MPL-2.0

mod bodhi;
mod bugzilla;
mod config;
mod nvd;
mod version;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use bodhi::BodhiClient;
use bugzilla::BzClient;
use chrono::{DateTime, NaiveDateTime, TimeDelta, Utc};
use clap::{Parser, Subcommand};
use config::{AppConfig, BodhiCheckConfig, BugzillaConfig, JsFpsConfig};
use nvd::NvdClient;
use version::{Nvr, fedora_release_from_version, release_from_summary, version_gte};

const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

/// A tool for triaging CVEs reported against Fedora components in Bugzilla
#[derive(Parser)]
#[command(about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Search Bugzilla for CVE bugs
    Search {
        /// Bugzilla product (e.g. "Security Response", "Fedora", "Fedora EPEL")
        #[arg(short, long)]
        product: String,

        /// Bugzilla component, typically the source RPM name (e.g. "vulnerability", "kernel")
        #[arg(short, long)]
        component: Option<String>,

        /// Filter bugs by assignee email address
        #[arg(short, long)]
        assignee: Option<String>,

        /// Bug status filter
        #[arg(short, long, default_value = "NEW")]
        status: String,
    },

    /// Detect JavaScript/NodeJS false positives
    JsFps {
        /// Path to TOML config file
        #[arg(short = 'f', long)]
        config: PathBuf,

        /// Close detected false positives as NOTABUG and add them as blocking the tracker bug
        #[arg(long)]
        close_bugs: bool,
    },

    /// Check if CVE bugs are already fixed by a Bodhi update
    BodhiCheck {
        /// Path to TOML config file
        #[arg(short = 'f', long)]
        config: PathBuf,

        /// Close bugs that have a stable fix as ERRATA
        #[arg(long)]
        close_bugs: bool,

        /// Edit Bodhi updates in testing to add bug references (requires bodhi CLI)
        #[arg(long)]
        edit_bodhi: bool,
    },

    /// Set up or verify Bugzilla API key configuration
    Config,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Search {
            product,
            component,
            assignee,
            status,
        } => cmd_search(product, component, assignee, status).await,
        Command::JsFps { config, close_bugs } => cmd_js_fps(config, close_bugs).await,
        Command::BodhiCheck {
            config,
            close_bugs,
            edit_bodhi,
        } => cmd_bodhi_check(config, close_bugs, edit_bodhi).await,
        Command::Config => cmd_config(),
    }
}

/// Check whether a bug is a CVE bug (summary starts with "CVE-" or has the "Security" keyword).
fn is_cve_bug(summary: &str, keywords: &[String]) -> bool {
    summary.starts_with("CVE-") || keywords.iter().any(|k| k == "Security")
}

/// Extract a CVE ID from a bug summary (e.g. "CVE-2026-25639 axios: ..." → "CVE-2026-25639").
fn extract_cve_id(summary: &str) -> Option<&str> {
    match summary.split_whitespace().next() {
        Some(id) if id.starts_with("CVE-") => Some(id.trim_end_matches(':')),
        _ => None,
    }
}

/// Build a Bugzilla query string from lists of products, components, and statuses.
fn build_multi_query(products: &[String], components: &[String], statuses: &[String]) -> String {
    let mut query = String::new();
    for product in products {
        query.push_str(&format!("&product={product}"));
    }
    for component in components {
        query.push_str(&format!("&component={component}"));
    }
    for status in statuses {
        query.push_str(&format!("&bug_status={status}"));
    }
    query.trim_start_matches('&').to_string()
}

async fn cmd_search(
    product: String,
    component: Option<String>,
    assignee: Option<String>,
    status: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = BzClient::new(BUGZILLA_URL);

    let mut query = format!("product={product}&bug_status={status}");

    if let Some(ref component) = component {
        query.push_str(&format!("&component={component}"));
    }
    if let Some(ref assignee) = assignee {
        query.push_str(&format!("&assigned_to={assignee}"));
    }

    let bugs = client.search(&query, 0).await?;

    for bug in &bugs {
        if !is_cve_bug(&bug.summary, &bug.keywords) {
            continue;
        }
        println!("[{}] {}", bug.id, bug.summary);
    }

    Ok(())
}

async fn cmd_js_fps(
    config_path: PathBuf,
    close_bugs: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let config = JsFpsConfig::from_file(&config_path)?;
    let bz = BzClient::new(BUGZILLA_URL);
    let nvd = NvdClient::new();

    let query = build_multi_query(&config.products, &config.components, &config.statuses);

    let bugs = bz.search(&query, 0).await?;

    // Filter to CVE bugs only
    let cve_bugs: Vec<_> = bugs
        .iter()
        .filter(|b| is_cve_bug(&b.summary, &b.keywords))
        .collect();

    println!("Checking {} CVE bugs for JavaScript false positives...", cve_bugs.len());

    let mut fp_bug_ids: Vec<u64> = Vec::new();
    let mut nvd_requests = 0;
    let mut js_cache: HashMap<String, bool> = HashMap::new();

    for bug in &cve_bugs {
        // Extract CVE ID from summary (e.g. "CVE-2026-25639 axios: ...")
        let cve_id = match extract_cve_id(&bug.summary) {
            Some(id) => id,
            None => continue,
        };

        let is_js = if let Some(&cached) = js_cache.get(cve_id) {
            cached
        } else {
            // Rate-limit NVD requests (5 req / 30s for unauthenticated)
            if nvd_requests > 0 {
                tokio::time::sleep(Duration::from_secs(6)).await;
            }
            nvd_requests += 1;

            match nvd.cve(cve_id).await {
                Ok(resp) => {
                    let result = resp.targets_js();
                    js_cache.insert(cve_id.to_string(), result);
                    result
                }
                Err(e) => {
                    eprintln!("Warning: failed to fetch {} from NVD: {}", cve_id, e);
                    continue;
                }
            }
        };

        if is_js {
            fp_bug_ids.push(bug.id);
            println!("FP: bug {} — {}", bug.id, bug.summary);
        }
    }

    if fp_bug_ids.is_empty() {
        println!("No JavaScript false positives found.");
        return Ok(());
    }

    println!("\n{} likely false positive(s) found.", fp_bug_ids.len());

    if !close_bugs {
        return Ok(());
    }

    // Load API key for bug modifications
    let app_config = AppConfig::load()?;
    let bz = BzClient::new(BUGZILLA_URL).with_api_key(app_config.bugzilla.api_key);

    println!(
        "\nThis will close {} bug(s) as NOTABUG and mark them as blocking {}.",
        fp_bug_ids.len(),
        config.tracker_bug
    );
    print!("Proceed? [y/N] ");
    io::stdout().flush()?;

    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !answer.trim().eq_ignore_ascii_case("y") {
        println!("Aborted.");
        return Ok(());
    }

    let update = serde_json::json!({
        "status": "CLOSED",
        "resolution": "NOTABUG",
        "blocks": {
            "add": [config.tracker_bug]
        },
        "comment": {
            "body": config.reason
        }
    });

    match bz.update_many(&fp_bug_ids, &update).await {
        Ok(()) => println!("Closed {} bug(s).", fp_bug_ids.len()),
        Err(e) => eprintln!("Error closing bugs: {e}"),
    }

    Ok(())
}

/// Result of checking whether a bug is already fixed in Bodhi.
#[derive(Debug, PartialEq)]
enum BodhiCheckResult {
    /// A stable update contains a build that fixes the CVE.
    StableFix {
        bug_id: u64,
        alias: String,
        nvr: String,
        date_submitted: Option<String>,
    },
    /// A testing update contains a build that fixes the CVE.
    TestingFix {
        bug_id: u64,
        alias: String,
        nvr: String,
        date_submitted: Option<String>,
    },
    /// No Bodhi update has a build that fixes the CVE.
    NoFix { bug_id: u64 },
    /// NVD has no fixed version information for this CVE.
    NoFixedVersion { bug_id: u64 },
}

/// Determine the Fedora release for a bug: first from the version field, then from summary tags.
fn determine_release(version: &[String], summary: &str) -> Option<String> {
    // Try the version field first (e.g. ["42"] → "F42")
    if let Some(ver) = version.first() {
        if let Some(rel) = fedora_release_from_version(ver) {
            return Some(rel);
        }
    }
    // Fall back to summary tag (e.g. "[fedora-42]" → "F42")
    release_from_summary(summary)
}

/// Check a single bug against Bodhi updates and NVD fixed versions.
fn categorize_bug(
    bug_id: u64,
    fixed_versions: &[nvd::FixedVersion],
    updates: &[bodhi::models::Update],
    component: &str,
) -> BodhiCheckResult {
    if fixed_versions.is_empty() {
        return BodhiCheckResult::NoFixedVersion { bug_id };
    }

    for update in updates {
        for build in &update.builds {
            let nvr = match Nvr::parse(&build.nvr) {
                Some(n) => n,
                None => continue,
            };

            // Only consider builds for this component
            if nvr.name != component {
                continue;
            }

            // Check if this build's version is >= any of the fixed versions
            let is_fix = fixed_versions
                .iter()
                .any(|fv| version_gte(&nvr.version, &fv.version));

            if is_fix {
                if update.status == "stable" {
                    return BodhiCheckResult::StableFix {
                        bug_id,
                        alias: update.alias.clone(),
                        nvr: build.nvr.clone(),
                        date_submitted: update.date_submitted.clone(),
                    };
                } else {
                    return BodhiCheckResult::TestingFix {
                        bug_id,
                        alias: update.alias.clone(),
                        nvr: build.nvr.clone(),
                        date_submitted: update.date_submitted.clone(),
                    };
                }
            }
        }
    }

    BodhiCheckResult::NoFix { bug_id }
}

/// Check whether a bug was filed late (after the Bodhi update was already submitted + tolerance).
fn is_late_filed(
    bug_created: DateTime<Utc>,
    date_submitted: &str,
    lag_tolerance_minutes: i64,
) -> bool {
    let submitted = match NaiveDateTime::parse_from_str(date_submitted, "%Y-%m-%d %H:%M:%S") {
        Ok(dt) => dt.and_utc(),
        Err(_) => return false,
    };
    let deadline = submitted + TimeDelta::minutes(lag_tolerance_minutes);
    bug_created > deadline
}

async fn cmd_bodhi_check(
    config_path: PathBuf,
    close_bugs: bool,
    edit_bodhi: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Check bodhi CLI is available if --edit-bodhi was requested
    if edit_bodhi {
        let status = std::process::Command::new("which")
            .arg("bodhi")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if status.is_err() || !status.unwrap().success() {
            return Err("bodhi CLI not found. Install it with: sudo dnf install bodhi-client".into());
        }
    }

    let config = BodhiCheckConfig::from_file(&config_path)?;
    let bz = BzClient::new(BUGZILLA_URL);
    let nvd = NvdClient::new();
    let bodhi_client = BodhiClient::new();

    let query = build_multi_query(&config.products, &config.components, &config.statuses);
    let bugs = bz.search(&query, 0).await?;

    // Filter to CVE bugs only
    let cve_bugs: Vec<_> = bugs
        .iter()
        .filter(|b| is_cve_bug(&b.summary, &b.keywords))
        .collect();

    println!(
        "Checking {} CVE bugs for existing Bodhi fixes...",
        cve_bugs.len()
    );

    // Collect unique CVE IDs and query NVD for fixed versions
    let mut nvd_cache: HashMap<String, Vec<nvd::FixedVersion>> = HashMap::new();
    let mut nvd_requests = 0;

    for bug in &cve_bugs {
        let cve_id = match extract_cve_id(&bug.summary) {
            Some(id) => id.to_string(),
            None => continue,
        };

        if nvd_cache.contains_key(&cve_id) {
            continue;
        }

        // Rate-limit NVD requests (5 req / 30s for unauthenticated)
        if nvd_requests > 0 {
            tokio::time::sleep(Duration::from_secs(6)).await;
        }
        nvd_requests += 1;

        match nvd.cve(&cve_id).await {
            Ok(resp) => {
                let fv = resp.fixed_versions();
                nvd_cache.insert(cve_id, fv);
            }
            Err(e) => {
                eprintln!("Warning: failed to fetch {} from NVD: {}", cve_id, e);
            }
        }
    }

    // Query Bodhi for each (component, release) pair and categorize
    let mut bodhi_cache: HashMap<(String, String), Vec<bodhi::models::Update>> = HashMap::new();
    let mut results: Vec<BodhiCheckResult> = Vec::new();

    for bug in &cve_bugs {
        let cve_id = match extract_cve_id(&bug.summary) {
            Some(id) => id,
            None => continue,
        };

        let fixed_versions = match nvd_cache.get(cve_id) {
            Some(fv) => fv,
            None => continue,
        };

        let release = match determine_release(&bug.version, &bug.summary) {
            Some(r) => r,
            None => {
                eprintln!(
                    "Warning: cannot determine release for bug {} (version={:?})",
                    bug.id,
                    bug.version
                );
                continue;
            }
        };

        let component = match bug.component.first() {
            Some(c) => c.clone(),
            None => continue,
        };

        let cache_key = (component.clone(), release.clone());
        if !bodhi_cache.contains_key(&cache_key) {
            match bodhi_client
                .updates_for_package(&component, &release, &["stable", "testing"])
                .await
            {
                Ok(updates) => {
                    bodhi_cache.insert(cache_key.clone(), updates);
                }
                Err(e) => {
                    eprintln!(
                        "Warning: failed to query Bodhi for {} on {}: {}",
                        component, release, e
                    );
                    bodhi_cache.insert(cache_key.clone(), Vec::new());
                }
            }
        }

        let updates = bodhi_cache.get(&cache_key).unwrap();

        let result = categorize_bug(bug.id, fixed_versions, updates, &component);
        results.push(result);
    }

    // Print summary
    let stable_fixes: Vec<_> = results
        .iter()
        .filter(|r| matches!(r, BodhiCheckResult::StableFix { .. }))
        .collect();
    let testing_fixes: Vec<_> = results
        .iter()
        .filter(|r| matches!(r, BodhiCheckResult::TestingFix { .. }))
        .collect();
    let no_fixes: Vec<_> = results
        .iter()
        .filter(|r| matches!(r, BodhiCheckResult::NoFix { .. }))
        .collect();
    let no_fixed_ver: Vec<_> = results
        .iter()
        .filter(|r| matches!(r, BodhiCheckResult::NoFixedVersion { .. }))
        .collect();

    if !stable_fixes.is_empty() {
        println!("\nStable fixes ({}):", stable_fixes.len());
        for r in &stable_fixes {
            if let BodhiCheckResult::StableFix {
                bug_id,
                alias,
                nvr,
                ..
            } = r
            {
                println!("  bug {} — {} ({})", bug_id, nvr, alias);
            }
        }
    }

    if !testing_fixes.is_empty() {
        println!("\nTesting fixes ({}):", testing_fixes.len());
        for r in &testing_fixes {
            if let BodhiCheckResult::TestingFix {
                bug_id,
                alias,
                nvr,
                ..
            } = r
            {
                println!("  bug {} — {} ({})", bug_id, nvr, alias);
            }
        }
    }

    if !no_fixes.is_empty() {
        println!("\nNo fix found ({}):", no_fixes.len());
        for r in &no_fixes {
            if let BodhiCheckResult::NoFix { bug_id } = r {
                println!("  bug {}", bug_id);
            }
        }
    }

    if !no_fixed_ver.is_empty() {
        println!("\nNo fixed version in NVD ({}):", no_fixed_ver.len());
        for r in &no_fixed_ver {
            if let BodhiCheckResult::NoFixedVersion { bug_id } = r {
                println!("  bug {}", bug_id);
            }
        }
    }

    // --close-bugs: close StableFix bugs as ERRATA, and block tracker for late-filed bugs
    if close_bugs {
        // Build a map of bug_id → creation_time for late-filed detection
        let bug_creation_times: HashMap<u64, DateTime<Utc>> = cve_bugs
            .iter()
            .map(|b| (b.id, b.creation_time))
            .collect();

        // Find late-filed bugs (filed after update submission + lag tolerance)
        let late_filed: Vec<u64> = results
            .iter()
            .filter_map(|r| {
                let (bug_id, date_submitted) = match r {
                    BodhiCheckResult::StableFix {
                        bug_id,
                        date_submitted: Some(ds),
                        ..
                    }
                    | BodhiCheckResult::TestingFix {
                        bug_id,
                        date_submitted: Some(ds),
                        ..
                    } => (*bug_id, ds.as_str()),
                    _ => return None,
                };
                let created = bug_creation_times.get(&bug_id)?;
                if is_late_filed(*created, date_submitted, config.lag_tolerance) {
                    Some(bug_id)
                } else {
                    None
                }
            })
            .collect();

        if !stable_fixes.is_empty() || !late_filed.is_empty() {
            let app_config = AppConfig::load()?;
            let bz = BzClient::new(BUGZILLA_URL).with_api_key(app_config.bugzilla.api_key);

            // Describe what will happen
            let mut actions: Vec<String> = Vec::new();
            if !stable_fixes.is_empty() {
                actions.push(format!("close {} bug(s) as ERRATA", stable_fixes.len()));
            }
            if !late_filed.is_empty() {
                actions.push(format!(
                    "mark {} late-filed bug(s) as blocking {}",
                    late_filed.len(),
                    config.tracker_bug
                ));
            }
            println!("\nThis will {}.", actions.join(" and "));
            print!("Proceed? [y/N] ");
            io::stdout().flush()?;

            let mut answer = String::new();
            io::stdin().read_line(&mut answer)?;
            if !answer.trim().eq_ignore_ascii_case("y") {
                println!("Aborted.");
                return Ok(());
            }

            // Close StableFix bugs as ERRATA, grouped by NVR for batch updates
            let mut by_nvr: HashMap<String, Vec<u64>> = HashMap::new();
            for r in &stable_fixes {
                if let BodhiCheckResult::StableFix { bug_id, nvr, .. } = r {
                    by_nvr.entry(nvr.clone()).or_default().push(*bug_id);
                }
            }
            for (nvr, bug_ids) in &by_nvr {
                let update = serde_json::json!({
                    "status": "CLOSED",
                    "resolution": "ERRATA",
                    "cf_fixed_in": nvr,
                    "comment": {
                        "body": format!("This bug is already fixed in a published Bodhi update: {nvr}")
                    }
                });

                match bz.update_many(bug_ids, &update).await {
                    Ok(()) => println!(
                        "Closed {} bug(s) as ERRATA ({})",
                        bug_ids.len(),
                        nvr
                    ),
                    Err(e) => eprintln!("Error closing bugs for {}: {}", nvr, e),
                }
            }

            // Add tracker_bug as blocker for late-filed bugs
            if !late_filed.is_empty() {
                let update = serde_json::json!({
                    "blocks": {
                        "add": [config.tracker_bug]
                    },
                    "comment": {
                        "body": config.reason
                    }
                });

                match bz.update_many(&late_filed, &update).await {
                    Ok(()) => println!(
                        "Marked {} bug(s) as blocking {} (late-filed)",
                        late_filed.len(),
                        config.tracker_bug
                    ),
                    Err(e) => eprintln!("Error updating late-filed bugs: {}", e),
                }
            }
        }
    }

    // --edit-bodhi: add bug references to testing updates
    if edit_bodhi && !testing_fixes.is_empty() {
        println!("\nAdding bug references to testing updates...");
        for r in &testing_fixes {
            if let BodhiCheckResult::TestingFix {
                bug_id,
                alias,
                ..
            } = r
            {
                let output = std::process::Command::new("bodhi")
                    .args(["updates", "edit", alias, "--bugs", &bug_id.to_string()])
                    .output();

                match output {
                    Ok(out) if out.status.success() => {
                        println!("  Added bug {} to {}", bug_id, alias);
                    }
                    Ok(out) => {
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        eprintln!("  Error editing {}: {}", alias, stderr.trim());
                    }
                    Err(e) => {
                        eprintln!("  Failed to run bodhi CLI for {}: {}", alias, e);
                    }
                }
            }
        }
    }

    println!("\nDone.");
    Ok(())
}

fn cmd_config() -> Result<(), Box<dyn std::error::Error>> {
    let config_path = AppConfig::path();

    match AppConfig::load() {
        Ok(_) => {
            println!("Config OK: {}", config_path.display());
        }
        Err(_) => {
            println!("No config found at {}", config_path.display());
            println!(
                "Create an API key at https://bugzilla.redhat.com/userprefs.cgi?tab=apikey"
            );
            print!("Enter your Bugzilla API key: ");
            io::stdout().flush()?;

            let key = rpassword::read_password()?.trim().to_string();

            if key.is_empty() {
                return Err("API key cannot be empty".into());
            }

            let config = AppConfig {
                bugzilla: BugzillaConfig { api_key: key },
            };
            config.save()?;
            println!("Saved to {}", config_path.display());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- is_cve_bug ----

    #[test]
    fn is_cve_bug_with_cve_prefix() {
        assert!(is_cve_bug("CVE-2026-25639 axios: SSRF", &[]));
    }

    #[test]
    fn is_cve_bug_with_security_keyword() {
        let kw = vec!["Security".to_string()];
        assert!(is_cve_bug("Some non-CVE summary", &kw));
    }

    #[test]
    fn is_cve_bug_with_both() {
        let kw = vec!["Security".to_string()];
        assert!(is_cve_bug("CVE-2026-12345 foo: bar", &kw));
    }

    #[test]
    fn is_not_cve_bug() {
        assert!(!is_cve_bug("Crash in libfoo on startup", &[]));
    }

    #[test]
    fn is_not_cve_bug_wrong_keyword() {
        let kw = vec!["SecurityTracking".to_string()];
        assert!(!is_cve_bug("Bug in libfoo", &kw));
    }

    #[test]
    fn is_cve_bug_bare_prefix() {
        assert!(is_cve_bug("CVE-", &[]));
    }

    // ---- extract_cve_id ----

    #[test]
    fn extract_cve_id_normal() {
        assert_eq!(
            extract_cve_id("CVE-2026-25639 axios: SSRF vulnerability"),
            Some("CVE-2026-25639")
        );
    }

    #[test]
    fn extract_cve_id_with_trailing_colon() {
        assert_eq!(
            extract_cve_id("CVE-2026-25639: axios SSRF"),
            Some("CVE-2026-25639")
        );
    }

    #[test]
    fn extract_cve_id_only_id() {
        assert_eq!(extract_cve_id("CVE-2026-25639"), Some("CVE-2026-25639"));
    }

    #[test]
    fn extract_cve_id_not_cve() {
        assert_eq!(extract_cve_id("Bug in libfoo causes crash"), None);
    }

    #[test]
    fn extract_cve_id_empty() {
        assert_eq!(extract_cve_id(""), None);
    }

    #[test]
    fn extract_cve_id_cve_in_middle() {
        // CVE ID must be the first word
        assert_eq!(extract_cve_id("Bug CVE-2026-12345 in foo"), None);
    }

    // ---- build_multi_query ----

    #[test]
    fn build_multi_query_full() {
        let q = build_multi_query(
            &["Fedora".into(), "Fedora EPEL".into()],
            &["vulnerability".into()],
            &["NEW".into(), "ASSIGNED".into()],
        );
        assert_eq!(
            q,
            "product=Fedora&product=Fedora EPEL&component=vulnerability&bug_status=NEW&bug_status=ASSIGNED"
        );
    }

    #[test]
    fn build_multi_query_single_each() {
        let q = build_multi_query(
            &["Fedora".into()],
            &["kernel".into()],
            &["NEW".into()],
        );
        assert_eq!(q, "product=Fedora&component=kernel&bug_status=NEW");
    }

    #[test]
    fn build_multi_query_empty() {
        let q = build_multi_query(&[], &[], &[]);
        assert_eq!(q, "");
    }

    #[test]
    fn build_multi_query_products_only() {
        let q = build_multi_query(&["Security Response".into()], &[], &[]);
        assert_eq!(q, "product=Security Response");
    }

    // ---- determine_release ----

    #[test]
    fn determine_release_from_version_field() {
        assert_eq!(
            determine_release(&["42".to_string()], "CVE-2026-12345 foo: bar"),
            Some("F42".to_string())
        );
    }

    #[test]
    fn determine_release_from_summary_tag() {
        assert_eq!(
            determine_release(&[], "CVE-2026-12345 foo: bar [fedora-42]"),
            Some("F42".to_string())
        );
    }

    #[test]
    fn determine_release_version_takes_precedence() {
        // version field says 41, summary says 42 — version wins
        assert_eq!(
            determine_release(&["41".to_string()], "CVE-2026-12345 foo [fedora-42]"),
            Some("F41".to_string())
        );
    }

    #[test]
    fn determine_release_rawhide_falls_back_to_summary() {
        assert_eq!(
            determine_release(&["rawhide".to_string()], "CVE-2026-12345 foo [fedora-42]"),
            Some("F42".to_string())
        );
    }

    #[test]
    fn determine_release_no_info() {
        assert_eq!(
            determine_release(&[], "CVE-2026-12345 foo: bar"),
            None
        );
    }

    #[test]
    fn determine_release_epel() {
        assert_eq!(
            determine_release(&["epel9".to_string()], "CVE-2026-12345 foo"),
            Some("EPEL-9".to_string())
        );
    }

    // ---- categorize_bug ----

    fn make_update(alias: &str, status: &str, builds: &[&str]) -> bodhi::models::Update {
        make_update_with_date(alias, status, builds, None)
    }

    fn make_update_with_date(
        alias: &str,
        status: &str,
        builds: &[&str],
        date_submitted: Option<&str>,
    ) -> bodhi::models::Update {
        bodhi::models::Update {
            alias: alias.to_string(),
            status: status.to_string(),
            builds: builds
                .iter()
                .map(|nvr| bodhi::models::Build {
                    nvr: nvr.to_string(),
                })
                .collect(),
            bugs: vec![],
            release: None,
            date_submitted: date_submitted.map(|s| s.to_string()),
        }
    }

    fn make_fixed_version(product: &str, version: &str) -> nvd::FixedVersion {
        nvd::FixedVersion {
            product: product.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn categorize_stable_fix() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update(
            "FEDORA-2026-abc",
            "stable",
            &["freerdp-3.23.0-1.fc42"],
        )];

        let result = categorize_bug(100, &fv, &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::StableFix {
                bug_id: 100,
                alias: "FEDORA-2026-abc".to_string(),
                nvr: "freerdp-3.23.0-1.fc42".to_string(),
                date_submitted: None,
            }
        );
    }

    #[test]
    fn categorize_testing_fix() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update(
            "FEDORA-2026-xyz",
            "testing",
            &["freerdp-3.24.0-1.fc42"],
        )];

        let result = categorize_bug(200, &fv, &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::TestingFix {
                bug_id: 200,
                alias: "FEDORA-2026-xyz".to_string(),
                nvr: "freerdp-3.24.0-1.fc42".to_string(),
                date_submitted: None,
            }
        );
    }

    #[test]
    fn categorize_no_fix_version_too_old() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update(
            "FEDORA-2026-old",
            "stable",
            &["freerdp-3.22.0-1.fc42"],
        )];

        let result = categorize_bug(300, &fv, &updates, "freerdp");
        assert_eq!(result, BodhiCheckResult::NoFix { bug_id: 300 });
    }

    #[test]
    fn categorize_no_fixed_version() {
        let updates = vec![make_update(
            "FEDORA-2026-any",
            "stable",
            &["freerdp-3.23.0-1.fc42"],
        )];

        let result = categorize_bug(400, &[], &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::NoFixedVersion { bug_id: 400 }
        );
    }

    #[test]
    fn categorize_wrong_component_ignored() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update(
            "FEDORA-2026-other",
            "stable",
            &["other-pkg-3.23.0-1.fc42"],
        )];

        let result = categorize_bug(500, &fv, &updates, "freerdp");
        assert_eq!(result, BodhiCheckResult::NoFix { bug_id: 500 });
    }

    #[test]
    fn categorize_no_updates() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];

        let result = categorize_bug(600, &fv, &[], "freerdp");
        assert_eq!(result, BodhiCheckResult::NoFix { bug_id: 600 });
    }

    #[test]
    fn categorize_stable_preferred_over_testing() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![
            make_update("FEDORA-2026-stable", "stable", &["freerdp-3.23.0-1.fc42"]),
            make_update(
                "FEDORA-2026-testing",
                "testing",
                &["freerdp-3.24.0-1.fc42"],
            ),
        ];

        let result = categorize_bug(700, &fv, &updates, "freerdp");
        // Should find stable first since it appears first
        assert!(matches!(result, BodhiCheckResult::StableFix { .. }));
    }

    #[test]
    fn categorize_multiple_fixed_versions() {
        // Two version ranges (e.g. 2.x and 3.x branches)
        let fv = vec![
            make_fixed_version("freerdp", "3.23.0"),
            make_fixed_version("freerdp", "2.11.8"),
        ];
        let updates = vec![make_update(
            "FEDORA-2026-old",
            "stable",
            &["freerdp-2.11.8-1.fc41"],
        )];

        let result = categorize_bug(800, &fv, &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::StableFix {
                bug_id: 800,
                alias: "FEDORA-2026-old".to_string(),
                nvr: "freerdp-2.11.8-1.fc41".to_string(),
                date_submitted: None,
            }
        );
    }

    #[test]
    fn categorize_stable_fix_with_date_submitted() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update_with_date(
            "FEDORA-2026-dated",
            "stable",
            &["freerdp-3.23.0-1.fc42"],
            Some("2026-02-25 11:55:26"),
        )];

        let result = categorize_bug(900, &fv, &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::StableFix {
                bug_id: 900,
                alias: "FEDORA-2026-dated".to_string(),
                nvr: "freerdp-3.23.0-1.fc42".to_string(),
                date_submitted: Some("2026-02-25 11:55:26".to_string()),
            }
        );
    }

    #[test]
    fn categorize_testing_fix_with_date_submitted() {
        let fv = vec![make_fixed_version("freerdp", "3.23.0")];
        let updates = vec![make_update_with_date(
            "FEDORA-2026-test",
            "testing",
            &["freerdp-3.24.0-1.fc42"],
            Some("2026-02-20 08:00:00"),
        )];

        let result = categorize_bug(950, &fv, &updates, "freerdp");
        assert_eq!(
            result,
            BodhiCheckResult::TestingFix {
                bug_id: 950,
                alias: "FEDORA-2026-test".to_string(),
                nvr: "freerdp-3.24.0-1.fc42".to_string(),
                date_submitted: Some("2026-02-20 08:00:00".to_string()),
            }
        );
    }

    // ---- is_late_filed ----

    #[test]
    fn is_late_filed_bug_filed_well_after_submission() {
        // Update submitted at 2026-02-25 12:00:00, bug filed 2 hours later, tolerance 30 min
        let bug_created = "2026-02-25T14:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(is_late_filed(bug_created, "2026-02-25 12:00:00", 30));
    }

    #[test]
    fn is_late_filed_bug_filed_before_submission() {
        // Bug filed before the update was even submitted
        let bug_created = "2026-02-24T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(!is_late_filed(bug_created, "2026-02-25 12:00:00", 30));
    }

    #[test]
    fn is_late_filed_bug_filed_within_tolerance() {
        // Bug filed 10 minutes after submission, tolerance 30 min → not late
        let bug_created = "2026-02-25T12:10:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(!is_late_filed(bug_created, "2026-02-25 12:00:00", 30));
    }

    #[test]
    fn is_late_filed_bug_filed_exactly_at_deadline() {
        // Bug filed exactly at submission + tolerance → not late (need to be strictly after)
        let bug_created = "2026-02-25T12:30:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(!is_late_filed(bug_created, "2026-02-25 12:00:00", 30));
    }

    #[test]
    fn is_late_filed_zero_tolerance() {
        // Zero tolerance: bug filed 1 second after submission → late
        let bug_created = "2026-02-25T12:00:01Z".parse::<DateTime<Utc>>().unwrap();
        assert!(is_late_filed(bug_created, "2026-02-25 12:00:00", 0));
    }

    #[test]
    fn is_late_filed_invalid_date_returns_false() {
        let bug_created = "2026-02-25T14:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert!(!is_late_filed(bug_created, "not-a-date", 30));
    }
}
