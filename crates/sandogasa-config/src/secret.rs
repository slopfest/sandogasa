// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Secrets in the desktop keyring rather than in a config file (#79).
//!
//! A token kept in `~/.config/<tool>/config.toml` is readable by anything
//! running as the user, lands in backups and dotfile syncs, and was once
//! printed by an assistant reading the file. The keyring (Secret Service,
//! through libsecret's `secret-tool`) keeps it encrypted at rest and
//! unlocked with the login session.
//!
//! An entry stands for a service, not a tool: the GitLab token for
//! `gitlab.com` is one entry every tool that talks to gitlab.com reads,
//! so rotating it is one `store`. Its attributes are `application
//! sandogasa`, `service <API host>`, and `account <who>` for a service
//! that needs one, which is what `secret-tool lookup` takes to find it
//! by hand ([`SecretRef::lookup_command`]).
//!
//! `secret-tool` is shelled out to, not linked: it is packaged wherever
//! the tools are (Fedora, EPEL 9 and 10, Debian's libsecret-tools), adds
//! no dependency to the workspace, and is the command a person uses to
//! check what is stored. The secret goes to it on stdin, never on a
//! command line, and nothing here prints one.

use std::io::Write;
use std::process::{Command, Stdio};

/// The `application` attribute every entry carries.
pub const APPLICATION: &str = "sandogasa";

/// One secret in the keyring: the service it unlocks, and the account
/// when the service needs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    service: String,
    account: Option<String>,
    label: String,
}

/// Why the keyring could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretError {
    /// No keyring to use: `secret-tool` is missing, or no Secret Service
    /// answers (a headless machine, no login session).
    Unavailable(String),
    /// The keyring answered, but the operation failed.
    Failed(String),
}

/// Where a secret was kept by [`keep`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    Keyring,
    /// In the config file, because the keyring could not be used.
    File(String),
}

/// A keyring: `secret-tool` in use, an in-memory one in tests.
pub trait SecretStore {
    /// The secret, or `None` when there is no such entry.
    fn lookup(&self, secret: &SecretRef) -> Result<Option<String>, SecretError>;
    /// Store `value` as the secret, replacing any there.
    fn store(&self, secret: &SecretRef, value: &str) -> Result<(), SecretError>;
    /// Remove the entry, if there is one.
    fn clear(&self, secret: &SecretRef) -> Result<(), SecretError>;
}

/// The keyring through libsecret's `secret-tool`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretTool;

/// A keyring in memory, for tests that must not touch the user's.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: std::sync::Mutex<Vec<(SecretRef, String)>>,
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::Unavailable(why) => write!(f, "no keyring to use: {why}"),
            SecretError::Failed(why) => write!(f, "the keyring refused: {why}"),
        }
    }
}

impl std::error::Error for SecretError {}

impl SecretRef {
    /// The secret for `service` (the API host, `gitlab.com`), shown in
    /// keyring browsers as `label`.
    pub fn new(service: &str, label: &str) -> Self {
        Self {
            service: service.to_string(),
            account: None,
            label: label.to_string(),
        }
    }

    /// The same, for one `account` of the service.
    pub fn account(mut self, account: &str) -> Self {
        self.account = Some(account.to_string());
        self
    }

    /// The secret for a service reached at `base_url`: its host, with
    /// the port when it has one.
    pub fn for_url(base_url: &str, label: &str) -> Self {
        let rest = base_url.split_once("://").map_or(base_url, |(_, r)| r);
        let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let host = host.rsplit('@').next().unwrap_or(host);
        Self::new(&host.to_ascii_lowercase(), label)
    }

    /// The service this unlocks.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// The attributes that identify the entry, in order.
    pub fn attributes(&self) -> Vec<(&'static str, &str)> {
        let mut attrs = vec![("application", APPLICATION), ("service", &self.service)];
        if let Some(a) = &self.account {
            attrs.push(("account", a));
        }
        attrs
    }

    /// `secret-tool lookup …` for this entry, to check it by hand.
    pub fn lookup_command(&self) -> String {
        let mut cmd = String::from("secret-tool lookup");
        for (k, v) in self.attributes() {
            cmd.push(' ');
            cmd.push_str(k);
            cmd.push(' ');
            cmd.push_str(&quote(v));
        }
        cmd
    }

