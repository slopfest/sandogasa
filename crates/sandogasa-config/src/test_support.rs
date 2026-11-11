// SPDX-License-Identifier: Apache-2.0 OR MIT

//! What the tools' tests share, behind the `test-support` feature: a
//! tool lists `sandogasa-config` again under `[dev-dependencies]` with
//! `features = ["test-support"]`.

/// Sets (or unsets) environment variables for its lifetime, restoring
/// what was there when dropped. Callers serialise the tests that use
/// it with `#[serial_test::serial]`, as the environment is the
/// process's.
///
/// A guard that moves the user's config or data (`XDG_CONFIG_HOME`,
/// `XDG_DATA_HOME` or `HOME`) moves the system layer beneath it too,
/// unless the variable relocating that layer is among those given: it
/// points `SANDOGASA_ETC` (for `/etc/<tool>/`) or `SANDOGASA_DATADIR`
/// (for `/usr/share/<tool>/`) at a directory that does not exist
/// beneath the new home, so a test reads nothing the machine running
/// it has installed (#119, #127). Given `HOME` without a layer's XDG
/// variable, it also unsets that variable, which would otherwise keep
/// the user's files where a developer exported them.
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
        for layer in LAYERS {
            if has("HOME") && !has(layer.user) {
                guard.remember(layer.user);
                // SAFETY: as above.
                unsafe { std::env::remove_var(layer.user) };
            }
            if let Some(dir) = isolated(vars, layer) {
                guard.remember(layer.system);
                // SAFETY: as above.
                unsafe { std::env::set_var(layer.system, dir) };
            }
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

/// A per-user directory and the system layer beneath it.
struct Layer {
    /// The XDG variable naming the user's directory.
    user: &'static str,
    /// The variable relocating the system layer.
    system: &'static str,
    /// Where a guard puts the system layer, beneath the user's home.
    missing: &'static str,
}

/// Config over `/etc`, as `ConfigFile`'s layered loads read it.
const CONFIG: Layer = Layer {
    user: "XDG_CONFIG_HOME",
    system: crate::SYSTEM_ROOT_VAR,
    missing: "no-system-config",
};

/// Data over `/usr/share`, as [`crate::system_data_root`] reads it.
const DATA: Layer = Layer {
    user: "XDG_DATA_HOME",
    system: crate::SYSTEM_DATA_ROOT_VAR,
    missing: "no-system-data",
};

/// The layers a guard isolates.
const LAYERS: &[Layer] = &[CONFIG, DATA];

/// Where a guard setting `vars` puts `layer`'s system side: nowhere
/// that exists, beneath the home it moves the user's side to, or
/// `None` when it moves neither or relocates the system side itself.
fn isolated(vars: &[(&str, &str)], layer: &Layer) -> Option<String> {
    if vars.iter().any(|(k, _)| *k == layer.system) {
        return None;
    }
    let home = [layer.user, "HOME"]
        .iter()
        .find_map(|name| vars.iter().find(|(k, _)| k == name))?;
    Some(format!("{}/{}", home.1, layer.missing))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn etc(vars: &[(&str, &str)]) -> Option<String> {
        isolated(vars, &CONFIG)
    }

    fn data(vars: &[(&str, &str)]) -> Option<String> {
        isolated(vars, &DATA)
    }

    #[test]
    fn moving_a_home_moves_the_system_layer_beneath_it() {
        assert_eq!(
            etc(&[("XDG_CONFIG_HOME", "/tmp/x/config"), ("PATH", "/bin")]).as_deref(),
            Some("/tmp/x/config/no-system-config")
        );
        assert_eq!(
            etc(&[("HOME", "/tmp/x/home")]).as_deref(),
            Some("/tmp/x/home/no-system-config")
        );
        // The XDG variable is where the user's files are read from when
        // both are set.
        assert_eq!(
            etc(&[("HOME", "/h"), ("XDG_CONFIG_HOME", "/c")]).as_deref(),
            Some("/c/no-system-config")
        );
        assert_eq!(
            data(&[("HOME", "/h"), ("XDG_DATA_HOME", "/d")]).as_deref(),
            Some("/d/no-system-data")
        );
        assert_eq!(
            data(&[("HOME", "/h")]).as_deref(),
            Some("/h/no-system-data")
        );
        // Moving the config home says nothing about data, and back.
        assert_eq!(data(&[("XDG_CONFIG_HOME", "/c")]), None);
        assert_eq!(etc(&[("XDG_DATA_HOME", "/d")]), None);
    }

    #[test]
    fn a_guard_that_sets_the_system_layer_or_no_home_is_left_alone() {
        assert_eq!(
            etc(&[
                ("XDG_CONFIG_HOME", "/c"),
                ("SANDOGASA_ETC", "/repo/configs")
            ]),
            None
        );
        assert_eq!(
            data(&[("HOME", "/h"), ("SANDOGASA_DATADIR", "/repo/data")]),
            None
        );
        assert_eq!(etc(&[("GITLAB_TOKEN", "t")]), None);
    }
}
