// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Query release codenames via the `distro-info` package. The bulk
//! `rebuild` uses `ubuntu-distro-info` to pick the PPA branches — a
//! branch whose codename is a real Ubuntu release — and to flag
//! end-of-life ones. `debian-distro-info` maps a Debian codename to its
//! release number (e.g. `trixie` → 13), used to recognise a
//! `debian/<codename>` proposed-update target and form its
//! `~deb<N>u<M>` version.

use std::collections::HashSet;
use std::process::Command;

/// The Debian release number for a codename
/// (`debian-distro-info --series=<codename> -r`), e.g. `trixie` → 13.
/// `Ok(None)` when the series isn't a numbered Debian release (e.g.
/// `unstable`/`sid`, or an Ubuntu codename); `Err` only when
/// `debian-distro-info` is missing. The lookup is a static table, so it
/// works on any host (no Debian environment required).
pub fn debian_major(codename: &str) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    let out = Command::new("debian-distro-info")
        .args([&format!("--series={codename}"), "-r"])
        .output()
        .map_err(|e| format!("debian-distro-info not available (install: distro-info): {e}"))?;
    // An unknown/unnumbered series exits non-zero — that just means
    // "not a Debian proposed-update target", not a hard error.
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_major(&String::from_utf8_lossy(&out.stdout)))
}

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

/// Parse the leading integer of `debian-distro-info -r` output (e.g.
/// `13` or `13.0` → `13`); `None` when there's no leading number.
fn parse_major(output: &str) -> Option<u32> {
    let digits: String = output
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
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

    #[test]
    fn parse_major_takes_leading_integer() {
        assert_eq!(parse_major("13\n"), Some(13));
        assert_eq!(parse_major("12.0\n"), Some(12));
        assert_eq!(parse_major(" 11 "), Some(11));
        // No leading number (e.g. an unnumbered series).
        assert_eq!(parse_major(""), None);
        assert_eq!(parse_major("sid\n"), None);
    }
}
