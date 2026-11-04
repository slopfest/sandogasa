// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Answers kept between runs, under the tool's XDG cache directory.
//!
//! A tool that asks the same service the same question on every run —
//! NVD for a CVE, fedrq for a package's provides, meetbot for a
//! meeting's log — keeps the answer here, one file per key, each cache
//! with a life that matches how the answer ages: a day for a service
//! whose data moves, forever for a record that never changes (a Koji
//! build's RPMs, a finished meeting's log). `--refresh` on the tool
//! bypasses every read. Failures to write are silent: a cache is a
//! convenience, and the answer is in hand either way.
//!
//! Nothing here removes an entry on its own. A tool marks the entries
//! it uses ([`mark_used`]), and a separate command removes the ones
//! that have gone unused ([`prune`]), whole, never file by file: a git
//! clone with some of its objects deleted is worse than none.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Answers kept between runs, one file per key under a subdirectory
/// of the tool's XDG cache directory (`~/.cache/<tool>/<what>/`).
/// Each cache has its own life: NVD's answer to a CVE and a referenced
/// advisory page serve for a day — NVD's analysis changes, a fixed
/// version appears, a CPE is added — while what a Koji build provides
/// never changes, so those serve for good. `--refresh` bypasses reads
/// (and re-stores). Failures to write are silent: a cache is a
/// convenience, and the answer is in hand either way.
pub struct DiskCache {
    dir: Option<PathBuf>,
    refresh: bool,
    ttl: Option<Duration>,
}

/// What [`prune`] did with the entries of one cache directory.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct Pruned {
    /// The entries removed (or, in a dry run, that would be), oldest
    /// first.
    pub removed: Vec<Expired>,
    /// How many entries were kept, and their size in bytes.
    pub kept: usize,
    pub kept_bytes: u64,
}

/// A cache entry unused for longer than its cache allows.
#[derive(Debug)]
#[non_exhaustive]
pub struct Expired {
    pub path: PathBuf,
    /// Its size in bytes, files under it included.
    pub bytes: u64,
    /// How long ago it was last used.
    pub idle: Duration,
}

/// Whether the missing-cache-directory warning has been printed.
static CACHE_DIR_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

impl DiskCache {
    /// The cache `what` under the XDG cache directory, or no cache —
    /// with a warning, once, since a silent miss would look like a slow
    /// network — when neither `$XDG_CACHE_HOME` nor `$HOME` says where
    /// that is.
    pub fn new(tool: &str, what: &str, ttl: Option<Duration>, refresh: bool) -> Self {
        let dir = dirs::cache_dir().map(|d| d.join(tool).join(what));
        if dir.is_none() && !CACHE_DIR_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!(
                "warning: neither XDG_CACHE_HOME nor HOME is set, so nothing is cached between \
                 runs"
            );
        }
        Self::at(dir, refresh, ttl)
    }

    pub fn at(dir: Option<PathBuf>, refresh: bool, ttl: Option<Duration>) -> Self {
        Self { dir, refresh, ttl }
    }

    fn path(&self, key: &str) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join(key))
    }

    /// The stored body for `key`, if present and — when the cache has
    /// a life — young enough.
    pub fn load(&self, key: &str) -> Option<String> {
        if self.refresh {
            return None;
        }
        let path = self.path(key)?;
        if let Some(ttl) = self.ttl {
            let age = std::fs::metadata(&path)
                .ok()?
                .modified()
                .ok()?
                .elapsed()
                .ok()?;
            if age > ttl {
                return None;
            }
        }
        std::fs::read_to_string(path).ok()
    }

    /// Store a body, atomically (a reader never sees a half-written
    /// file); failures are silent.
    pub fn store(&self, key: &str, body: &str) {
        let Some(path) = self.path(key) else { return };
        if let Some(parent) = path.parent()
            && std::fs::create_dir_all(parent).is_ok()
        {
            let tmp = path.with_extension("tmp");
            if std::fs::write(&tmp, body).is_ok() {
                let _ = std::fs::rename(&tmp, &path);
            }
        }
    }
}

/// A file name for a URL: its host, made safe, and a hash of the whole,
/// so two pages on one host do not collide and no URL character reaches
/// the filesystem.
pub fn url_key(url: &str) -> String {
    use std::hash::{Hash, Hasher};
    let host = url
        .split("://")
        .nth(1)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or("")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    let mut hasher = std::hash::DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{host}-{:016x}.txt", hasher.finish())
}

