// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Helpers shared by this crate's tests.

/// RAII guard: sets env vars on construction, restores the
/// original values (or unsets if absent) on drop. Callers are
/// responsible for serializing against parallel tests
/// (typically via `#[serial_test::serial]`).
pub struct EnvGuard {
    originals: Vec<(String, Option<String>)>,
}

impl EnvGuard {
    pub fn new(vars: &[(&str, &str)]) -> Self {
        let mut originals = Vec::with_capacity(vars.len());
        for (k, v) in vars {
            originals.push(((*k).to_string(), std::env::var(k).ok()));
            // SAFETY: callers serialize with #[serial_test::serial]
            // so no other thread concurrently reads these vars.
            unsafe {
                std::env::set_var(k, v);
            }
        }
        Self { originals }
    }

    /// Also unset `vars` for the guard's lifetime, restoring them on
    /// drop: for credentials a developer may have exported, which
    /// must not reach a test.
    pub fn unset(mut self, vars: &[&str]) -> Self {
        for k in vars {
            self.originals
                .push(((*k).to_string(), std::env::var(k).ok()));
            // SAFETY: callers serialize with #[serial_test::serial].
            unsafe {
                std::env::remove_var(k);
            }
        }
        self
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (k, v) in self.originals.iter().rev() {
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
