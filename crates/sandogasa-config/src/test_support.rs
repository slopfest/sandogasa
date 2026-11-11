// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the tools' tests share, behind the `test-support` feature: a
//! tool lists `sandogasa-config` again under `[dev-dependencies]` with
//! `features = ["test-support"]`.

/// Sets (or unsets) environment variables for its lifetime, restoring
/// what was there when dropped. Callers serialise the tests that use
/// it with `#[serial_test::serial]`, as the environment is the
/// process's.
///
/// A guard that moves the user's config (`XDG_CONFIG_HOME` or `HOME`)
/// moves the system layer too, unless `SANDOGASA_ETC` is among the
/// variables given: it points `SANDOGASA_ETC` at a directory that does
/// not exist beneath the new config home, so a layered load reads no
/// `/etc/<tool>/config.toml` from the machine running the tests (#119).
/// One given `HOME` alone also unsets `XDG_CONFIG_HOME`, which would
/// otherwise keep the user's config where a developer exported it.
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
        let has = |name: &str| vars.iter().any(|(k, _)| *k == name);
        if has("HOME") && !has("XDG_CONFIG_HOME") {
            guard.remember("XDG_CONFIG_HOME");
            // SAFETY: as above.
            unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
        }
        if let Some(etc) = isolated_etc(vars) {
            guard.remember(SYSTEM_ROOT_VAR);
            // SAFETY: as above.
            unsafe { std::env::set_var(SYSTEM_ROOT_VAR, etc) };
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

/// The variable that relocates the system layer, read by
/// `system_root`.
const SYSTEM_ROOT_VAR: &str = "SANDOGASA_ETC";

/// Where a guard setting `vars` puts the system layer: nowhere that
/// exists, beneath the config home it moves the user's config to, or
/// `None` when it moves neither or sets the system layer itself.
fn isolated_etc(vars: &[(&str, &str)]) -> Option<String> {
    if vars.iter().any(|(k, _)| *k == SYSTEM_ROOT_VAR) {
        return None;
    }
    let home = ["XDG_CONFIG_HOME", "HOME"]
        .iter()
        .find_map(|name| vars.iter().find(|(k, _)| k == name))?;
    Some(format!("{}/no-system-config", home.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moving_the_config_home_moves_the_system_layer() {
        assert_eq!(
            isolated_etc(&[("XDG_CONFIG_HOME", "/tmp/x/config"), ("PATH", "/bin")]).as_deref(),
            Some("/tmp/x/config/no-system-config")
        );
        assert_eq!(
            isolated_etc(&[("HOME", "/tmp/x/home")]).as_deref(),
            Some("/tmp/x/home/no-system-config")
        );
        // XDG_CONFIG_HOME is where the user's config is read from when
        // both are set.
        assert_eq!(
            isolated_etc(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")]).as_deref(),
            Some("/c/no-system-config")
        );
    }

    #[test]
    fn a_guard_that_sets_the_system_layer_or_no_home_is_left_alone() {
        assert_eq!(
            isolated_etc(&[
                ("XDG_CONFIG_HOME", "/c"),
                ("SANDOGASA_ETC", "/repo/configs")
            ]),
            None
        );
        assert_eq!(isolated_etc(&[("GITLAB_TOKEN", "t")]), None);
    }
}