/// The shortest age [`prune`] accepts: no command runs for a day, so
/// an entry one is using is never removed under it.
pub const MIN_PRUNE_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// Record that the cache entry `entry` (a directory or a file) was used
/// just now, for [`prune`]. The time goes into a stamp beside it,
/// `.<name>.last-used`, not into the entry: a stamp inside a git clone
/// would show up as an untracked file, and touching a file's own
/// modification time would make a [`DiskCache`] entry look freshly
/// fetched. Failures are silent, as with every cache write.
pub fn mark_used(entry: &Path) {
    if let Some(stamp) = stamp_of(entry) {
        let _ = std::fs::write(stamp, "");
    }
}

/// When the cache entry `entry` was last used: its [`mark_used`]
/// stamp, or for an entry never marked, its own modification time —
/// for a [`DiskCache`] file, when it was fetched, which is also what
/// its life is counted from. `None` when neither can be read.
pub fn last_used(entry: &Path) -> Option<SystemTime> {
    stamp_of(entry)
        .and_then(|s| std::fs::metadata(s).ok()?.modified().ok())
        .or_else(|| std::fs::symlink_metadata(entry).ok()?.modified().ok())
}

/// Remove the entries directly under `dir` that have gone unused (see
/// [`last_used`]) for longer than `max_age`, whole, with their stamps;
/// with `dry_run`, only report them. `max_age` is raised to
/// [`MIN_PRUNE_AGE`] if shorter. An entry whose time cannot be read is
/// kept. A missing `dir` is an empty cache, not an error.
pub fn prune(dir: &Path, max_age: Duration, dry_run: bool) -> std::io::Result<Pruned> {
    prune_at(dir, max_age, dry_run, SystemTime::now())
}

/// Where [`mark_used`] keeps `entry`'s stamp: `.<name>.last-used` in
/// the same directory.
fn stamp_of(entry: &Path) -> Option<PathBuf> {
    let name = entry.file_name()?.to_str()?;
    Some(entry.with_file_name(format!(".{name}.last-used")))
}

/// The size of `path` in bytes, everything under it included; symbolic
/// links count as themselves, never followed.
fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