    /// Whether this is a service on this machine, as a test server is:
    /// such a service never has an entry, so it is not looked up.
    fn is_local(&self) -> bool {
        let host = self
            .service
            .rsplit_once(':')
            .map_or(&*self.service, |(h, p)| {
                if p.chars().all(|c| c.is_ascii_digit()) {
                    h
                } else {
                    &self.service
                }
            });
        let host = host.trim_start_matches('[').trim_end_matches(']');
        host == "localhost" || host == "::1" || host.starts_with("127.")
    }
}

impl SecretTool {
    fn run(&self, verb: &str, secret: &SecretRef, label: bool) -> Command {
        let mut cmd = Command::new("secret-tool");
        cmd.arg(verb);
        if label {
            cmd.arg(format!("--label={}", secret.label));
        }
        for (k, v) in secret.attributes() {
            cmd.args([k, v]);
        }
        cmd
    }
}

impl SecretStore for SecretTool {
    fn lookup(&self, secret: &SecretRef) -> Result<Option<String>, SecretError> {
        if secret.is_local() {
            return Ok(None);
        }
        let out = self
            .run("lookup", secret, false)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SecretError::Unavailable(format!("cannot run secret-tool: {e}")))?;
        lookup_answer(
            out.status.success(),
            &String::from_utf8_lossy(&out.stdout),
            &String::from_utf8_lossy(&out.stderr),
        )
    }

    fn store(&self, secret: &SecretRef, value: &str) -> Result<(), SecretError> {
        let mut child = self
            .run("store", secret, true)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SecretError::Unavailable(format!("cannot run secret-tool: {e}")))?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(value.as_bytes()).map_err(|e| {
                SecretError::Failed(format!("cannot hand secret-tool the secret: {e}"))
            })?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(unavailable_or_failed(&String::from_utf8_lossy(&out.stderr)))
        }
    }

    fn clear(&self, secret: &SecretRef) -> Result<(), SecretError> {
        let out = self
            .run("clear", secret, false)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| SecretError::Unavailable(format!("cannot run secret-tool: {e}")))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        // Nothing to clear is not a failure.
        if out.status.success() || stderr.trim().is_empty() {
            Ok(())
        } else {
            Err(unavailable_or_failed(&stderr))
        }
    }
}

impl SecretStore for MemoryStore {
    fn lookup(&self, secret: &SecretRef) -> Result<Option<String>, SecretError> {
        let entries = self.entries.lock().expect("store lock");
        Ok(entries
            .iter()
            .find(|(r, _)| r.attributes() == secret.attributes())
            .map(|(_, v)| v.clone()))
    }

    fn store(&self, secret: &SecretRef, value: &str) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("store lock");
        entries.retain(|(r, _)| r.attributes() != secret.attributes());
        entries.push((secret.clone(), value.to_string()));
        Ok(())
    }

    fn clear(&self, secret: &SecretRef) -> Result<(), SecretError> {
        let mut entries = self.entries.lock().expect("store lock");
        entries.retain(|(r, _)| r.attributes() != secret.attributes());
        Ok(())
    }
}

