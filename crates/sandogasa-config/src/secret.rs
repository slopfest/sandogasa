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

/// A keyring that cannot be used, as on a machine without one: every
/// call fails as unavailable. For tests of the fall-back to the file.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoKeyring;

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
    /// such a service never has an entry, so it is neither looked up nor
    /// stored, and its secret stays in the config file.
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
        // Never looked up, so never stored: kept in the file instead,
        // where reading finds it, rather than in an entry nothing reads.
        if secret.is_local() {
            return Err(SecretError::Unavailable(
                "a service on this machine is not kept in the keyring".into(),
            ));
        }
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
        // Nothing was stored for it.
        if secret.is_local() {
            return Ok(());
        }
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

impl SecretStore for NoKeyring {
    fn lookup(&self, _: &SecretRef) -> Result<Option<String>, SecretError> {
        Err(SecretError::Unavailable("no keyring".into()))
    }

    fn store(&self, _: &SecretRef, _: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("no keyring".into()))
    }

    fn clear(&self, _: &SecretRef) -> Result<(), SecretError> {
        Err(SecretError::Unavailable("no keyring".into()))
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

/// One secret a tool's config holds, for [`fill`], [`settle`] and
/// [`save_with`]: where in the file it goes, what to call it, its
/// keyring entry, and the field holding it.
pub struct Slot<'a> {
    pub table: &'a str,
    pub key: &'a str,
    /// What to call it in messages ("Bugzilla API key").
    pub what: &'a str,
    pub entry: SecretRef,
    pub value: &'a mut String,
    /// Whether the tool cannot work without it: a required secret turned
    /// off is asked about on any run, and using the keyring's entry again
    /// is the default; an optional one only with `--all`, and not the
    /// default, as it was turned off on purpose.
    pub required: bool,
}

/// How a secret stood before a tool's prompts, as [`fill`] found it, for
/// [`settle`] to decide what an empty answer means.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Before {
    /// Never set: nothing in the keyring or the file.
    #[default]
    Absent,
    /// Set, with what the keyring held for it (`None` when it came from
    /// the file), so it is not looked up again.
    Set { keyring: Option<String> },
    /// Turned off for this tool ([`OFF`] in its file), and left off.
    Off,
}

/// A tool's one secret, kept in the keyring when there is one and else
/// as `[table] key` in its config file: hs-relmon's and cbspkg's GitLab
/// token. What a `config` command offers as the saved value
/// ([`saved`](Self::saved)) and how it saves the answer
/// ([`save`](Self::save)), so a tool with one secret needs no
/// [`Slot`]s of its own. Made with [`new`](Self::new).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ConfigSecret<'a> {
    /// The tool, as [`fill`] and [`save_with`] name it.
    pub tool: &'a str,
    pub table: &'a str,
    pub key: &'a str,
    /// What the secret is, for messages: "GitLab token".
    pub what: &'a str,
    pub entry: SecretRef,
    /// Whether the tool cannot work without it, as [`Slot::required`].
    pub required: bool,
}

/// Whether `value` is a secret to use: set, and not turned off. What a
/// tool checks before sending one to its service, so neither an empty
/// value nor [`OFF`] is ever checked or used as one.
pub fn is_set(value: &str) -> bool {
    !value.is_empty() && value != OFF
}

/// The question [`fill`] asks whether a secret turned off for `tool` is
/// to use the keyring's entry again: one wording for every tool, also for
/// one that asks it itself.
pub fn reenable_question(what: &str, tool: &str) -> String {
    format!("{what}: off for {tool} (the keyring has one). Use it again?")
}

/// The `ask` answer for a run that cannot ask: a secret turned off stays
/// off.
pub fn never(_question: &str, _default_yes: bool) -> bool {
    false
}

