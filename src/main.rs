mod bugzilla;
mod config;
mod nvd;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use bugzilla::BzClient;
use clap::{Parser, Subcommand};
use config::NodejsFpsConfig;
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

    /// Detect NodeJS false positives from a tracker bug
    NodejsFps {
        /// Path to TOML config file
        #[arg(short = 'f', long)]
        config: PathBuf,
    },
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
        Command::NodejsFps { config } => cmd_nodejs_fps(config).await,
    }
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

    let bugs = client.search(&query).await?;

    for bug in &bugs {
        let is_cve =
            bug.summary.starts_with("CVE-") || bug.keywords.iter().any(|k| k == "Security");
        if !is_cve {
            continue;
        }
        println!("[{}] {}", bug.id, bug.summary);
    }

    Ok(())
}

async fn cmd_nodejs_fps(config_path: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let config = NodejsFpsConfig::from_file(&config_path)?;
    let bz = BzClient::new(BUGZILLA_URL);
    let nvd = NvdClient::new();

    // Build search query with multiple products, components, and statuses
    let mut query = String::new();
    for product in &config.products {
        query.push_str(&format!("&product={product}"));
    }
    for component in &config.components {
        query.push_str(&format!("&component={component}"));
    }
    for status in &config.statuses {
        query.push_str(&format!("&bug_status={status}"));
    }
    let query = query.trim_start_matches('&');

    let bugs = bz.search(query).await?;

    // Filter to CVE bugs only
    let cve_bugs: Vec<_> = bugs
        .iter()
        .filter(|b| b.summary.starts_with("CVE-") || b.keywords.iter().any(|k| k == "Security"))
        .collect();

    println!("Found {} CVE bugs to check", cve_bugs.len());

    let mut fps_found = 0;
    let mut nvd_requests = 0;
    let mut nodejs_cache: HashMap<String, bool> = HashMap::new();

    for bug in &cve_bugs {
        let component = bug.component.first().map(String::as_str).unwrap_or("");

        // Extract CVE ID from summary (e.g. "CVE-2026-25639 axios: ...")
        let cve_id = match bug.summary.split_whitespace().next() {
            Some(id) if id.starts_with("CVE-") => id.trim_end_matches(':'),
            _ => continue,
        };

        let is_nodejs = if let Some(&cached) = nodejs_cache.get(cve_id) {
            cached
        } else {
            // Rate-limit NVD requests (5 req / 30s for unauthenticated)
            if nvd_requests > 0 {
                tokio::time::sleep(Duration::from_secs(6)).await;
            }
            nvd_requests += 1;

            match nvd.cve(cve_id).await {
                Ok(resp) => {
                    let result = resp
                        .vulnerabilities
                        .iter()
                        .flat_map(|v| &v.cve.configurations)
                        .flat_map(|c| &c.nodes)
                        .flat_map(|n| &n.cpe_match)
                        .any(|m| m.targets_nodejs());
                    nodejs_cache.insert(cve_id.to_string(), result);
                    result
                }
                Err(e) => {
                    eprintln!("Warning: failed to fetch {} from NVD: {}", cve_id, e);
                    continue;
                }
            }
        };

        if is_nodejs {
            fps_found += 1;
            println!(
                "FP: bug {} ({} / {}) — {} targets node.js",
                bug.id, bug.product, component, cve_id
            );
        }
    }

    if fps_found == 0 {
        println!("No NodeJS false positives found.");
    } else {
        println!("\n{fps_found} likely false positive(s) found.");
    }

    Ok(())
}
