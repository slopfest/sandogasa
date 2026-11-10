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

/// A secret's value in a tool's config file that turns it off for that
/// tool: [`resolve`] then takes only an environment variable, never the
/// keyring, whose entry other tools may still use. What a `config`
/// prompt writes when a secret is cleared.
pub const OFF: &str = "-";

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

    /// The label the entry is shown with, which names the kind of
    /// secret (`sandogasa: GitLab token`) whatever the instance.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// `secret-tool clear …` for this entry, to remove it by hand.
    pub fn clear_command(&self) -> String {
        self.lookup_command()
            .replacen("secret-tool lookup", "secret-tool clear", 1)
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

/// [`SecretTool`] with the program to run named, so the tests can run a
/// stand-in for `secret-tool` rather than reach the user's keyring.
struct SecretToolAt<'a> {
    program: &'a std::ffi::OsStr,
}

impl SecretTool {
    /// The `secret-tool` on PATH.
    fn at() -> SecretToolAt<'static> {
        SecretToolAt {
            program: std::ffi::OsStr::new("secret-tool"),
        }
    }
}

impl SecretStore for SecretTool {
    fn lookup(&self, secret: &SecretRef) -> Result<Option<String>, SecretError> {
        Self::at().lookup(secret)
    }

    fn store(&self, secret: &SecretRef, value: &str) -> Result<(), SecretError> {
        Self::at().store(secret, value)
    }

    fn clear(&self, secret: &SecretRef) -> Result<(), SecretError> {
        Self::at().clear(secret)
    }
}

