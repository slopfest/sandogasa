// SPDX-License-Identifier: MPL-2.0

use hs_relmon::repology::{self, Client};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new();
    let project = "ethtool";
    let packages = client.get_project(project)?;

    let newest = repology::find_newest(&packages);
    let rawhide = repology::latest_for_repo(&packages, "fedora_rawhide");
    let stable = repology::latest_fedora_stable(&packages);

    println!("{project}:");
    match newest {
        Some(p) => println!("  upstream newest: {}", p.version),
        None => println!("  upstream newest: unknown"),
    }
    match rawhide {
        Some(p) => println!("  fedora rawhide:  {}", p.version),
        None => println!("  fedora rawhide:  not found"),
    }
    match stable {
        Some(p) => println!("  fedora stable:   {} ({})", p.version, p.repo),
        None => println!("  fedora stable:   not found"),
    }

    Ok(())
}