/// Fill each slot with the value `tool`'s `config` offers as its saved
/// one: the keyring's, which reading takes first, so a stale value left in
/// the file is never offered and saved back over the shared entry.
///
/// A secret turned off ([`OFF`]) is left alone on a plain run when it is
/// optional: it stays off, and its prompt sees a value and asks nothing,
/// so turning it off means not being asked again. With `review` (the
/// `--all` run, which goes over every setting), or for a required one on
/// any run, it is offered again: when the keyring still holds its entry,
/// `ask` is asked [`reenable_question`], defaulting to yes for a
/// required secret and no for an optional one, and a yes offers the
/// entry's value (which [`settle`] then takes the marker out for);
/// otherwise it is offered blank, so the marker never reaches a check
/// against the service. `ask` is `sandogasa_cli::ask_to_reenable`, or
/// [`never`] where nothing may be asked. How each slot stood, for
/// [`settle`].
pub fn fill(
    store: &dyn SecretStore,
    tool: &str,
    slots: Vec<Slot<'_>>,
    review: bool,
    ask: &dyn Fn(&str, bool) -> bool,
) -> Vec<Before> {
    slots
        .into_iter()
        .map(|s| {
            if s.value == OFF {
                if !review && !s.required {
                    return Before::Off;
                }
                if let Some(v) = store.lookup(&s.entry).ok().flatten()
                    && ask(&reenable_question(s.what, tool), s.required)
                {
                    *s.value = v.clone();
                    return Before::Set { keyring: Some(v) };
                }
                s.value.clear();
                return Before::Off;
            }
            match store.lookup(&s.entry).ok().flatten() {
                Some(v) => {
                    *s.value = v.clone();
                    Before::Set { keyring: Some(v) }
                }
                None if !s.value.is_empty() => Before::Set { keyring: None },
                None => Before::Absent,
            }
        })
        .collect()
}

/// Settle each slot after `tool`'s prompts, `before` being what [`fill`]
/// said. A value the keyring already held is left there, quietly. A new
/// value is kept in the keyring (or the file, without one). An empty
/// answer depends on how the secret stood: one that was set is turned off
/// for the tool, its file saying [`OFF`], with [`kept_on_clear`]'s note;
/// one that was off stays off, quietly; one never set stays absent. The
/// `(table, key)` of each secret to take out of the file.
pub fn settle<'a>(
    store: &dyn SecretStore,
    tool: &str,
    slots: Vec<Slot<'a>>,
    before: &[Before],
) -> Vec<(&'a str, &'a str)> {
    let mut out_of_file = Vec::new();
    let absent = Before::Absent;
    for (i, s) in slots.into_iter().enumerate() {
        let before = before.get(i).unwrap_or(&absent);
        if s.value == OFF {
            continue;
        }
        if s.value.is_empty() {
            match before {
                Before::Set { .. } => {
                    if let Some(note) = kept_on_clear(store, tool, s.what, &s.entry) {
                        eprintln!("{note}");
                    }
                    *s.value = OFF.to_string();
                }
                Before::Off => *s.value = OFF.to_string(),
                Before::Absent => out_of_file.push((s.table, s.key)),
            }
            continue;
        }
        if let Before::Set {
            keyring: Some(held),
        } = before
            && *held == *s.value
        {
            s.value.clear();
            out_of_file.push((s.table, s.key));
            continue;
        }
        *s.value = keep_for_file(store, s.what, &s.entry, std::mem::take(s.value));
        if s.value.is_empty() {
            out_of_file.push((s.table, s.key));
        }
    }
    out_of_file
}

/// [`settle`] `config`'s secrets, which `slots` lists, then save it to
/// `cf` and take each secret the keyring holds out of the file: `save`
/// lays the struct over the file and never removes a key, so a secret
/// moved to the keyring would otherwise stay there in plain text.
/// Whether the file is still there (false once nothing was left in it).
pub fn save_with<T: serde::Serialize>(
    cf: &crate::ConfigFile,
    store: &dyn SecretStore,
    tool: &str,
    config: &mut T,
    before: &[Before],
    slots: impl for<'b> FnOnce(&'b mut T) -> Vec<Slot<'b>>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let out_of_file: Vec<(String, String)> = settle(store, tool, slots(config), before)
        .into_iter()
        .map(|(t, k)| (t.to_string(), k.to_string()))
        .collect();
    cf.save(&*config)?;
    let mut removed = false;
    for (table, key) in out_of_file {
        removed |= cf.remove(&table, &key)?;
    }
    Ok(!removed)
}

impl<'a> ConfigSecret<'a> {
    /// `tool`'s `what` ("GitLab token"), kept in the keyring `entry`
    /// or else as `[table] key` in its config file; required, as a
    /// tool's one secret usually is.
    pub fn new(
        tool: &'a str,
        table: &'a str,
        key: &'a str,
        what: &'a str,
        entry: SecretRef,
    ) -> Self {
        Self {
            tool,
            table,
            key,
            what,
            entry,
            required: true,
        }
    }

