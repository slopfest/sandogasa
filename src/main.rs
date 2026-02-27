mod bugzilla;

use bugzilla::BzClient;
use clap::Parser;

const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

/// A tool for triaging CVEs reported against Fedora components in Bugzilla
#[derive(Parser)]
#[command(about)]
struct Args {
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let client = BzClient::new(BUGZILLA_URL);

    let mut query = format!("product={}&bug_status={}", args.product, args.status);

    if let Some(ref component) = args.component {
        query.push_str(&format!("&component={component}"));
    }
    if let Some(ref assignee) = args.assignee {
        query.push_str(&format!("&assigned_to={assignee}"));
    }

    let bugs = client.search(&query).await?;

    for bug in &bugs {
        let is_cve = bug.summary.starts_with("CVE-")
            || bug.keywords.iter().any(|k| k == "Security");
        if !is_cve {
            continue;
        }
        println!("[{}] {}", bug.id, bug.summary);
    }

    Ok(())
}