/// What `secret-tool lookup` exiting so means: the secret on success, no
/// entry on a failure that says nothing, an error otherwise.
pub fn lookup_answer(
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<Option<String>, SecretError> {
    if success {
        let value = stdout.trim_end_matches(['\n', '\r']);
        return Ok((!value.is_empty()).then(|| value.to_string()));
    }
    if stderr.trim().is_empty() {
        Ok(None)
    } else {
        Err(unavailable_or_failed(stderr))
    }
}

/// The secret, first found of: the environment variable `env` (when
/// set and not empty), the keyring, then `from_file` (the config
/// file's value). A keyring that cannot be used is passed over.
pub fn resolve(
    store: &dyn SecretStore,
    env: Option<&str>,
    secret: &SecretRef,
    from_file: Option<&str>,
) -> Option<String> {
    if let Some(v) = env
        .and_then(|e| std::env::var(e).ok())
        .filter(|v| !v.is_empty())
    {
        return Some(v);
    }
    if let Ok(Some(v)) = store.lookup(secret) {
        return Some(v);
    }
    from_file.filter(|v| !v.is_empty()).map(str::to_string)
}

/// Keep `value` as the secret: in the keyring when there is one, telling
/// the caller to leave it out of the config file; otherwise to write it
/// there as before, with the reason.
pub fn keep(store: &dyn SecretStore, secret: &SecretRef, value: &str) -> Kept {
    match store.store(secret, value) {
        Ok(()) => Kept::Keyring,
        Err(e) => Kept::File(e.to_string()),
    }
}

/// [`keep`], telling the person where `what` went: in the keyring, with
/// how to check it by hand, or in the config file, with why. Returns
/// what goes in the file: empty when the keyring has it.
pub fn keep_for_file(
    store: &dyn SecretStore,
    what: &str,
    secret: &SecretRef,
    value: String,
) -> String {
    match keep(store, secret, &value) {
        Kept::Keyring => {
            eprintln!(
                "{what}: kept in the keyring, not the config file (check it with `{}`).",
                secret.lookup_command()
            );
            String::new()
        }
        Kept::File(why) => {
            eprintln!("{what}: kept in the config file ({why}).");
            value
        }
    }
}

/// What a failed `secret-tool` said: no Secret Service is
/// [`SecretError::Unavailable`], anything else a failure.
fn unavailable_or_failed(stderr: &str) -> SecretError {
    let why = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("secret-tool failed")
        .to_string();
    let lower = why.to_ascii_lowercase();
    if [
        "d-bus",
        "dbus",
        "secret service",
        "org.freedesktop.secrets",
        "autolaunch",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        SecretError::Unavailable(why)
    } else {
        SecretError::Failed(why)
    }
}

/// `arg`, quoted for the shell when it needs it.
fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-/:@".contains(c))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_is_named_by_its_service_and_found_by_hand_the_same_way() {
        let gitlab = SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token");
        assert_eq!(gitlab.service(), "gitlab.com");
        assert_eq!(
            gitlab.lookup_command(),
            "secret-tool lookup application sandogasa service gitlab.com"
        );
        let jira =
            SecretRef::for_url("https://redhat.atlassian.net/rest", "l").account("me@example.com");
        assert_eq!(
            jira.attributes(),
            [
                ("application", "sandogasa"),
                ("service", "redhat.atlassian.net"),
                ("account", "me@example.com"),
            ]
        );
        assert!(SecretRef::for_url("http://127.0.0.1:4321", "l").is_local());
        assert!(SecretRef::for_url("http://localhost:80/x", "l").is_local());
        assert!(!gitlab.is_local());
    }

    #[test]
    fn secret_tools_answers_are_told_apart() {
        assert_eq!(
            lookup_answer(true, "glpat-x\n", ""),
            Ok(Some("glpat-x".into()))
        );
        // Not found: exit 1, nothing said.
        assert_eq!(lookup_answer(false, "", ""), Ok(None));
        assert!(matches!(
            lookup_answer(false, "", "Cannot autolaunch D-Bus without X11 $DISPLAY\n"),
            Err(SecretError::Unavailable(_))
        ));
        assert!(matches!(
            lookup_answer(false, "", "secret-tool: Prompt dismissed\n"),
            Err(SecretError::Failed(_))
        ));
    }

    #[test]
    fn the_environment_wins_then_the_keyring_then_the_file() {
        let store = MemoryStore::default();
        let r = SecretRef::new("gitlab.example", "l");
        assert_eq!(
            resolve(&store, None, &r, Some("from-file")).as_deref(),
            Some("from-file")
        );
        assert_eq!(keep(&store, &r, "from-keyring"), Kept::Keyring);
        assert_eq!(
            resolve(&store, None, &r, Some("from-file")).as_deref(),
            Some("from-keyring")
        );
        // An unset or empty variable is passed over.
        assert_eq!(
            resolve(&store, Some("SANDOGASA_TEST_UNSET_VARIABLE"), &r, None).as_deref(),
            Some("from-keyring")
        );
        store.clear(&r).unwrap();
        assert_eq!(resolve(&store, None, &r, Some("")), None);
    }

    /// A keyring that cannot be used leaves the secret in the file.
    #[test]
    fn without_a_keyring_the_secret_stays_in_the_file() {
        struct Absent;
        impl SecretStore for Absent {
            fn lookup(&self, _: &SecretRef) -> Result<Option<String>, SecretError> {
                Err(SecretError::Unavailable("no Secret Service".into()))
            }
            fn store(&self, _: &SecretRef, _: &str) -> Result<(), SecretError> {
                Err(SecretError::Unavailable("no Secret Service".into()))
            }
            fn clear(&self, _: &SecretRef) -> Result<(), SecretError> {
                Ok(())
            }
        }
        let r = SecretRef::new("gitlab.example", "l");
        assert_eq!(
            keep(&Absent, &r, "t"),
            Kept::File("no keyring to use: no Secret Service".into())
        );
        assert_eq!(resolve(&Absent, None, &r, Some("t")).as_deref(), Some("t"));
    }
}
