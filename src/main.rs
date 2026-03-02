// SPDX-License-Identifier: MPL-2.0

mod bugzilla;
mod config;
mod nvd;

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

use bugzilla::BzClient;
use clap::{Parser, Subcommand};
use config::{AppConfig, BugzillaConfig, JsFpsConfig};
use nvd::NvdClient;

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

    for &bug_id in &fp_bug_ids {
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

        match bz.update(bug_id, &update).await {
            Ok(()) => println!("Closed bug {bug_id}"),
            Err(e) => eprintln!("Error closing bug {bug_id}: {e}"),
        }
    }

    println!("Done. {} bug(s) closed.", fp_bug_ids.len());

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

            let mut key = String::new();
            io::stdin().read_line(&mut key)?;
            let key = key.trim().to_string();

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
}
