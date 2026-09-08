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

use std::path::PathBuf;
use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;

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
