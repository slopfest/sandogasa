// SPDX-License-Identifier: Apache-2.0 OR MIT

//! The CBS login, checked before anything is written to CBS.
//!
//! CBS's `cbs` koji profile logs in with a TLS client certificate
//! (`cert = ~/.centos.cert`), which `centos-cert -u <user>` makes from a
//! Fedora Kerberos ticket. Without it every write fails with koji's
//! traceback ("TLSV13_ALERT_CERTIFICATE_REQUIRED", "GSSAPIAuthError"),
//! which says nothing of the certificate or how to get one. So the login
//! is tried first, with one call, and a failure is explained: what is
//! wrong, and the commands that fix it, offered to run at a terminal.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::PROFILE;

/// Why the login failed, as far as can be told.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    /// The certificate the profile names does not exist.
    MissingCert(PathBuf),
    /// It exists, but `centos-cert -v` does not accept it.
    InvalidCert(PathBuf),
    /// Something else: koji's own explanation.
    Other(String),
}

/// The Fedora account a Kerberos principal belongs to: `salimma` for
/// `salimma@FEDORAPROJECT.ORG`, from `klist`'s "Default principal" line.
pub fn fedora_user(klist: &str) -> Option<String> {
    klist.lines().find_map(|l| {
        let principal = l.trim().strip_prefix("Default principal:")?.trim();
        let (user, realm) = principal.split_once('@')?;
        (realm == "FEDORAPROJECT.ORG" && !user.is_empty()).then(|| user.to_string())
    })
}

/// What failed, from koji's output and the certificate's state.
pub fn diagnose(koji_output: &str, cert: Option<&Path>, cert_valid: Option<bool>) -> Failure {
    match (cert, cert_valid) {
        (Some(c), _) if !c.exists() => Failure::MissingCert(c.to_path_buf()),
        (Some(c), Some(false)) => Failure::InvalidCert(c.to_path_buf()),
        _ => Failure::Other(
            koji_output
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("koji gave no reason")
                .to_string(),
        ),
    }
}

/// The commands that make a certificate, for `user`.
pub fn fix_commands(user: Option<&str>) -> [String; 2] {
    let user = user.unwrap_or("<your-FAS-username>");
    [
        format!("kinit {user}@FEDORAPROJECT.ORG"),
        format!("centos-cert -u {user}"),
    ]
}

/// What went wrong and how to fix it, spelled out for someone who does
/// not log in to CBS every day.
pub fn explain(failure: &Failure, user: Option<&str>) -> String {
    let [kinit, cert] = fix_commands(user);
    let cause = match failure {
        Failure::MissingCert(p) => format!(
            "the certificate it logs in with, {}, does not exist",
            p.display()
        ),
        Failure::InvalidCert(p) => format!(
            "the certificate it logs in with, {}, is not valid any more (expired, or for \
             another account)",
            p.display()
        ),
        Failure::Other(why) => format!("koji says: {why}"),
    };
    format!(
        "CBS refused the login: {cause}.\n\n\
         CBS logs in with a TLS certificate made from your Fedora account. To get one:\n\
         \x20 {kinit}    (only if `klist` shows no valid ticket)\n\
         \x20 {cert}\n\
         then run this again. `centos-cert -v` checks the certificate."
    )
}

/// Whether CBS accepts the login, with koji's output when not.
fn login() -> Result<(), String> {
    let out = Command::new("koji")
        .args(["--profile", PROFILE, "moshimoshi"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run koji: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{}{}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout)
        ))
    }
}

/// Whether `centos-cert -v` accepts the certificate; `None` when it
/// cannot be run.
fn cert_valid() -> Option<bool> {
    Command::new("centos-cert")
        .arg("-v")
        .stdin(Stdio::null())
        .output()
        .ok()
        .map(|o| o.status.success())
}

/// The Fedora account the Kerberos ticket cache names.
fn kerberos_user() -> Option<String> {
    let out = Command::new("klist").output().ok()?;
    fedora_user(&String::from_utf8_lossy(&out.stdout))
}

