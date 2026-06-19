// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query Ubuntu release codenames via `ubuntu-distro-info` (from the
//! `distro-info` package). The bulk `rebuild` uses this to pick the PPA
//! branches — a branch whose codename is a real Ubuntu release — and to
//! flag end-of-life ones (a codename no longer supported).

use std::collections::HashSet;
use std::process::Command;

/// Every Ubuntu release codename (`ubuntu-distro-info --all`), in
/// release order (oldest first) — the bulk rebuild uses this order to
/// process branches newest-first.
pub fn all_codenames() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    query(&["--all"])
}

/// Currently supported codenames (`ubuntu-distro-info --supported`),
/// which includes the in-development release. The complement (within
/// [`all_codenames`]) is end-of-life.
pub fn supported_codenames() -> Result<HashSet<String>, Box<dyn std::error::Error>> {
    Ok(query(&["--supported"])?.into_iter().collect())
}

fn query(args: &[&str]) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let out = Command::new("ubuntu-distro-info")
        .args(args)
        .output()
        .map_err(|e| format!("ubuntu-distro-info not available (install: distro-info): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "ubuntu-distro-info {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(parse_lines(&String::from_utf8_lossy(&out.stdout)))
}

/// Parse one-codename-per-line `ubuntu-distro-info` output, preserving
/// order and dropping blanks.
fn parse_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines_keeps_order_drops_blanks() {
        assert_eq!(
            parse_lines("jammy\nnoble\n\nquesting\n"),
            vec!["jammy", "noble", "questing"]
        );
    }
}