/// [`prune`] as of `now`, so the ages can be tested.
fn prune_at(
    dir: &Path,
    max_age: Duration,
    dry_run: bool,
    now: SystemTime,
) -> std::io::Result<Pruned> {
    let max_age = max_age.max(MIN_PRUNE_AGE);
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Pruned::default()),
        Err(e) => return Err(e),
    };
    let mut pruned = Pruned::default();
    for entry in entries.flatten() {
        let path = entry.path();
        // Stamps, and a DiskCache's half-written `.tmp`, are not
        // entries; a stamp whose entry is gone is dropped.
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(of) = name
            .strip_prefix('.')
            .and_then(|n| n.strip_suffix(".last-used"))
        {
            if !dry_run && !dir.join(of).exists() {
                let _ = std::fs::remove_file(&path);
            }
            continue;
        }
        if name.starts_with('.') || name.ends_with(".tmp") {
            continue;
        }
        let bytes = size_of(&path);
        let idle = last_used(&path).and_then(|t| now.duration_since(t).ok());
        match idle {
            Some(idle) if idle > max_age => {
                if !dry_run {
                    if entry.file_type()?.is_dir() {
                        std::fs::remove_dir_all(&path)?;
                    } else {
                        std::fs::remove_file(&path)?;
                    }
                    if let Some(stamp) = stamp_of(&path) {
                        let _ = std::fs::remove_file(stamp);
                    }
                }
                pruned.removed.push(Expired { path, bytes, idle });
            }
            _ => {
                pruned.kept += 1;
                pruned.kept_bytes += bytes;
            }
        }
    }
    pruned.removed.sort_by_key(|e| std::cmp::Reverse(e.idle));
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: Duration = Duration::from_secs(24 * 60 * 60);

    /// Set `path`'s modification time to `ago` before now.
    fn backdate(path: &Path, ago: Duration) {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(SystemTime::now() - ago)
            .unwrap();
    }

    #[test]
    fn unused_entries_are_pruned_whole_and_used_ones_kept() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        // A clone-like directory fetched long ago but used today...
        std::fs::create_dir_all(d.join("osmo/.git/objects")).unwrap();
        std::fs::write(d.join("osmo/.git/objects/pack"), "12345").unwrap();
        backdate(&d.join("osmo"), 90 * DAY);
        mark_used(&d.join("osmo"));
        // ...one never marked and 40 days old...
        std::fs::create_dir_all(d.join("orage")).unwrap();
        std::fs::write(d.join("orage/spec"), "123").unwrap();
        backdate(&d.join("orage"), 40 * DAY);
        // ...and a DiskCache file, aged by when it was written.
        std::fs::write(d.join("nvd-abc.txt"), "{}").unwrap();
        backdate(&d.join("nvd-abc.txt"), 40 * DAY);
        // The stamp lives beside the entry, never inside it.
        assert!(d.join(".osmo.last-used").exists());
        assert!(!d.join("osmo/.last-used").exists());

        // A dry run reports and removes nothing.
        let p = prune(d, 30 * DAY, true).unwrap();
        assert_eq!(p.removed.len(), 2);
        assert!(d.join("orage/spec").exists());
        let orage = p
            .removed
            .iter()
            .find(|e| e.path.ends_with("orage"))
            .unwrap();
        assert_eq!(orage.bytes, 3);
        assert!(orage.idle >= 39 * DAY);

        let p = prune(d, 30 * DAY, false).unwrap();
        assert_eq!(p.removed.len(), 2);
        assert_eq!((p.kept, p.kept_bytes), (1, 5));
        assert!(d.join("osmo/.git/objects/pack").exists());
        assert!(!d.join("orage").exists() && !d.join("nvd-abc.txt").exists());

        // Forty days on, osmo has gone unused too, and goes with its stamp.
        let p = prune_at(d, 30 * DAY, false, SystemTime::now() + 40 * DAY).unwrap();
        assert_eq!(p.removed.len(), 1);
        assert!(!d.join("osmo").exists() && !d.join(".osmo.last-used").exists());
    }

    #[test]
    fn prune_never_takes_an_entry_used_within_a_day() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir(d.join("distgit-pkg")).unwrap();
        mark_used(&d.join("distgit-pkg"));
        // A zero age is raised to a day: a run using it keeps it.
        let p = prune_at(d, Duration::ZERO, false, SystemTime::now() + DAY / 2).unwrap();
        assert!(p.removed.is_empty());
        assert!(d.join("distgit-pkg").exists());
        // An orphaned stamp is dropped; a missing cache is empty.
        std::fs::write(d.join(".gone.last-used"), "").unwrap();
        prune_at(d, DAY, false, SystemTime::now()).unwrap();
        assert!(!d.join(".gone.last-used").exists());
        let none = prune(&d.join("nope"), DAY, false).unwrap();
        assert_eq!((none.removed.len(), none.kept), (0, 0));
    }

    #[test]
    fn round_trips_honours_refresh_and_ages() {
        let dir = tempfile::tempdir().unwrap();
        let day = Some(Duration::from_secs(86_400));
        let disk = DiskCache::at(Some(dir.path().to_path_buf()), false, day);
        assert_eq!(disk.load("k"), None);
        disk.store("k", "body");
        assert_eq!(disk.load("k").as_deref(), Some("body"));
        assert!(!dir.path().join("k.tmp").exists());
        // --refresh reads nothing, but still stores.
        let fresh = DiskCache::at(Some(dir.path().to_path_buf()), true, day);
        assert_eq!(fresh.load("k"), None);
        // An entry older than its life is a miss.
        let aged = DiskCache::at(Some(dir.path().to_path_buf()), false, Some(Duration::ZERO));
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(aged.load("k"), None);
        // No directory: nothing stored, nothing read, no error.
        let none = DiskCache::at(None, false, None);
        none.store("k", "{}");
        assert_eq!(none.load("k"), None);
    }

    #[test]
    fn url_keys_are_stable_safe_and_distinct() {
        let key = url_key("https://cert-portal.siemens.com/productcert/html/ssa-032379.html");
        assert!(
            key.starts_with("cert-portal.siemens.com-") && key.ends_with(".txt"),
            "{key}"
        );
        assert_eq!(
            key,
            url_key("https://cert-portal.siemens.com/productcert/html/ssa-032379.html")
        );
        assert_ne!(
            key,
            url_key("https://cert-portal.siemens.com/productcert/html/ssa-082556.html")
        );
        assert!(!url_key("https://a/b?c=1&d=/x").contains(['/', '?', '&']));
    }
}
