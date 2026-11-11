// SPDX-License-Identifier: Apache-2.0 OR MIT

//! `ebranch prune-cache`: remove what ebranch has cached and not used
//! for a while.
//!
//! The commands that keep data under `~/.cache/ebranch/` mark each
//! entry as they use it (`sandogasa_cli::cache::mark_used`); this
//! removes whole entries that have gone unused for longer than their
//! cache's age, and never runs as a side effect of another command.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// One of ebranch's caches.
pub struct Cache {
    /// Its directory under `~/.cache/ebranch/`.
    pub name: &'static str,
    pub holds: &'static str,
    pub max_age: Duration,
}

pub struct Options {
    /// Overrides every cache's own age.
    pub older_than: Option<Duration>,
    pub dry_run: bool,
    pub json: bool,
}

/// What pruning did to one cache.
#[derive(Debug, Serialize)]
pub struct CacheReport {
    pub name: String,
    pub holds: String,
    pub dir: PathBuf,
    pub max_age_days: u64,
    pub removed: Vec<RemovedEntry>,
    pub kept: usize,
    pub kept_bytes: u64,
}

/// An entry removed, or in a dry run one that would be.
#[derive(Debug, Serialize)]
pub struct RemovedEntry {
    pub name: String,
    pub bytes: u64,
    pub idle_days: u64,
}

/// ebranch's caches under `~/.cache/ebranch/`, what an entry is, and
/// how long one may go unused before it is removed.
pub const CACHES: &[Cache] = &[
    Cache {
        name: "bundled",
        holds: "upstream and submodule clones, for bundled-provides",
        max_age: Duration::from_secs(30 * 24 * 60 * 60),
    },
    Cache {
        name: "crates-io",
        holds: "crates.io answers, per crate",
        max_age: Duration::from_secs(30 * 24 * 60 * 60),
    },
    Cache {
        name: "distgit",
        holds: "dist-git clones, for rebuild-plan's comparison",
        max_age: Duration::from_secs(30 * 24 * 60 * 60),
    },
    Cache {
        name: "nvd",
        holds: "NVD answers, served for a day",
        max_age: sandogasa_cve::cache::NVD_TTL,
    },
    Cache {
        name: "update-repos",
        holds: "downloaded update builds, for check-update",
        max_age: Duration::from_secs(14 * 24 * 60 * 60),
    },
];

/// Prune every cache and print what was done.
pub fn run(opts: &Options) -> Result<(), String> {
    let base = dirs::cache_dir()
        .ok_or("cannot locate the cache directory: neither $XDG_CACHE_HOME nor $HOME is set")?
        .join("ebranch");
    let reports = prune_all(&base, opts)?;
    if opts.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "dry_run": opts.dry_run,
                "caches": reports,
            }))
            .expect("JSON serialization failed")
        );
    } else {
        print!("{}", render(&reports, opts.dry_run));
    }
    Ok(())
}

/// A size for people: `512 B`, `3.4 KB`, `43.0 MB`.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// `1 day`, `30 days`.
fn days(n: u64) -> String {
    format!("{n} day{}", if n == 1 { "" } else { "s" })
}

/// Prune each of [`CACHES`] under `base`.
fn prune_all(base: &Path, opts: &Options) -> Result<Vec<CacheReport>, String> {
    CACHES
        .iter()
        .map(|cache| {
            let dir = base.join(cache.name);
            let max_age = opts
                .older_than
                .unwrap_or(cache.max_age)
                .max(sandogasa_cli::cache::MIN_PRUNE_AGE);
            let pruned = sandogasa_cli::cache::prune(&dir, max_age, opts.dry_run)
                .map_err(|e| format!("cannot prune {}: {e}", dir.display()))?;
            Ok(CacheReport {
                name: cache.name.to_string(),
                holds: cache.holds.to_string(),
                max_age_days: max_age.as_secs() / DAY.as_secs(),
                removed: pruned
                    .removed
                    .iter()
                    .map(|e| RemovedEntry {
                        name: e
                            .path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        bytes: e.bytes,
                        idle_days: e.idle.as_secs() / DAY.as_secs(),
                    })
                    .collect(),
                kept: pruned.kept,
                kept_bytes: pruned.kept_bytes,
                dir,
            })
        })
        .collect()
}

/// The report: per cache, what was (or would be) removed and what is
/// left.
fn render(reports: &[CacheReport], dry_run: bool) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let verb = if dry_run { "would remove" } else { "removed" };
    let mut freed = 0;
    for r in reports {
        let removed: u64 = r.removed.iter().map(|e| e.bytes).sum();
        freed += removed;
        let _ = writeln!(
            out,
            "{} ({}; unused for over {}): {verb} {}, {}; kept {}, {}",
            r.name,
            r.holds,
            days(r.max_age_days),
            r.removed.len(),
            human_bytes(removed),
            r.kept,
            human_bytes(r.kept_bytes)
        );
        for e in &r.removed {
            let _ = writeln!(
                out,
                "  {} ({}, unused for {})",
                e.name,
                human_bytes(e.bytes),
                days(e.idle_days)
            );
        }
    }
    let _ = writeln!(out, "In all: {verb} {}.", human_bytes(freed));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_people_write_them() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(3_500), "3.4 KB");
        assert_eq!(human_bytes(45_088_768), "43.0 MB");
    }

    #[test]
    fn every_cache_is_pruned_and_reported() {
        let base = tempfile::tempdir().unwrap();
        let b = base.path();
        std::fs::create_dir_all(b.join("distgit/osmo")).unwrap();
        std::fs::write(b.join("distgit/osmo/osmo.spec"), "x".repeat(2048)).unwrap();
        // Last used 40 days ago: past distgit's 30.
        std::fs::write(b.join("distgit/.osmo.last-used"), "").unwrap();
        std::fs::File::open(b.join("distgit/.osmo.last-used"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - 40 * DAY)
            .unwrap();
        std::fs::create_dir_all(b.join("distgit/orage")).unwrap();
        sandogasa_cli::cache::mark_used(&b.join("distgit/orage"));

        let opts = |dry_run| Options {
            older_than: None,
            dry_run,
            json: false,
        };
        let reports = prune_all(b, &opts(true)).unwrap();
        assert_eq!(reports.len(), CACHES.len());
        let distgit = reports.iter().find(|r| r.name == "distgit").unwrap();
        assert_eq!(distgit.removed.len(), 1);
        assert_eq!(distgit.removed[0].name, "osmo");
        assert_eq!(distgit.removed[0].idle_days, 40);
        assert_eq!(distgit.kept, 1);
        let out = render(&reports, true);
        assert!(
            out.contains(
                "distgit (dist-git clones, for rebuild-plan's comparison; unused for over 30 \
                 days): would remove 1, 2.0 KB; kept 1, 0 B"
            ),
            "{out}"
        );
        assert!(out.contains("  osmo (2.0 KB, unused for 40 days)"), "{out}");
        assert!(
            out.contains("served for a day; unused for over 1 day): would remove 0"),
            "{out}"
        );
        assert!(out.ends_with("In all: would remove 2.0 KB.\n"), "{out}");
        assert!(b.join("distgit/osmo").exists());

        prune_all(b, &opts(false)).unwrap();
        assert!(!b.join("distgit/osmo").exists());
        assert!(b.join("distgit/orage").exists());
    }
}