impl SecretToolAt<'_> {
    fn run(&self, verb: &str, secret: &SecretRef, label: bool) -> Command {
        let mut cmd = Command::new(self.program);
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

impl SecretStore for SecretToolAt<'_> {
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
        // A secret-tool that cannot reach a Secret Service may quit before
        // reading the secret, so the write fails; what it said on exit is
        // then the reason, not the broken pipe.
        let wrote = child
            .stdin
            .take()
            .map(|mut stdin| stdin.write_all(value.as_bytes()));
        let out = child
            .wait_with_output()
            .map_err(|e| SecretError::Failed(e.to_string()))?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        match wrote {
            Some(Err(e)) if stderr.trim().is_empty() => Err(SecretError::Failed(format!(
                "cannot hand secret-tool the secret: {e}"
            ))),
            _ if out.status.success() => Ok(()),
            _ => Err(unavailable_or_failed(&stderr)),
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
    // Turned off for this tool: the shared entry is not its to read.
    if from_file == Some(OFF) {
        return None;
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
    // Off for this tool: the marker stays in its file, and the keyring
    // entry, which is the service's, is left as it is.
    if value == OFF {
        return value;
    }
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

/// The tools that keep a secret under each label, so a tool clearing
/// one can say which others it would break: entries are shared by
/// service, and removing one is removing it for all of them. Sorted by
/// label, each list by tool.
///
/// Kept in step with the tools by a test that reads each tool's sources
/// for the labels it uses; see DEVELOPMENT.md, "Secrets go in the
/// keyring, one entry per service".
pub const USERS: &[(&str, &[&str])] = &[
    (
        "sandogasa: Bugzilla API key",
        &[
            "ebranch",
            "fedora-cve-triage",
            "fedora-review-digest",
            "poi-tracker",
        ],
    ),
    (
        "sandogasa: Forgejo token",
        &["ebranch", "fesco-chair", "sandogasa-report"],
    ),
    (
        "sandogasa: GitHub token",
        &["ebranch", "fedora-cve-triage", "sandogasa-report"],
    ),
    (
        "sandogasa: GitLab token",
        &["cbspkg", "cpu-sig-tracker", "hs-relmon", "sandogasa-report"],
    ),
    ("sandogasa: Jira API token", &["cpu-sig-tracker"]),
    ("sandogasa: NVD API key", &["ebranch", "fedora-cve-triage"]),
    (
        "sandogasa: Pagure token",
        &["poi-tracker", "sandogasa-pkg-acl"],
    ),
    ("sandogasa: SourceHut token", &["sandogasa-report"]),
];

/// The tools other than `tool` that keep a secret labelled `label` and
/// are set up on this machine: those with a config file under
/// `config_home` (`$XDG_CONFIG_HOME`).
fn other_users_in(config_home: &std::path::Path, tool: &str, label: &str) -> Vec<&'static str> {
    USERS
        .iter()
        .filter(|(l, _)| *l == label)
        .flat_map(|(_, tools)| tools.iter().copied())
        .filter(|t| *t != tool && config_home.join(t).join("config.toml").exists())
        .collect()
}

/// The tools other than `tool` that keep a secret labelled `label` and
/// are set up on this machine. None when no config directory can be
/// found: this only ever feeds a note.
pub fn other_users_here(tool: &str, label: &str) -> Vec<&'static str> {
    dirs::config_dir()
        .map(|home| other_users_in(&home, tool, label))
        .unwrap_or_default()
}

/// What `tool`'s `config` says when `what` is turned off for it (its file
/// now holds [`OFF`]) while the keyring still holds `secret`: the entry
/// is kept, which other tools set up here keep that kind of secret, and
/// how to remove it by hand. The entry is never removed for the person,
/// as other tools may use it. `None` when the keyring holds no entry.
pub fn kept_on_clear(
    store: &dyn SecretStore,
    tool: &str,
    what: &str,
    secret: &SecretRef,
) -> Option<String> {
    let others = other_users_here(tool, secret.label());
    kept_note(store, tool, what, secret, &others)
}

/// [`kept_on_clear`] with the other tools already found.
fn kept_note(
    store: &dyn SecretStore,
    tool: &str,
    what: &str,
    secret: &SecretRef,
    others: &[&str],
) -> Option<String> {
    store.lookup(secret).ok().flatten()?;
    let who = if others.is_empty() {
        String::new()
    } else {
        format!(", as {} here may use it", others.join(", "))
    };
    Some(format!(
        "{what}: off for {tool}; the keyring entry is kept{who}; to remove it: {}",
        secret.clear_command()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every label each tool writes in its sources, `"sandogasa: …"`
    /// literals, which are the entries it keeps.
    fn labels_in(dir: &std::path::Path, out: &mut std::collections::BTreeSet<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                labels_in(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path).unwrap();
                let mut rest = text.as_str();
                while let Some(at) = rest.find("\"sandogasa: ") {
                    let tail = &rest[at + 1..];
                    let end = tail.find('"').unwrap();
                    out.insert(tail[..end].to_string());
                    rest = &tail[end..];
                }
            }
        }
    }

    /// The table says exactly which tools keep which secrets: read from
    /// the tools' own sources, so a tool that starts or stops keeping
    /// one fails here until the table says so. Skipped outside a
    /// checkout (a packaged crate has no `tools/` beside it).
    #[test]
    fn the_users_table_matches_what_the_tools_keep() {
        let tools = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools");
        if !tools.is_dir() {
            return;
        }
        let mut found: std::collections::BTreeMap<String, Vec<String>> = Default::default();
        for entry in std::fs::read_dir(&tools).unwrap() {
            let dir = entry.unwrap().path();
            let src = dir.join("src");
            if !src.is_dir() {
                continue;
            }
            let tool = dir.file_name().unwrap().to_string_lossy().into_owned();
            let mut labels = Default::default();
            labels_in(&src, &mut labels);
            for label in labels {
                found.entry(label).or_default().push(tool.clone());
            }
        }
        for tools in found.values_mut() {
            tools.sort();
        }
        let table: std::collections::BTreeMap<String, Vec<String>> = USERS
            .iter()
            .map(|(l, ts)| (l.to_string(), ts.iter().map(|t| t.to_string()).collect()))
            .collect();
        assert_eq!(
            table, found,
            "secret::USERS is out of step with the tools; update it to what they keep"
        );
    }

    #[test]
    fn the_users_table_is_sorted() {
        let labels: Vec<&str> = USERS.iter().map(|(l, _)| *l).collect();
        assert!(labels.is_sorted(), "{labels:?}");
        for (label, tools) in USERS {
            assert!(tools.is_sorted(), "{label}: {tools:?}");
        }
    }

    /// Clearing names the entry, the other tools set up here that keep
    /// that kind of secret (not the one clearing), and how to remove it.
    #[test]
    fn a_cleared_secret_names_who_else_here_keeps_one() {
        let home = tempfile::tempdir().unwrap();
        for tool in ["ebranch", "poi-tracker", "fesco-chair"] {
            std::fs::create_dir_all(home.path().join(tool)).unwrap();
            std::fs::write(home.path().join(tool).join("config.toml"), "").unwrap();
        }
        let label = "sandogasa: Bugzilla API key";
        // ebranch and poi-tracker are set up here and keep one;
        // fedora-review-digest keeps one but is not set up; fesco-chair
        // is set up but keeps none; the tool asking is never listed.
        assert_eq!(
            other_users_in(home.path(), "fedora-cve-triage", label),
            ["ebranch", "poi-tracker"]
        );
        assert_eq!(
            other_users_in(home.path(), "ebranch", label),
            ["poi-tracker"]
        );

        let store = MemoryStore::default();
        let bz = SecretRef::for_url("https://bugzilla.redhat.com", label);
        let others = other_users_in(home.path(), "fedora-cve-triage", label);
        // No entry: nothing to say.
        assert_eq!(
            kept_note(
                &store,
                "fedora-cve-triage",
                "Bugzilla API key",
                &bz,
                &others
            ),
            None
        );
        store.store(&bz, "s3cr3t-bz-key").unwrap();
        let note = kept_note(
            &store,
            "fedora-cve-triage",
            "Bugzilla API key",
            &bz,
            &others,
        )
        .unwrap();
        assert_eq!(
            note,
            "Bugzilla API key: off for fedora-cve-triage; the keyring entry is kept, as \
             ebranch, poi-tracker here may use it; to remove it: secret-tool clear \
             application sandogasa service bugzilla.redhat.com"
        );
        assert!(!note.contains("s3cr3t"), "the secret is never in the note");
    }

    /// A stand-in for `secret-tool` in `dir`, keeping entries as files
    /// there: `store` reads the secret from stdin, `lookup` prints it
    /// (nothing, and success, when there is none, as the real one does),
    /// `clear` removes it. With `broken`, every call fails as one with
    /// no Secret Service reachable does.
    fn fake_secret_tool(dir: &std::path::Path, broken: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = if broken {
            "#!/bin/sh\necho 'Cannot autolaunch D-Bus without X11 $DISPLAY' >&2\nexit 1\n"
                .to_string()
        } else {
            format!(
                "#!/bin/sh\nverb=$1; shift\ncase $1 in --label=*) shift;; esac\n\
                 key=$(echo \"$*\" | tr ' /' '__')\nf='{db}'/$key\n\
                 case $verb in\n\
                 store) cat > \"$f\";;\n\
                 lookup) [ -f \"$f\" ] && cat \"$f\"; exit 0;;\n\
                 clear) rm -f \"$f\";;\n\
                 esac\n",
                db = dir.display()
            )
        };
        let path = dir.join("secret-tool");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    /// What every tool's keyring goes through: a secret stored is the
    /// one looked up, under its own entry, and gone once cleared; a
    /// service on this machine is never asked about.
    #[test]
    fn secret_tool_stores_looks_up_and_clears_an_entry() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_secret_tool(dir.path(), false);
        let tool = SecretToolAt {
            program: program.as_os_str(),
        };
        let gitlab = SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token");
        let other = SecretRef::for_url("https://gitlab.example.org", "sandogasa: GitLab token");
        assert_eq!(tool.lookup(&gitlab).unwrap(), None);
        tool.store(&gitlab, "glpat-1").unwrap();
        assert_eq!(tool.lookup(&gitlab).unwrap().as_deref(), Some("glpat-1"));
        assert_eq!(
            tool.lookup(&other).unwrap(),
            None,
            "another service's entry"
        );
        tool.clear(&gitlab).unwrap();
        assert_eq!(tool.lookup(&gitlab).unwrap(), None);
        // Clearing what is not there is not a failure.
        tool.clear(&gitlab).unwrap();
        // A test server's entry is never looked up.
        let local = SecretRef::for_url("http://127.0.0.1:8080", "sandogasa: GitLab token");
        assert_eq!(tool.lookup(&local).unwrap(), None);
    }

    /// With no Secret Service, every call says the keyring is
    /// unavailable, and a secret kept then goes to the file, as before.
    #[test]
    fn without_a_secret_service_the_keyring_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_secret_tool(dir.path(), true);
        let tool = SecretToolAt {
            program: program.as_os_str(),
        };
        let gitlab = SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token");
        assert!(matches!(
            tool.lookup(&gitlab),
            Err(SecretError::Unavailable(_))
        ));
        assert!(matches!(
            tool.store(&gitlab, "t"),
            Err(SecretError::Unavailable(_))
        ));
        assert!(matches!(
            tool.clear(&gitlab),
            Err(SecretError::Unavailable(_))
        ));
        assert_eq!(
            keep_for_file(&tool, "GitLab token", &gitlab, "glpat-1".into()),
            "glpat-1",
            "kept in the file"
        );
        // No secret-tool at all is unavailable too.
        let missing = SecretToolAt {
            program: std::ffi::OsStr::new("/nonexistent/secret-tool"),
        };
        assert!(matches!(
            missing.lookup(&gitlab),
            Err(SecretError::Unavailable(_))
        ));
    }

    /// `-` in a tool's file turns the secret off for it: the keyring's
    /// entry is not read, an environment variable still is, and keeping
    /// it leaves the marker in the file and the entry alone.
    #[test]
    fn a_secret_turned_off_is_not_read_from_the_keyring() {
        let store = MemoryStore::default();
        let nvd = SecretRef::for_url("https://services.nvd.nist.gov", "sandogasa: NVD API key");
        store.store(&nvd, "shared").unwrap();
        assert_eq!(
            resolve(&store, None, &nvd, Some("")).as_deref(),
            Some("shared")
        );
        assert_eq!(resolve(&store, None, &nvd, Some(OFF)), None);
        assert_eq!(
            keep_for_file(&store, "NVD API key", &nvd, OFF.to_string()),
            OFF
        );
        assert_eq!(store.lookup(&nvd).unwrap().as_deref(), Some("shared"));
    }

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
