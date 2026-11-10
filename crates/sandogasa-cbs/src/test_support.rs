// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Fake CBS tools for the tests: a temp dir whose `bin` is the whole
//! PATH, so nothing real (`koji`, `centos-cert`, `klist`) can run, and
//! whose `home` is HOME, so no real koji config or certificate is read.
//! Tests using it run under `#[serial_test::serial]`.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// A temp dir standing in for PATH and HOME until dropped.
pub struct FakeTools {
    pub tmp: tempfile::TempDir,
    originals: Vec<(&'static str, Option<String>)>,
}

impl FakeTools {
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        for d in ["bin", "home"] {
            std::fs::create_dir_all(tmp.path().join(d)).unwrap();
        }
        let mut originals = Vec::new();
        for (k, v) in [("PATH", "bin"), ("HOME", "home")] {
            originals.push((k, std::env::var(k).ok()));
            // SAFETY: callers serialize with #[serial_test::serial].
            unsafe { std::env::set_var(k, tmp.path().join(v)) };
        }
        FakeTools { tmp, originals }
    }

    pub fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    /// A `name` on PATH that prints the body of the first case whose
    /// needle its arguments contain and exits 0, or with a body
    /// starting `!`, prints the rest to stderr and exits 1. Anything
    /// else exits 1.
    pub fn fake(&self, name: &str, cases: &[(&str, &str)]) {
        let mut script = String::from("#!/bin/sh\nargs=\"$*\"\n");
        for (needle, body) in cases {
            // Quoted for the shell's printf, a builtin: PATH has no cat.
            let body = body.replace('\'', "'\\''");
            let (out, code) = match body.strip_prefix('!') {
                Some(err) => (format!("printf '%s\\n' '{err}' >&2"), 1),
                None => (format!("printf '%s\\n' '{body}'"), 0),
            };
            script.push_str(&format!(
                "case \"$args\" in *\"{needle}\"*) {out}; exit {code};; esac\n"
            ));
        }
        script.push_str("exit 1\n");
        let path = self.tmp.path().join("bin").join(name);
        write_executable(&path, &script);
    }
}

fn write_executable(path: &Path, script: &str) {
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Drop for FakeTools {
    fn drop(&mut self) {
        for (k, v) in &self.originals {
            // SAFETY: callers serialize env mutations.
            unsafe {
                match v {
                    Some(prev) => std::env::set_var(k, prev),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}