/// Make sure CBS accepts the login before anything is written to it. A
/// failure is explained, and at a terminal (`interactive`) the fix is
/// offered and the login tried again; otherwise the explanation is the
/// error. `tool` names the caller in the `verbose` progress line. Once
/// accepted, the login is not checked again in the same process.
pub fn ensure_login(tool: &str, interactive: bool, verbose: bool) -> Result<(), String> {
    // Once per run: a command that writes to CBS in several steps (a
    // build that uploads sources first) asks once, not once a step.
    if LOGGED_IN.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(());
    }
    let result = check_login(tool, interactive, verbose);
    if result.is_ok() {
        LOGGED_IN.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    result
}

/// Whether [`ensure_login`] has seen the login accepted in this process.
static LOGGED_IN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// [`ensure_login`]'s check itself.
fn check_login(tool: &str, interactive: bool, verbose: bool) -> Result<(), String> {
    if verbose {
        eprintln!("[{tool}] checking the CBS login (koji --profile {PROFILE} moshimoshi)");
    }
    let Err(output) = login() else {
        return Ok(());
    };
    let cert = sandogasa_kojihub::config::cert_path(PROFILE);
    let failure = diagnose(
        &output,
        cert.as_deref(),
        cert.as_ref()
            .filter(|c| c.exists())
            .and_then(|_| cert_valid()),
    );
    let user = kerberos_user();
    let explanation = explain(&failure, user.as_deref());
    let can_fix = matches!(failure, Failure::MissingCert(_) | Failure::InvalidCert(_));
    if !(interactive && can_fix && std::io::stdin().is_terminal()) {
        return Err(explanation);
    }
    eprintln!("{explanation}\n");
    let [_, cert_cmd] = fix_commands(user.as_deref());
    let Some(user) = user else {
        return Err(
            "no Fedora Kerberos ticket to make the certificate from: run the kinit above first"
                .to_string(),
        );
    };
    let go = sandogasa_cli::confirm(&format!("Run `{cert_cmd}` now?"), true)
        .map_err(|e| e.to_string())?;
    if !go {
        return Err("CBS login not fixed".to_string());
    }
    let status = Command::new("centos-cert")
        .args(["-u", &user])
        .status()
        .map_err(|e| format!("cannot run centos-cert: {e}"))?;
    if !status.success() {
        return Err(format!(
            "centos-cert failed; if it asked for a Kerberos ticket, run `kinit \
             {user}@FEDORAPROJECT.ORG` and try again"
        ));
    }
    login().map_err(|output| explain(&diagnose(&output, None, None), Some(&user)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fedora_account_is_read_from_klist() {
        let klist = "Ticket cache: KEYRING:persistent:1000:1000\n\
                     Default principal: salimma@FEDORAPROJECT.ORG\n\n\
                     Valid starting     Expires            Service principal\n";
        assert_eq!(fedora_user(klist).as_deref(), Some("salimma"));
        assert_eq!(fedora_user("Default principal: x@REDHAT.COM\n"), None);
        assert_eq!(fedora_user("klist: No credentials cache found\n"), None);
    }

    #[test]
    fn a_missing_certificate_is_named_with_the_commands_that_make_one() {
        let koji = "2026-11-09 17:50:13,558 [ERROR] koji: (gssapi auth failed: \
                    requests.exceptions.SSLError: … TLSV13_ALERT_CERTIFICATE_REQUIRED …)\n";
        let missing = Path::new("/nonexistent/home/.centos.cert");
        let failure = diagnose(koji, Some(missing), None);
        assert_eq!(failure, Failure::MissingCert(missing.to_path_buf()));
        let text = explain(&failure, Some("salimma"));
        assert!(
            text.contains("/nonexistent/home/.centos.cert, does not exist"),
            "{text}"
        );
        assert!(text.contains("  kinit salimma@FEDORAPROJECT.ORG"), "{text}");
        assert!(text.contains("  centos-cert -u salimma\n"), "{text}");
        // No ticket to take the account from: a placeholder.
        assert!(explain(&failure, None).contains("centos-cert -u <your-FAS-username>"));
    }

    #[test]
    fn a_rejected_certificate_and_other_failures_are_told_apart() {
        let here = std::env::current_dir().unwrap();
        assert_eq!(
            diagnose("x", Some(&here), Some(false)),
            Failure::InvalidCert(here.clone())
        );
        assert_eq!(
            diagnose("\n  koji: hub unreachable\nmore", Some(&here), Some(true)),
            Failure::Other("koji: hub unreachable".into())
        );
        assert!(
            explain(&Failure::InvalidCert(here), Some("salimma")).contains("not valid any more")
        );
    }
}
