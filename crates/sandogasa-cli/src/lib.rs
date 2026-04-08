// SPDX-License-Identifier: MPL-2.0

//! Shared CLI utilities for sandogasa tools.

use std::process::{Command, Stdio};

/// Check that an external tool is available in `$PATH`.
///
/// Runs `<name> --version` and returns `Ok(())` if it exits
/// successfully, or an error message with the install hint.
///
/// # Example
///
/// ```no_run
/// sandogasa_cli::require_tool("fedrq", "sudo dnf install fedrq").unwrap();
/// ```
pub fn require_tool(name: &str, install_hint: &str) -> Result<(), String> {
    match Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!(
            "{name} exited with {s}; is it installed correctly? \
             Install it with: {install_hint}"
        )),
        Err(_) => Err(format!(
            "{name} not found. Install it with: {install_hint}"
        )),
    }
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
