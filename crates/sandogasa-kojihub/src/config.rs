// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Where a `koji` CLI profile points: the hub URL a tool that takes a
//! `--koji-profile` should talk to when it calls the hub directly.
//!
//! The files are the CLI's own, read in its order, a later one
//! overriding an earlier one: `/etc/koji.conf.d/*.conf`,
//! `/etc/koji.conf`, `~/.koji/config.d/*.conf`, `~/.koji/config`. Each
//! is INI, a section per profile (`[koji]` is the CLI's default, Fedora
//! ships `[fedora]` too), and the hub is its `server` key.

use std::path::PathBuf;

/// The hub URL `profile` names, or `None` when no configuration file
/// defines it.
pub fn hub_url(profile: &str) -> Option<String> {
    // The last file to define it wins, so look from the end.
    config_files()
        .iter()
        .rev()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .find_map(|text| server_in(&text, profile))
}

/// The `server` of the `[profile]` section of one INI file.
pub fn server_in(text: &str, profile: &str) -> Option<String> {
    let mut in_section = false;
    let mut server = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_section = name.trim() == profile;
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((key, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        if key.trim() == "server" {
            server = Some(value.trim().to_string());
        }
    }
    server
}

/// The configuration files, in the order the CLI reads them.
fn config_files() -> Vec<PathBuf> {
    let mut out = conf_d("/etc/koji.conf.d".into());
    out.push("/etc/koji.conf".into());
    if let Some(home) = dirs::home_dir() {
        out.extend(conf_d(home.join(".koji/config.d")));
        out.push(home.join(".koji/config"));
    }
    out
}

/// The `*.conf` files of a directory, sorted.
fn conf_d(dir: PathBuf) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "conf"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_is_read_from_the_profile_s_section() {
        let text = "\
[koji]
;configuration for koji cli tool
server = https://koji.fedoraproject.org/kojihub
weburl = https://koji.fedoraproject.org/koji

[stream]
server: https://kojihub.stream.centos.org/kojihub
";
        assert_eq!(
            server_in(text, "koji").as_deref(),
            Some("https://koji.fedoraproject.org/kojihub")
        );
        assert_eq!(
            server_in(text, "stream").as_deref(),
            Some("https://kojihub.stream.centos.org/kojihub")
        );
        assert_eq!(server_in(text, "cbs"), None);
    }
}