    /// The same, for a secret the tool can work without, as
    /// [`Slot::required`].
    pub fn optional(self) -> Self {
        Self {
            required: false,
            ..self
        }
    }

    fn slot<'b>(&self, value: &'b mut String) -> Slot<'b>
    where
        'a: 'b,
    {
        Slot {
            table: self.table,
            key: self.key,
            what: self.what,
            entry: self.entry.clone(),
            value,
            required: self.required,
        }
    }

    /// The value `config` offers as the saved one, from `cf`'s user
    /// file alone, never `/etc`'s, which saving would copy into it: the
    /// keyring's in `store` first. `review` is the `--all` run. One
    /// turned off is offered blank unless `ask` takes the keyring's
    /// back, as [`fill`] does. With how it stood, for
    /// [`save`](Self::save). A user file that cannot be read, or holds
    /// something other than a string there, is an error rather than no
    /// secret, so nothing is saved over it.
    pub fn saved(
        &self,
        cf: &crate::ConfigFile,
        store: &dyn SecretStore,
        review: bool,
        ask: &dyn Fn(&str, bool) -> bool,
    ) -> Result<(String, Vec<Before>), Box<dyn std::error::Error>> {
        let mut value = match cf.path().exists() {
            true => match cf.load_user::<toml::Table>()?.get(self.table) {
                None => String::new(),
                Some(table) => match table.get(self.key) {
                    None => String::new(),
                    Some(toml::Value::String(v)) => v.clone(),
                    Some(_) => {
                        return Err(format!(
                            "{}: [{}] {} is not a string",
                            cf.path().display(),
                            self.table,
                            self.key
                        )
                        .into());
                    }
                },
            },
            false => String::new(),
        };
        let before = fill(store, self.tool, vec![self.slot(&mut value)], review, ask);
        Ok((value, before))
    }

    /// Save `value`, in the keyring `store` when there is one and out of
    /// `cf` then, `before` being what [`saved`](Self::saved) said; the
    /// file's other tables are left as they are. Whether `cf` is still
    /// there, as [`save_with`].
    pub fn save(
        &self,
        cf: &crate::ConfigFile,
        store: &dyn SecretStore,
        mut value: String,
        before: &[Before],
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let out_of_file = !settle(store, self.tool, vec![self.slot(&mut value)], before).is_empty();
        if out_of_file {
            cf.remove(self.table, self.key)?;
        } else {
            let mut keys = toml::Table::new();
            keys.insert(self.key.to_string(), toml::Value::String(value));
            let mut file = toml::Table::new();
            file.insert(self.table.to_string(), toml::Value::Table(keys));
            cf.save(&file)?;
        }
        Ok(cf.path().exists())
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

    fn gitlab_token() -> ConfigSecret<'static> {
        ConfigSecret::new(
            "a-tool",
            "gitlab",
            "access_token",
            "GitLab token",
            SecretRef::for_url("https://gitlab.com", "sandogasa: GitLab token"),
        )
    }

    /// A token in an old file is offered keyring-first, then moved to
    /// the keyring; the file's other tables stay, and a file left with
    /// nothing is removed.
    #[test]
    fn a_config_secret_is_offered_from_the_keyring_and_moved_there() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[gitlab]\naccess_token = \"glpat-stale\"\n\n[packages.perf]\nvariants = [\"main\"]\n",
        )
        .unwrap();
        let cf = crate::ConfigFile::from_path(path.clone());
        let token = gitlab_token();
        let store = MemoryStore::default();
        let (saved, before) = token.saved(&cf, &store, false, &never).unwrap();
        assert_eq!(saved, "glpat-stale");
        assert_eq!(before, [Before::Set { keyring: None }]);
        store.store(&token.entry, "glpat-newer").unwrap();
        let (saved, before) = token.saved(&cf, &store, false, &never).unwrap();
        assert_eq!(saved, "glpat-newer");
        // Enter keeps it: out of the file, the rest of it kept.
        assert!(token.save(&cf, &store, saved, &before).unwrap());
        let left = std::fs::read_to_string(&path).unwrap();
        assert!(!left.contains("gitlab"), "{left}");
        assert!(left.contains("[packages.perf]"), "{left}");
        std::fs::remove_file(&path).unwrap();
        // With nothing saved anywhere, a new one goes to the keyring and
        // no file is written.
        let empty = MemoryStore::default();
        let (saved, before) = token.saved(&cf, &empty, false, &never).unwrap();
        assert_eq!(
            (saved.as_str(), before.as_slice()),
            ("", [Before::Absent].as_slice())
        );
        assert!(
            !token
                .save(&cf, &empty, "glpat-new".into(), &before)
                .unwrap()
        );
        assert!(!path.exists());
        assert_eq!(
            empty.lookup(&token.entry).unwrap().as_deref(),
            Some("glpat-new")
        );
    }

    /// One turned off is offered blank, and asked about only when the
    /// keyring has one to take back; taken back, it leaves the file.
    #[test]
    fn an_off_config_secret_is_offered_only_when_taken_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[gitlab]\naccess_token = \"-\"\n").unwrap();
        let cf = crate::ConfigFile::from_path(path.clone());
        let token = gitlab_token();
        let store = MemoryStore::default();
        let unasked = |_: &str, _: bool| -> bool { panic!("nothing to take back") };
        assert_eq!(
            token.saved(&cf, &store, false, &unasked).unwrap(),
            (String::new(), vec![Before::Off])
        );
        store.store(&token.entry, "glpat-shared").unwrap();
        assert_eq!(
            token.saved(&cf, &store, false, &never).unwrap(),
            (String::new(), vec![Before::Off])
        );
        let (saved, before) = token
            .saved(&cf, &store, false, &|q, default_yes| {
                assert!(q.contains("GitLab token: off for a-tool"), "{q}");
                default_yes
            })
            .unwrap();
        assert_eq!(saved, "glpat-shared");
        assert!(!token.save(&cf, &store, saved, &before).unwrap());
        assert!(!path.exists());
    }

    /// What `/etc` ships is read beneath the user's file but never
    /// offered, so saving never copies it there.
    #[test]
    fn an_etc_config_secret_is_not_copied_into_the_users_file() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        let etc = dir.path().join("etc.toml");
        std::fs::write(&etc, "[gitlab]\naccess_token = \"etc-token\"\n").unwrap();
        let cf = crate::ConfigFile::from_path(user.clone()).with_system_path(etc);
        let token = gitlab_token();
        let store = MemoryStore::default();
        let (saved, before) = token.saved(&cf, &store, false, &never).unwrap();
        assert_eq!(saved, "", "the /etc token is not offered");
        assert_eq!(before, [Before::Absent]);
        assert!(
            !token
                .save(&cf, &store, "glpat-mine".into(), &before)
                .unwrap()
        );
        assert!(!user.exists());
        assert_eq!(
            store.lookup(&token.entry).unwrap().as_deref(),
            Some("glpat-mine")
        );
    }

    /// A file that does not parse, or holds no string where the secret
    /// goes, is an error: nothing is offered or saved over it.
    #[test]
    fn an_unreadable_config_secret_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cf = crate::ConfigFile::from_path(path.clone());
        let token = gitlab_token();
        std::fs::write(&path, "[gitlab\n").unwrap();
        assert!(token.saved(&cf, &NoKeyring, false, &never).is_err());
        std::fs::write(&path, "[gitlab]\naccess_token = 123\n").unwrap();
        let e = token
            .saved(&cf, &NoKeyring, false, &never)
            .unwrap_err()
            .to_string();
        assert!(e.ends_with("[gitlab] access_token is not a string"), "{e}");
        // Another table, or none: no secret yet.
        std::fs::write(&path, "[other]\nx = 1\n").unwrap();
        assert_eq!(token.saved(&cf, &NoKeyring, false, &never).unwrap().0, "");
        assert!(token.required);
        assert!(!token.optional().required);
    }

    /// Without a keyring the value stays in the file.
    #[test]
    fn a_config_secret_stays_in_the_file_without_a_keyring() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let cf = crate::ConfigFile::from_path(path.clone());
        let token = gitlab_token();
        let store = NoKeyring;
        let (_, before) = token.saved(&cf, &store, false, &never).unwrap();
        assert!(
            token
                .save(&cf, &store, "glpat-file".into(), &before)
                .unwrap()
        );
        let loaded: toml::Table = cf.load_user().unwrap();
        assert_eq!(
            loaded["gitlab"]["access_token"].as_str(),
            Some("glpat-file")
        );
    }

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

    /// A service on this machine (a Forgejo on localhost:3000) is never
    /// looked up, so it is never stored either: its secret stays in the
    /// file, where the next run reads it, and secret-tool is not run.
    #[test]
    fn a_local_services_secret_stays_in_the_file() {
        let local = SecretRef::for_url("http://localhost:3000", "sandogasa: Forgejo token");
        let tool = SecretToolAt {
            program: std::ffi::OsStr::new("/nonexistent/secret-tool"),
        };
        assert_eq!(tool.lookup(&local), Ok(None));
        assert!(matches!(
            tool.store(&local, "fj-1"),
            Err(SecretError::Unavailable(_))
        ));
        assert_eq!(tool.clear(&local), Ok(()));
        assert_eq!(
            keep_for_file(&tool, "Forgejo token", &local, "fj-1".into()),
            "fj-1",
            "kept in the file"
        );
    }

    /// A tool's config as the tools write theirs: secrets and other
    /// fields left out when empty, so a file of nothing is empty.
    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct Sample {
        #[serde(default)]
        bugzilla: SampleTable,
        #[serde(default)]
        nvd: SampleTable,
    }

    #[derive(Default, serde::Serialize, serde::Deserialize)]
    struct SampleTable {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        api_key: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        email: String,
    }

    fn sample_slots(c: &mut Sample) -> Vec<Slot<'_>> {
        vec![
            Slot {
                table: "bugzilla",
                key: "api_key",
                what: "Bugzilla API key",
                entry: SecretRef::for_url(
                    "https://bugzilla.redhat.com",
                    "sandogasa: Bugzilla API key",
                ),
                value: &mut c.bugzilla.api_key,
                required: true,
            },
            Slot {
                table: "nvd",
                key: "api_key",
                what: "NVD API key",
                entry: SecretRef::for_url(
                    "https://services.nvd.nist.gov",
                    "sandogasa: NVD API key",
                ),
                value: &mut c.nvd.api_key,
                required: false,
            },
        ]
    }

    /// One `config` run end to end: an old file's secrets are offered
    /// from the keyring first when it has them, then moved there, the
    /// file keeping only what is not secret; one never set stays absent.
    #[test]
    fn save_with_moves_secrets_out_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[bugzilla]\napi_key = \"stale-in-file\"\nemail = \"me@example.com\"\n",
        )
        .unwrap();
        let cf = crate::ConfigFile::from_path(path.clone());
        let store = MemoryStore::default();
        let bz = SecretRef::for_url("https://bugzilla.redhat.com", "sandogasa: Bugzilla API key");
        // Another tool stored a newer key in the shared entry.
        store.store(&bz, "newer-in-keyring").unwrap();
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "poi-tracker", sample_slots(&mut c), false, &never);
        assert_eq!(
            c.bugzilla.api_key, "newer-in-keyring",
            "the keyring's is offered"
        );
        assert_eq!(
            before,
            [
                Before::Set {
                    keyring: Some("newer-in-keyring".into())
                },
                Before::Absent
            ]
        );
        assert!(save_with(&cf, &store, "poi-tracker", &mut c, &before, sample_slots).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("api_key"), "{text}");
        assert!(text.contains("email = \"me@example.com\""), "{text}");
        assert_eq!(
            store.lookup(&bz).unwrap().as_deref(),
            Some("newer-in-keyring")
        );
    }

    /// An optional secret cleared is turned off, and stays off: a plain
    /// run neither offers it nor asks about it, so turning it off means
    /// not being asked again; `--all` offers it blank, asking first
    /// whether to use the keyring's entry again, no by default.
    #[test]
    fn an_optional_secret_turned_off_is_only_offered_again_with_all() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[bugzilla]\nemail = \"me@example.com\"\n").unwrap();
        let cf = crate::ConfigFile::from_path(path.clone());
        let store = MemoryStore::default();
        let nvd = SecretRef::for_url("https://services.nvd.nist.gov", "sandogasa: NVD API key");
        store.store(&nvd, "nvd-shared").unwrap();
        // Cleared at a run's prompt.
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "ebranch", sample_slots(&mut c), false, &never);
        assert_eq!(c.nvd.api_key, "nvd-shared");
        c.nvd.api_key.clear();
        save_with(&cf, &store, "ebranch", &mut c, &before, sample_slots).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("api_key = \"-\"")
        );
        assert_eq!(store.lookup(&nvd).unwrap().as_deref(), Some("nvd-shared"));

        let asked = std::cell::RefCell::new(Vec::new());
        let answer = std::cell::Cell::new(true);
        let ask = |q: &str, default: bool| {
            asked.borrow_mut().push((q.to_string(), default));
            answer.get()
        };
        // A plain run: left off, nothing asked, the prompt sees a value.
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "ebranch", sample_slots(&mut c), false, &ask);
        assert_eq!(before[1], Before::Off);
        assert_eq!(c.nvd.api_key, OFF);
        assert!(!is_set(&c.nvd.api_key), "never checked against NVD");
        assert!(asked.borrow().is_empty());
        save_with(&cf, &store, "ebranch", &mut c, &before, sample_slots).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("api_key = \"-\"")
        );

        // With --all, asked (default no) and told no: offered blank, and
        // Enter keeps it off.
        answer.set(false);
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "ebranch", sample_slots(&mut c), true, &ask);
        assert_eq!(c.nvd.api_key, "");
        assert_eq!(
            asked.borrow().as_slice(),
            [(
                "NVD API key: off for ebranch (the keyring has one). Use it again?".to_string(),
                false
            )]
        );
        save_with(&cf, &store, "ebranch", &mut c, &before, sample_slots).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("api_key = \"-\"")
        );

        // With --all and a yes: the entry is used again, and the marker
        // leaves the file, nothing stored anew.
        answer.set(true);
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "ebranch", sample_slots(&mut c), true, &ask);
        assert_eq!(c.nvd.api_key, "nvd-shared");
        save_with(&cf, &store, "ebranch", &mut c, &before, sample_slots).unwrap();
        assert!(!std::fs::read_to_string(&path).unwrap().contains("api_key"));
    }

    /// A required secret turned off is asked about on any run, using the
    /// keyring's entry again by default, as the tool cannot work without
    /// it; with no entry it is offered blank, to be asked for.
    #[test]
    fn a_required_secret_turned_off_is_asked_about_on_any_run() {
        let store = MemoryStore::default();
        let bz = SecretRef::for_url("https://bugzilla.redhat.com", "sandogasa: Bugzilla API key");
        let mut c = Sample::default();
        c.bugzilla.api_key = OFF.to_string();
        let not_asked = |q: &str, _: bool| -> bool { panic!("asked {q}") };
        let before = fill(
            &store,
            "poi-tracker",
            sample_slots(&mut c),
            false,
            &not_asked,
        );
        assert_eq!(before[0], Before::Off);
        assert_eq!(c.bugzilla.api_key, "", "offered blank, to be asked for");

        store.store(&bz, "bz-shared").unwrap();
        c.bugzilla.api_key = OFF.to_string();
        let default = std::cell::Cell::new(None);
        let take_default = |_: &str, d: bool| {
            default.set(Some(d));
            d
        };
        let before = fill(
            &store,
            "poi-tracker",
            sample_slots(&mut c),
            false,
            &take_default,
        );
        assert_eq!(default.get(), Some(true), "yes by default");
        assert_eq!(c.bugzilla.api_key, "bz-shared");
        assert_eq!(
            before[0],
            Before::Set {
                keyring: Some("bz-shared".into())
            }
        );
    }

    /// A file left holding only secrets is removed once they move, and
    /// `save_with` says so; a new value goes to the keyring.
    #[test]
    fn a_file_of_only_secrets_is_removed_once_they_move() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[nvd]\napi_key = \"nvd-old\"\n").unwrap();
        let cf = crate::ConfigFile::from_path(path.clone());
        let store = MemoryStore::default();
        let mut c: Sample = cf.load_user().unwrap();
        let before = fill(&store, "ebranch", sample_slots(&mut c), false, &never);
        c.nvd.api_key = "nvd-new".into();
        assert!(!save_with(&cf, &store, "ebranch", &mut c, &before, sample_slots).unwrap());
        assert!(!path.exists());
        let nvd = SecretRef::for_url("https://services.nvd.nist.gov", "sandogasa: NVD API key");
        assert_eq!(store.lookup(&nvd).unwrap().as_deref(), Some("nvd-new"));
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
