// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Who the person running the tools is, kept once for every tool
//! (#259).
//!
//! Several tools ask for the same account: the Jira email, the Bugzilla
//! email, the Fedora account. Each used to keep it in its own config, so
//! someone running several tools set it in each. The shared file,
//! `~/.config/sandogasa/config.toml` over `/etc/sandogasa/config.toml`,
//! holds each identity by service host, the way the keyring holds
//! secrets:
//!
//! ```toml
//! [jira."redhat.atlassian.net"]
//! email = "someone@example.com"
//! ```
//!
//! A tool reads its own field first and falls back to the shared value
//! ([`lookup`]), so a setup written before the shared file keeps
//! working. Its `config` command moves the value over: [`choose`] gives
//! the default to offer, from the tool's own file, the shared one, or
//! another tool's, and [`settle`] saves the answer to the shared file and
//! offers to remove the copies the tools hold. [`FIELDS`] says where
//! each tool keeps each identity, as [`crate::secret::USERS`] says which
//! tools keep each secret.

use std::path::PathBuf;

use crate::ConfigFile;
use crate::credentials::wrapped;

/// An identity the tools share, per service host.
#[derive(Debug, PartialEq, Eq)]
pub struct Identity {
    /// The shared file's table (`jira`), holding one table per host.
    pub table: &'static str,
    /// The key in a host's table (`email`).
    pub key: &'static str,
    /// What to call it ("Jira account email").
    pub what: &'static str,
    /// Whether keyring entries are kept per account of it (a Jira token
    /// belongs to an Atlassian email), so changing it leaves them behind.
    pub keys_secrets: bool,
}

/// The Atlassian account a Jira API token belongs to, per Jira site.
pub const JIRA_EMAIL: Identity = Identity {
    table: "jira",
    key: "email",
    what: "Jira account email",
    keys_secrets: true,
};

/// Where one tool keeps an identity in its own config file. The field
/// holds the value for the one instance the tool works with, whichever
/// host that is: [`choose`] and [`settle`] take another tool's copy as
/// being for the same host.
#[derive(Debug)]
pub struct Field {
    pub tool: &'static str,
    pub identity: &'static Identity,
    /// The key's path in the tool's file (`["jira", "email"]`).
    pub path: &'static [&'static str],
}

/// Every tool field holding a shared identity, sorted by tool. Each tool
/// listed checks its entries against its config struct in its own tests,
/// through [`fields_of`].
pub const FIELDS: &[Field] = &[Field {
    tool: "cpu-sig-tracker",
    identity: &JIRA_EMAIL,
    path: &["jira", "email"],
}];

/// Where a default offered for an identity came from.
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// The tool's own file.
    Own,
    /// The shared file.
    Shared,
    /// Another tool's file, the one value any holds.
    Tool(&'static str),
}

