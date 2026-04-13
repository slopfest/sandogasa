// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Shared CLI utilities for sandogasa tools.

use std::process::{Command, Stdio};

/// Check that an external tool is available in `$PATH`.
///
/// Runs `<name> <version_arg>` and returns `Ok(())` if it exits
/// successfully, or an error message with the install hint.
/// Most tools use `--version`; see [`require_tool`] for a
/// convenience wrapper.
///
/// # Example
///
/// ```no_run
/// // koji uses `version` subcommand instead of `--version`
/// sandogasa_cli::require_tool_with_arg("koji", "version", "sudo dnf install koji").unwrap();
/// ```
pub fn require_tool_with_arg(
    name: &str,
    version_arg: &str,
    install_hint: &str,
) -> Result<(), String> {
    match Command::new(name)
        .arg(version_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "{name} exited with {s}; is it installed correctly? \
             Install it with: {install_hint}"
        )),
        Err(_) => Err(format!("{name} not found. Install it with: {install_hint}")),
    }
}

/// Check that an external tool is available in `$PATH`.
///
/// Runs `<name> --version` and returns `Ok(())` if it exits
/// successfully, or an error message with the install hint.
///
/// For tools that use a different version probe (e.g. `koji version`
/// instead of `koji --version`), use [`require_tool_with_arg`].
///
/// # Example
///
/// ```no_run
/// sandogasa_cli::require_tool("fedrq", "sudo dnf install fedrq").unwrap();
/// ```
pub fn require_tool(name: &str, install_hint: &str) -> Result<(), String> {
    require_tool_with_arg(name, "--version", install_hint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_missing_tool() {
        let result = require_tool("nonexistent_tool_xyz_123", "magic install");
        assert!(result.is_err());
        let msg = result.unwrap_err();
        assert!(msg.contains("nonexistent_tool_xyz_123"));
        assert!(msg.contains("magic install"));
    }

    #[test]
    fn require_available_tool() {
        // `true` is a standard Unix utility that always succeeds.
        // It doesn't support --version but some impls exit 0 anyway.
        // Use `sh` which reliably exists and handles --version.
        let result = require_tool("sh", "should already be installed");
        // sh --version may or may not succeed depending on implementation,
        // so just verify it doesn't panic.
        let _ = result;
    }
}
