// SPDX-License-Identifier: MPL-2.0

use std::process::Command;

/// Kerberos ticket status.
#[derive(Debug, PartialEq)]
pub enum TicketStatus {
    /// Valid ticket exists.
    Valid,
    /// Ticket is expired but renewable.
    ExpiredRenewable,
    /// No valid ticket (expired and not renewable, or no ticket at all).
    None,
}

/// Read the Fedora UPN (User Principal Name) from ~/.fedora.upn.
pub fn read_fedora_upn() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(".fedora.upn");
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

/// Check the current Kerberos ticket status.
pub fn ticket_status() -> TicketStatus {
    let output = Command::new("klist").arg("-s").output();

    match output {
        Ok(o) if o.status.success() => TicketStatus::Valid,
        _ => {
            // klist -s failed — check if there's a renewable ticket
            let output = Command::new("klist").output();
            match output {
                Ok(o) => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    if stdout.contains("renew until") {
                        TicketStatus::ExpiredRenewable
                    } else {
                        TicketStatus::None
                    }
                }
                Err(_) => TicketStatus::None,
            }
        }
    }
}

/// Attempt to renew an expired Kerberos ticket.
///
/// Returns `Ok(())` if renewal succeeded, `Err` with a message otherwise.
pub fn renew_ticket() -> Result<(), String> {
    let output = Command::new("kinit")
        .arg("-R")
        .status()
        .map_err(|e| format!("failed to run kinit: {e}"))?;

    if output.success() {
        Ok(())
    } else {
        Err("kinit -R failed — ticket may no longer be renewable".to_string())
    }
}

/// Acquire a new Kerberos ticket for the given principal.
///
/// This runs `kinit` interactively, prompting the user for their password.
pub fn acquire_ticket(principal: &str) -> Result<(), String> {
    let output = Command::new("kinit")
        .arg(principal)
        .status()
        .map_err(|e| format!("failed to run kinit: {e}"))?;

    if output.success() {
        Ok(())
    } else {
        Err("kinit failed — check your password".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_fedora_upn_from_env() {
        // This test depends on the actual file existing.
        // It verifies the function doesn't panic and returns a trimmed string.
        if let Some(upn) = read_fedora_upn() {
            assert!(!upn.is_empty());
            assert!(!upn.ends_with('\n'));
        }
    }
}