/// What a `config` command offers for an identity.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// Offer this value, saying where it came from.
    Offer(String, Source),
    /// Other tools hold different values: list them, and offer none.
    Differ(Vec<(&'static str, String)>),
    /// Nothing holds it.
    Nothing,
}

/// What a tool keeps in its own field once [`settle`] is done.
#[derive(Debug, PartialEq, Eq)]
pub enum Kept {
    /// Nothing: the shared value serves it.
    Shared,
    /// Its own value, differing from the shared one on purpose.
    Own(String),
}

impl Choice {
    /// The value to offer, empty for none.
    pub fn value(&self) -> &str {
        match self {
            Choice::Offer(v, _) => v,
            _ => "",
        }
    }

    /// A line to print before asking, `None` when there is nothing to
    /// say: the tool's own value needs no explanation.
    pub fn note(&self, identity: &Identity) -> Option<String> {
        match self {
            Choice::Offer(_, Source::Own) | Choice::Nothing => None,
            Choice::Offer(v, Source::Shared) => Some(format!(
                "  {}: {v}, shared by the sandogasa tools.",
                identity.what
            )),
            Choice::Offer(v, Source::Tool(t)) => {
                Some(format!("  {}: {v}, as {t} has it.", identity.what))
            }
            Choice::Differ(found) => {
                let list: Vec<String> = found.iter().map(|(t, v)| format!("{t}: {v}")).collect();
                Some(format!(
                    "  The tools set up here hold different values for the {} ({}); \
                     enter the one to use.",
                    identity.what,
                    list.join(", ")
                ))
            }
        }
    }
}

/// The fields `tool` keeps, for its tests to check against its config.
pub fn fields_of(tool: &str) -> impl Iterator<Item = &'static Field> + '_ {
    FIELDS.iter().filter(move |f| f.tool == tool)
}

/// The host an identity is kept under for a service at `base_url`, as
/// the keyring names it: `https://redhat.atlassian.net/` gives
/// `redhat.atlassian.net`.
pub fn host_of(base_url: &str) -> String {
    crate::secret::SecretRef::for_url(base_url, "")
        .service()
        .to_string()
}

/// The shared value of `identity` for `host`, from the shared file over
/// `/etc/sandogasa/config.toml`; `None` when neither sets it or neither
/// can be read.
pub fn lookup(identity: &Identity, host: &str) -> Option<String> {
    lookup_in(&Dirs::here()?, identity, host)
}

/// What to offer `tool`'s `config` for `identity` at `host`, given the
/// value its own file holds (empty for none): its own value, else the
/// shared one, else the one value the other tools' files hold.
pub fn choose(identity: &Identity, host: &str, tool: &str, own: &str) -> Choice {
    match Dirs::here() {
        Some(dirs) => choose_in(&dirs, identity, host, tool, own),
        None if own.is_empty() => Choice::Nothing,
        None => Choice::Offer(own.to_string(), Source::Own),
    }
}

/// Save `answer` for `identity` at `host` as `tool`'s `config` got it,
/// and tidy up the copies:
///
/// - With no shared value, `answer` becomes it.
/// - When the shared value differs, `ask` decides (default yes) whether
///   `answer` replaces it for every tool; declined, `tool` keeps
///   `answer` as its own and the shared value stays.
/// - Each other tool's copy equal to `answer` is offered for removal
///   (default yes); one that differs is left, and said to override it.
/// - `tool`'s own copy is removed from its file, unless it keeps one: an
///   override it already held and was given again stays, unasked.
///
/// Call it once the tool's own config is saved, with the field set, so
/// nothing moves unless that save worked. Its file keeps the field when
/// told [`Kept::Own`] and has lost it when told [`Kept::Shared`]; the
/// removal prints nothing, so the caller says what its file holds
/// afterward (it may be gone, if nothing else was in it). `ask` is the prompt
/// (question, default yes?); [`crate::secret::never`] declines every
/// question for a run that must not ask. An empty `answer` changes
/// nothing.
pub fn settle(
    identity: &Identity,
    host: &str,
    tool: &str,
    answer: &str,
    ask: &dyn Fn(&str, bool) -> bool,
) -> Result<Kept, Box<dyn std::error::Error>> {
    match Dirs::here() {
        Some(dirs) => settle_in(&dirs, identity, host, tool, answer, ask),
        None => Ok(Kept::Own(answer.to_string())),
    }
}

/// The config directories: the user's, and the system layer's.
struct Dirs {
    home: PathBuf,
    etc: PathBuf,
}

impl Dirs {
    fn here() -> Option<Self> {
        Some(Self {
            home: dirs::config_dir()?,
            etc: crate::system_root(),
        })
    }

    fn file(&self, tool: &str) -> ConfigFile {
        ConfigFile::in_dirs(&self.home, &self.etc, tool, "config.toml")
    }

    fn shared(&self) -> ConfigFile {
        self.file("sandogasa")
    }
}

/// The string at `path` in `table`, if there is one and it is not empty.
fn value_at(table: &toml::Table, path: &[&str]) -> Option<String> {
    let (last, tables) = path.split_last()?;
    let mut t = table;
    for key in tables {
        t = t.get(*key)?.as_table()?;
    }
    let v = t.get(*last)?.as_str()?;
    (!v.is_empty()).then(|| v.to_string())
}

fn lookup_in(dirs: &Dirs, identity: &Identity, host: &str) -> Option<String> {
    let table = dirs.shared().read_merged().ok()??;
    value_at(&table, &[identity.table, host, identity.key])
}

/// The values the tools other than `tool` hold for `identity` in their
/// own (user) files, by tool. The host is not checked: each tool's file
/// holds the value for the one instance it works with.
fn copies(dirs: &Dirs, identity: &Identity, tool: &str) -> Vec<(&'static str, String)> {
    FIELDS
        .iter()
        .filter(|f| f.identity == identity && f.tool != tool)
        .filter_map(|f| Some((f.tool, own_copy(dirs, f.tool, identity)?)))
        .collect()
}

fn choose_in(dirs: &Dirs, identity: &Identity, host: &str, tool: &str, own: &str) -> Choice {
    if !own.is_empty() {
        return Choice::Offer(own.to_string(), Source::Own);
    }
    if let Some(v) = lookup_in(dirs, identity, host) {
        return Choice::Offer(v, Source::Shared);
    }
    let found = copies(dirs, identity, tool);
    match found.as_slice() {
        [] => Choice::Nothing,
        [(t, v), rest @ ..] if rest.iter().all(|(_, other)| other == v) => {
            Choice::Offer(v.clone(), Source::Tool(t))
        }
        _ => Choice::Differ(found),
    }
}

/// Set the string at `path` in `file`'s user file, keeping the rest.
fn set(file: &ConfigFile, path: &[&str], value: &str) -> Result<(), Box<dyn std::error::Error>> {
    file.edit(|doc| {
        let (last, tables) = path.split_last().expect("a path");
        let mut t = doc.as_table_mut();
        for key in tables {
            let entry = t.entry(key).or_insert_with(toml_edit::table);
            // A table written inline is kept, as a table; anything else
            // under the name is not an identity table and is replaced.
            if let Some(inline) = entry.as_inline_table() {
                *entry = toml_edit::Item::Table(inline.clone().into_table());
            } else if !entry.is_table() {
                *entry = toml_edit::table();
            }
            let table = entry.as_table_mut().expect("is a table");
            // A host's table is written whole, as [jira."host"], with no
            // empty [jira] above it.
            table.set_implicit(true);
            t = table;
        }
        t.insert(last, toml_edit::value(value));
    })
}

fn settle_in(
    dirs: &Dirs,
    identity: &Identity,
    host: &str,
    tool: &str,
    answer: &str,
    ask: &dyn Fn(&str, bool) -> bool,
) -> Result<Kept, Box<dyn std::error::Error>> {
    if answer.is_empty() {
        return Ok(Kept::Shared);
    }
    let shared = dirs.shared();
    let key = [identity.table, host, identity.key];
    match lookup_in(dirs, identity, host) {
        Some(v) if v == answer => {}
        None => {
            set(&shared, &key, answer)?;
            eprintln!(
                "{}",
                wrapped(&format!(
                    "Saved the {} for {host} to {}, for every sandogasa tool.",
                    identity.what,
                    shared.path().display()
                ))
            );
        }
        // An override kept before, and kept again: nothing to ask.
        Some(_) if own_copy(dirs, tool, identity).as_deref() == Some(answer) => {
            return Ok(Kept::Own(answer.to_string()));
        }
        Some(v) => {
            let question = format!(
                "Use {answer} as the {} for {host} in every sandogasa tool, replacing {v}?",
                identity.what
            );
            if !ask(&question, true) {
                eprintln!("Keeping {answer} for {tool} alone.");
                return Ok(Kept::Own(answer.to_string()));
            }
            set(&shared, &key, answer)?;
            eprintln!("Saved to {}.", shared.path().display());
            if identity.keys_secrets {
                eprintln!(
                    "{}",
                    wrapped(&format!(
                        "Tokens kept in the keyring for {v} stay under that account; run \
                         `config` in the other tools that use one to enter it for {answer}."
                    ))
                );
            }
        }
    }
    for (other, value) in copies(dirs, identity, tool) {
        if value != answer {
            eprintln!(
                "{}",
                wrapped(&format!(
                    "{other} keeps its own {} ({value}), which it uses instead.",
                    identity.what
                ))
            );
            continue;
        }
        let question =
            format!("Remove the copy in {other}'s config, now that the shared file holds it?");
        if ask(&question, true) {
            remove_field(dirs, other, identity)?;
        }
    }
    // Not reported: the caller has just saved the tool's file, and says
    // what it holds once this returns.
    if own_copy(dirs, tool, identity).is_some() {
        remove_field(dirs, tool, identity)?;
    }
    Ok(Kept::Shared)
}

/// The value `tool` keeps for `identity` in its own user file.
fn own_copy(dirs: &Dirs, tool: &str, identity: &Identity) -> Option<String> {
    let field = fields_of(tool).find(|f| f.identity == identity)?;
    let table = crate::read_optional_table(dirs.file(tool).path()).ok()??;
    value_at(&table, field.path)
}

/// Remove `tool`'s copy of `identity` from its user file, if it keeps one.
fn remove_field(
    dirs: &Dirs,
    tool: &str,
    identity: &Identity,
) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(field) = fields_of(tool).find(|f| f.identity == identity) {
        dirs.file(tool).remove_path(field.path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const HOST: &str = "redhat.atlassian.net";

    fn dirs() -> (tempfile::TempDir, Dirs) {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = Dirs {
            home: tmp.path().join("home"),
            etc: tmp.path().join("etc"),
        };
        (tmp, dirs)
    }

    fn write(root: &Path, tool: &str, text: &str) {
        std::fs::create_dir_all(root.join(tool)).unwrap();
        std::fs::write(root.join(tool).join("config.toml"), text).unwrap();
    }

    fn read(root: &Path, tool: &str) -> String {
        std::fs::read_to_string(root.join(tool).join("config.toml")).unwrap_or_default()
    }

    fn shared(email: &str) -> String {
        format!("[jira.\"redhat.atlassian.net\"]\nemail = \"{email}\"\n")
    }

    #[test]
    fn the_host_is_the_keyring_service() {
        assert_eq!(host_of("https://redhat.atlassian.net/"), HOST);
        assert_eq!(host_of("https://Jira.Example:8443/x"), "jira.example:8443");
    }

    #[test]
    fn fields_are_sorted_by_tool() {
        let tools: Vec<&str> = FIELDS.iter().map(|f| f.tool).collect();
        assert!(tools.is_sorted(), "{tools:?}");
        assert_eq!(fields_of("cpu-sig-tracker").count(), 1);
        assert_eq!(fields_of("dbranch").count(), 0);
    }

    #[test]
    fn the_shared_value_comes_from_the_user_file_over_etc() {
        let (_tmp, d) = dirs();
        assert_eq!(lookup_in(&d, &JIRA_EMAIL, HOST), None);
        write(&d.etc, "sandogasa", &shared("etc@example.com"));
        assert_eq!(
            lookup_in(&d, &JIRA_EMAIL, HOST).as_deref(),
            Some("etc@example.com")
        );
        write(&d.home, "sandogasa", &shared("me@example.com"));
        assert_eq!(
            lookup_in(&d, &JIRA_EMAIL, HOST).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(lookup_in(&d, &JIRA_EMAIL, "other.atlassian.net"), None);
    }

    #[test]
    fn the_offer_is_the_own_value_then_the_shared_then_another_tools() {
        let (_tmp, d) = dirs();
        assert_eq!(
            choose_in(&d, &JIRA_EMAIL, HOST, "cbspkg", ""),
            Choice::Nothing
        );
        write(
            &d.home,
            "cpu-sig-tracker",
            "[jira]\nemail = \"tool@example.com\"\n",
        );
        let from_tool = choose_in(&d, &JIRA_EMAIL, HOST, "cbspkg", "");
        assert_eq!(
            from_tool,
            Choice::Offer("tool@example.com".into(), Source::Tool("cpu-sig-tracker"))
        );
        assert_eq!(from_tool.value(), "tool@example.com");
        let note = from_tool.note(&JIRA_EMAIL).unwrap();
        assert!(note.contains("as cpu-sig-tracker has it"), "{note}");
        // A tool's own copy is never offered back to it as another's.
        assert_eq!(
            choose_in(&d, &JIRA_EMAIL, HOST, "cpu-sig-tracker", ""),
            Choice::Nothing
        );
        write(&d.home, "sandogasa", &shared("shared@example.com"));
        let from_shared = choose_in(&d, &JIRA_EMAIL, HOST, "cbspkg", "");
        assert_eq!(
            from_shared,
            Choice::Offer("shared@example.com".into(), Source::Shared)
        );
        let note = from_shared.note(&JIRA_EMAIL).unwrap();
        assert!(note.contains("shared by the sandogasa tools"), "{note}");
        let own = choose_in(&d, &JIRA_EMAIL, HOST, "cbspkg", "own@example.com");
        assert_eq!(own, Choice::Offer("own@example.com".into(), Source::Own));
        assert_eq!(own.note(&JIRA_EMAIL), None);
    }

    #[test]
    fn differing_values_are_listed_and_none_is_offered() {
        let differ = Choice::Differ(vec![
            ("a", "x@example.com".into()),
            ("b", "y@example.com".into()),
        ]);
        assert_eq!(differ.value(), "");
        let note = differ.note(&JIRA_EMAIL).unwrap();
        assert!(
            note.contains("(a: x@example.com, b: y@example.com)"),
            "{note}"
        );
        assert_eq!(Choice::Nothing.value(), "");
        assert_eq!(Choice::Nothing.note(&JIRA_EMAIL), None);
    }

    #[test]
    fn a_first_answer_becomes_the_shared_value_and_leaves_the_tools_file() {
        let (_tmp, d) = dirs();
        write(
            &d.home,
            "cpu-sig-tracker",
            "[jira]\nemail = \"me@example.com\"\n\n[checkouts]\nroot = \"/src\"\n",
        );
        let kept = settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cpu-sig-tracker",
            "me@example.com",
            &|q, _| panic!("asked {q}"),
        )
        .unwrap();
        assert_eq!(kept, Kept::Shared);
        assert_eq!(read(&d.home, "sandogasa"), shared("me@example.com"));
        let tool = read(&d.home, "cpu-sig-tracker");
        assert!(!tool.contains("email"), "{tool}");
        assert!(tool.contains("root = \"/src\""), "{tool}");
    }

    #[test]
    fn an_answer_matching_etc_writes_nothing() {
        let (_tmp, d) = dirs();
        write(&d.etc, "sandogasa", &shared("me@example.com"));
        let kept = settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "me@example.com",
            &|q, _| panic!("asked {q}"),
        )
        .unwrap();
        assert_eq!(kept, Kept::Shared);
        assert!(!d.home.join("sandogasa/config.toml").exists());
    }

    #[test]
    fn a_different_answer_replaces_the_shared_value_or_stays_the_tools_own() {
        let (_tmp, d) = dirs();
        write(&d.home, "sandogasa", &shared("old@example.com"));
        let kept = settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "new@example.com",
            &|q, default| {
                assert!(q.contains("replacing old@example.com"), "{q}");
                assert!(default);
                false
            },
        )
        .unwrap();
        assert_eq!(kept, Kept::Own("new@example.com".into()));
        assert_eq!(read(&d.home, "sandogasa"), shared("old@example.com"));
        let kept = settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "new@example.com",
            &|_, _| true,
        )
        .unwrap();
        assert_eq!(kept, Kept::Shared);
        assert_eq!(read(&d.home, "sandogasa"), shared("new@example.com"));
        // Nothing answered, nothing changed.
        let kept = settle_in(&d, &JIRA_EMAIL, HOST, "cbspkg", "", &|q, _| panic!("{q}"));
        assert_eq!(kept.unwrap(), Kept::Shared);
        assert_eq!(read(&d.home, "sandogasa"), shared("new@example.com"));
    }

    #[test]
    fn an_inline_table_in_the_shared_file_is_kept() {
        let (_tmp, d) = dirs();
        write(
            &d.home,
            "sandogasa",
            "jira = { \"a.example\" = { email = \"x@a.example\" } }\n",
        );
        settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "me@example.com",
            &|q, _| panic!("asked {q}"),
        )
        .unwrap();
        assert_eq!(
            lookup_in(&d, &JIRA_EMAIL, "a.example").as_deref(),
            Some("x@a.example")
        );
        assert_eq!(
            lookup_in(&d, &JIRA_EMAIL, HOST).as_deref(),
            Some("me@example.com")
        );
    }

    #[test]
    fn an_override_given_again_is_kept_unasked() {
        let (_tmp, d) = dirs();
        write(&d.home, "sandogasa", &shared("me@example.com"));
        write(
            &d.home,
            "cpu-sig-tracker",
            "[jira]\nemail = \"work@example.com\"\n",
        );
        let kept = settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cpu-sig-tracker",
            "work@example.com",
            &|q, _| panic!("asked {q}"),
        )
        .unwrap();
        assert_eq!(kept, Kept::Own("work@example.com".into()));
        assert!(read(&d.home, "cpu-sig-tracker").contains("work@example.com"));
    }

    #[test]
    fn another_tools_equal_copy_is_offered_for_removal_and_a_different_one_kept() {
        let (_tmp, d) = dirs();
        write(
            &d.home,
            "cpu-sig-tracker",
            "[jira]\nemail = \"me@example.com\"\n",
        );
        let asked = std::cell::Cell::new(0);
        settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "me@example.com",
            &|q, _| {
                assert!(q.contains("cpu-sig-tracker's config"), "{q}");
                asked.set(asked.get() + 1);
                false
            },
        )
        .unwrap();
        assert_eq!(asked.get(), 1);
        assert!(read(&d.home, "cpu-sig-tracker").contains("me@example.com"));
        settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "me@example.com",
            &|_, _| true,
        )
        .unwrap();
        // The file held nothing else, so it is gone.
        assert!(!d.home.join("cpu-sig-tracker/config.toml").exists());
        // A copy that differs overrides the shared value: left, unasked.
        write(
            &d.home,
            "cpu-sig-tracker",
            "[jira]\nemail = \"work@example.com\"\n",
        );
        settle_in(
            &d,
            &JIRA_EMAIL,
            HOST,
            "cbspkg",
            "me@example.com",
            &|q, _| panic!("asked {q}"),
        )
        .unwrap();
        assert!(read(&d.home, "cpu-sig-tracker").contains("work@example.com"));
    }
}
