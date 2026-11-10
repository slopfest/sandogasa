// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the tools' tests share, behind the `test-support` feature: a
//! tool lists `sandogasa-config` again under `[dev-dependencies]` with
//! `features = ["test-support"]`.

/// Sets (or unsets) environment variables for its lifetime, restoring
/// what was there when dropped. Callers serialise the tests that use
/// it with `#[serial_test::serial]`, as the environment is the
/// process's.
pub struct EnvGuard {
    originals: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    pub fn new(vars: &[(&str, &str)]) -> Self {
        let mut guard = Self {
            originals: Vec::with_capacity(vars.len()),
        };
        for (k, v) in vars {
            guard.remember(k);
            // SAFETY: callers serialise with #[serial_test::serial], so
            // no other thread reads these variables meanwhile.
            unsafe { std::env::set_var(k, v) };
        }
        guard
    }

    /// Also unset `vars` for the guard's lifetime: for credentials a
    /// developer may have exported (`GITLAB_TOKEN`), which must not
    /// reach a test.
    pub fn unset(mut self, vars: &[&str]) -> Self {
        for k in vars {
            self.remember(k);
            // SAFETY: as in `new`.
            unsafe { std::env::remove_var(k) };
        }
        self
    }

    fn remember(&mut self, k: &str) {
        if !self.originals.iter().any(|(n, _)| n == k) {
            self.originals.push((k.to_string(), std::env::var(k).ok()));
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in &self.originals {
            // SAFETY: as in `new`.
            unsafe {
                match v {
                    Some(prev) => std::env::set_var(k, prev),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}
