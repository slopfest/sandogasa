// SPDX-License-Identifier: MPL-2.0

use hs_relmon::cbs::{self, HyperscaleSummary};
use hs_relmon::repology;

fn print_hyperscale(label: &str, summary: &HyperscaleSummary) {
    match (&summary.release, &summary.testing) {
        (Some(rel), Some(test)) => {
            println!("  {label} release: {} ({})", rel.version, rel.nvr);
            println!("  {label} testing: {} ({})", test.version, test.nvr);
        }
        (Some(rel), None) => {
            println!("  {label} release: {} ({})", rel.version, rel.nvr);
        }
        (None, Some(test)) => {
            println!("  {label} release: not found");
            println!("  {label} testing: {} ({})", test.version, test.nvr);
        }
        (None, None) => {
            println!("  {label}          not found");
        }
    }
}

fn query_project(
    repology_client: &repology::Client,
    cbs_client: &cbs::Client,
    project: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // Query Repology
    let packages = repology_client.get_project(project)?;

    let newest = repology::find_newest(&packages);
    let rawhide = repology::latest_for_repo(&packages, "fedora_rawhide");
    let stable = repology::latest_fedora_stable(&packages);
    let centos = repology::latest_centos_stream(&packages);

    // Query CBS Koji for Hyperscale builds
    let builds = cbs_client
        .get_package_id(project)?
        .map(|id| cbs_client.list_builds(id))
        .transpose()?;
    let empty = Vec::new();
    let builds = builds.as_deref().unwrap_or(&empty);
    let hs9 = cbs_client.hyperscale_summary(builds, 9)?;
    let hs10 = cbs_client.hyperscale_summary(builds, 10)?;

    println!("{project}:");
    match newest {
        Some(p) => println!("  upstream newest:  {}", p.version),
        None => println!("  upstream newest:  unknown"),
    }
    match rawhide {
        Some(p) => println!("  fedora rawhide:   {}", p.version),
        None => println!("  fedora rawhide:   not found"),
    }
    match stable {
        Some(p) => println!("  fedora stable:    {} ({})", p.version, p.repo),
        None => println!("  fedora stable:    not found"),
    }
    match centos {
        Some(p) => println!("  centos stream:    {} ({})", p.version, p.repo),
        None => println!("  centos stream:    not found"),
    }
    print_hyperscale("hs9", &hs9);
    print_hyperscale("hs10", &hs10);

    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let repology_client = repology::Client::new();
    let cbs_client = cbs::Client::new();

    for project in ["ethtool", "systemd"] {
        query_project(&repology_client, &cbs_client, project)?;
        println!();
    }

    Ok(())
}
