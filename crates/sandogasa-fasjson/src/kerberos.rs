// SPDX-License-Identifier: Apache-2.0 OR MIT

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

/// The command that gets a new ticket for the FAS user `user`:
/// `fkinit -u <user>` when fedora-packager-kerberos is installed
/// (`fkinit`), `kinit <user>@FEDORAPROJECT.ORG` otherwise. For an
/// account with a one-time password, plain kinit asks only "Enter OTP
/// Token Value", which wants the password and the code typed together,
/// and fails preauthentication when given the code alone. fkinit asks
/// for each separately, gets an anonymous armor ticket, and passes the
/// two joined through it.
fn new_ticket_command(user: &str, fkinit: bool) -> (&'static str, Vec<String>) {
    if fkinit {
        ("fkinit", vec!["-u".to_string(), user.to_string()])
    } else {
        ("kinit", vec![format!("{user}@FEDORAPROJECT.ORG")])
    }
}

/// Make sure there is a valid ticket: renew one that has expired but is
/// still renewable or, at a terminal, get a new one for the user in
/// `~/.fedora.upn` (see [`new_ticket_command`]), which asks for the
/// password. Otherwise the error is `remedy`, which says what to do
/// instead (e.g. "run `fkinit -u <you>`").
pub fn ensure_ticket(remedy: &str) -> Result<(), String> {
    let status = ticket_status();
    if status == TicketStatus::Valid
        || (status == TicketStatus::ExpiredRenewable && renew_ticket().is_ok())
    {
        return Ok(());
    }
    let upn = read_fedora_upn()
        .filter(|u| !u.is_empty())
        .ok_or_else(|| remedy.to_string())?;
    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        return Err(remedy.to_string());
    }
    let fkinit = sandogasa_cli::require_tools(&[("fkinit", "", None)]).is_ok();
    let (program, args) = new_ticket_command(&upn, fkinit);
    eprintln!("no Kerberos ticket; running {program} {}", args.join(" "));
    let ran = Command::new(program)
        .args(&args)
        .status()
        .map_err(|e| format!("failed to run {program}: {e}; {remedy}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("{program} failed; {remedy}"))
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
    fn a_new_ticket_comes_from_fkinit_where_it_is_installed() {
        assert_eq!(
            new_ticket_command("alice", true),
            ("fkinit", vec!["-u".to_string(), "alice".to_string()])
        );
        assert_eq!(
            new_ticket_command("alice", false),
            ("kinit", vec!["alice@FEDORAPROJECT.ORG".to_string()])
        );
    }

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
