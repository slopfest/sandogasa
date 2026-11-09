// SPDX-License-Identifier: Apache-2.0 OR MIT

pub mod secret;

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Validator function for [`prompt_field`].
type Validator<'a> = Option<&'a dyn Fn(&str) -> Result<(), String>>;

/// A setting a `config` command asks for, for [`ask`].
///
/// ```no_run
/// # use sandogasa_config::{Setting, ask, validate_email};
/// let email = Setting::new("Bugzilla", "account e-mail")
///     .optional()
///     .validated(&validate_email);
/// let saved = String::new();
/// let value = ask(&email, &saved, false).unwrap();
/// ```
#[non_exhaustive]
pub struct Setting<'a> {
    /// Where the setting belongs, as the prompt names it (`Bugzilla`).
    pub section: &'a str,
    /// What it is (`API key`).
    pub label: &'a str,
    /// Typed or pasted input is hidden, and a saved value is never
    /// shown.
    pub sensitive: bool,
    /// It may be left empty, and a saved value may be cleared with `-`
    /// (unless the validator accepts a lone `-` as a value).
    pub optional: bool,
    /// Printed before the setting is asked for, e.g. where to create a
    /// key; not printed when a saved value is kept without asking.
    pub hint: Option<&'a str>,
    /// Checks an answer, and a saved value as well.
    pub validate: Validator<'a>,
}

/// Manages a TOML config file at `~/.config/{tool}/config.toml`,
/// layered over an optional system-wide file at
/// `/etc/{tool}/config.toml`.
///
/// Reads merge the layers: the system file (if any) is read
/// first and the user file overrides it per key, recursively for
/// tables. Command-line flags override both (see
/// `sandogasa_cli::parse_with_defaults`). Writes (`save`) only
/// ever touch the user file.
pub struct ConfigFile {
    path: PathBuf,
    /// Whether to enforce secure permissions (700/600).
    /// Only true for user config files under ~/.config.
    secure: bool,
    /// System-wide layer read beneath the user file. `None` for
    /// explicit-path configs (`from_path`).
    system_path: Option<PathBuf>,
}

impl ConfigFile {
    /// Create a `ConfigFile` for the given tool name.
    ///
    /// The config path will be `~/.config/{tool_name}/config.toml`.
    /// Permissions are enforced (700 for dir, 600 for file).
    pub fn for_tool(tool_name: &str) -> Self {
        // A literal "~" fallback would silently create `./~/.config`
        // in the CWD; with no home at all, failing loudly is better.
        Self::try_for_tool(tool_name)
            .expect("cannot determine the config directory: set XDG_CONFIG_HOME (absolute) or HOME")
    }

    /// Like [`ConfigFile::for_tool`], but returns `None` when no
    /// config directory can be determined instead of panicking.
    /// For optional reads (e.g. `[defaults]` lookup) where a
    /// homeless environment should mean "no config", not a crash.
    pub fn try_for_tool(tool_name: &str) -> Option<Self> {
        Self::try_for_tool_file(tool_name, "config.toml")
    }

    /// The same layered pair for a differently-named file in those
    /// directories: `/etc/<tool>/<file>` beneath
    /// `$XDG_CONFIG_HOME/<tool>/<file>`.
    ///
    /// For a config a package can ship system-wide while users
    /// override it per key — a tool's run profile, say — rather than
    /// the tool's own `config.toml`. Permissions are not enforced,
    /// since such a file is not where credentials belong.
    pub fn try_for_tool_file(tool_name: &str, file_name: &str) -> Option<Self> {
        Some(Self {
            path: dirs::config_dir()?.join(tool_name).join(file_name),
            secure: file_name == "config.toml",
            system_path: Some(system_root().join(tool_name).join(file_name)),
        })
    }

    /// Create a `ConfigFile` with an explicit path.
    ///
    /// No permission hardening is applied — use this for project-level
    /// config files that don't contain secrets. No system layer.
    pub fn from_path(path: PathBuf) -> Self {
        Self {
            path,
            secure: false,
            system_path: None,
        }
    }

    /// Override (or set) the system-layer path. For tests and
    /// non-standard layouts; `for_tool` wires `/etc/<tool>/`
    /// automatically.
    pub fn with_system_path(mut self, path: PathBuf) -> Self {
        self.system_path = Some(path);
        self
    }

    /// Return the (user) config file path — the one `save` writes.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Human-readable list of the layer paths, for error messages.
    pub fn describe_sources(&self) -> String {
        self.join_sources("+")
    }

    /// The configured paths joined by `sep` — `+` when describing a
    /// merge, `or` when reporting that neither was found.
    fn join_sources(&self, sep: &str) -> String {
        match &self.system_path {
            Some(sys) => format!("{} {sep} {}", sys.display(), self.path.display()),
            None => self.path.display().to_string(),
        }
    }

    /// Load and deserialize the config, merging the system layer
    /// (if any) under the user file: the user file overrides the
    /// system file per key, recursively for tables. At least one
    /// layer must exist.
    ///
    /// Also fixes permissions on the user file and its parent
    /// directory if they are too open (e.g. configs created before
    /// the permission hardening was added).
    pub fn load<T: DeserializeOwned>(&self) -> Result<T, Box<dyn std::error::Error>> {
        match self.read_merged()? {
            Some(table) => Ok(toml::Value::Table(table).try_into()?),
            // Name both paths: a tool configured entirely from /etc
            // has no user file, so naming only that one would point
            // at a file that was never expected to exist.
            None => Err(format!(
                "Could not read {}: file not found. Run 'config' to set up.",
                self.join_sources("or")
            )
            .into()),
        }
    }

    /// Load the **user** file alone, ignoring the system layer — for a
    /// `config` command that will [`Self::save`] what it loads. Loading
    /// the merged view there would copy every value shipped in `/etc`
    /// that the struct models into the user file, where it would shadow
    /// later package updates; loading the user file alone keeps the
    /// round trip to what the person actually wrote. Errors when the
    /// user file does not exist, like `load`.
    pub fn load_user<T: DeserializeOwned>(&self) -> Result<T, Box<dyn std::error::Error>> {
        match read_optional_table(&self.path)? {
            Some(table) => Ok(toml::Value::Table(table).try_into()?),
            None => Err(format!(
                "Could not read {}: file not found. Run 'config' to set up.",
                self.path.display()
            )
            .into()),
        }
    }

    /// Read the raw merged TOML of the layers. `Ok(None)` when no
    /// layer exists. For callers that inspect config generically
    /// (e.g. the `[defaults]` flag-defaults lookup).
    pub fn read_merged(&self) -> Result<Option<toml::Table>, String> {
        let system = match &self.system_path {
            Some(p) => read_optional_table(p)?,
            None => None,
        };
        let user = read_optional_table(&self.path)?;
        // Best-effort permission fix for user config files.
        if user.is_some() && self.secure {
            let _ = set_file_permissions(&self.path);
            if let Some(parent) = self.path.parent() {
                let _ = set_dir_permissions(parent);
            }
        }
        Ok(match (system, user) {
            (Some(s), Some(u)) => Some(merge_tables(s, u)),
            (s, u) => s.or(u),
        })
    }

    /// Serialize `config` into the **user** config file, creating
    /// parent directories as needed — the system layer is never
    /// written. For user config files (`for_tool`), sets directory
    /// permissions to 700 and file permissions to 600.
    ///
    /// The struct is laid over what the file already holds, key by
    /// key and recursively for tables, so a table the struct does not
    /// model — `[defaults]`, written by hand — survives a `config`
    /// command that only knows about credentials, as do comments on
    /// untouched keys. A key the struct carries is replaced; one it
    /// lacks is left alone, so `save` never removes anything (use
    /// [`Self::edit`] for that).
    ///
    /// Note: `save(load()?)` still bakes system-layer values the
    /// struct *does* model into the user file; `config` flows that
    /// prompt only for per-user fields are unaffected.
    pub fn save<T: Serialize>(&self, config: &T) -> Result<(), Box<dyn std::error::Error>> {
        let new: toml_edit::DocumentMut = toml::to_string_pretty(config)?.parse()?;
        self.edit(|doc| overlay_items(doc.as_table_mut(), new.as_table()))
    }

    /// Remove `key` from `table` in the user file, and the table too when
    /// that leaves it empty, and the file when nothing is left in it;
    /// nothing when the file or key is not there. Returns whether the file
    /// was removed, for the caller to say so.
    /// [`save`](Self::save) never removes a key (it lays the struct over
    /// the file), so a secret moved to the keyring is taken out of the
    /// file this way.
    pub fn remove(&self, table: &str, key: &str) -> Result<bool, Box<dyn std::error::Error>> {
        if !self.path.exists() {
            return Ok(false);
        }
        self.edit(|doc| {
            let emptied = match doc.get_mut(table).and_then(|t| t.as_table_like_mut()) {
                Some(t) => {
                    t.remove(key);
                    t.is_empty()
                }
                None => false,
            };
            if emptied {
                doc.remove(table);
            }
        })?;
        // Nothing left at all, not even a comment: no file, rather than
        // an empty one.
        if std::fs::read_to_string(&self.path)?.trim().is_empty() {
            std::fs::remove_file(&self.path)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Change part of the **user** config file in place, keeping its
    /// comments and formatting — the system layer is never written.
    ///
    /// `edit` receives the parsed document, empty if the file does not
    /// exist yet. Use this rather than [`Self::save`] whenever the file
    /// is one a person wrote and still reads: `save` serializes a whole
    /// struct, which drops every comment and bakes the system layer's
    /// values into the user file.
    pub fn edit<F: FnOnce(&mut toml_edit::DocumentMut)>(
        &self,
        edit: F,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut doc: toml_edit::DocumentMut = match std::fs::read_to_string(&self.path) {
            Ok(text) => text.parse()?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => toml_edit::DocumentMut::new(),
            Err(e) => return Err(e.into()),
        };
        edit(&mut doc);
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
            if self.secure {
                set_dir_permissions(parent)?;
            }
        }
        std::fs::write(&self.path, doc.to_string())?;
        if self.secure {
            set_file_permissions(&self.path)?;
        }
        Ok(())
    }
}

impl<'a> Setting<'a> {
    /// A required, visible setting with no hint and no check.
    pub fn new(section: &'a str, label: &'a str) -> Self {
        Self {
            section,
            label,
            sensitive: false,
            optional: false,
            hint: None,
            validate: None,
        }
    }

    /// Hide what is typed, and never show the saved value.
    pub fn sensitive(mut self) -> Self {
        self.sensitive = true;
        self
    }

    /// Allow it to be left empty, or cleared with `-`.
    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    /// Print `hint` before asking.
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = Some(hint);
        self
    }

    /// Check answers, and the saved value, with `validate`.
    pub fn validated(mut self, validate: &'a dyn Fn(&str) -> Result<(), String>) -> Self {
        self.validate = Some(validate);
        self
    }
}

/// Prompt the user for a config field value.
///
/// Prints `"Enter {section} {label}: "` and reads input.  If `sensitive`
/// is true, the typed/pasted text is hidden.  If `validate` is provided
/// the value is checked and the user is re-prompted on failure.
///
/// Returns an error if the input is empty.
pub fn prompt_field(
    section: &str,
    label: &str,
    sensitive: bool,
    validate: Validator<'_>,
) -> Result<String, Box<dyn std::error::Error>> {
    loop {
        let value = read_answer(section, label, sensitive)?;
        if value.is_empty() {
            return Err(format!("{label} cannot be empty").into());
        }

        if let Some(validate) = validate {
            match validate(&value) {
                Ok(()) => return Ok(value),
                Err(e) => {
                    eprintln!("Invalid {label}: {e}");
                    continue;
                }
            }
        }

        return Ok(value);
    }
}

/// Prompt for an optional field: an empty answer is a clean "no",
/// returned as `Ok(None)`, where [`prompt_field`] would refuse it.
/// The label should say the field is optional.
pub fn prompt_optional_field(
    section: &str,
    label: &str,
    sensitive: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    prompt_optional_field_validated(section, label, sensitive, None)
}

/// [`prompt_optional_field`] with a check: an answer that fails
/// `validate` is reported and asked for again, while an empty one is
/// still a clean "no".
pub fn prompt_optional_field_validated(
    section: &str,
    label: &str,
    sensitive: bool,
    validate: Validator<'_>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    loop {
        let value = read_answer(section, label, sensitive)?;
        if value.is_empty() {
            return Ok(None);
        }
        if let Some(validate) = validate
            && let Err(e) = validate(&value)
        {
            eprintln!("Invalid {label}: {e}");
            continue;
        }
        return Ok(Some(value));
    }
}

/// Validate that a string looks like an email address.
///
/// Suitable for use as a `validate` callback in [`prompt_field`].
pub fn validate_email(value: &str) -> Result<(), String> {
    if value.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("must not contain spaces or control characters".to_string());
    }
    if !value.contains('@') {
        return Err("must contain '@'".to_string());
    }
    let (local, domain) = value.split_once('@').unwrap();
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err("invalid email address".to_string());
    }
    Ok(())
}

/// Ask for a setting whose saved value is `saved` (empty for none),
/// and return the value to save (empty for none).
///
/// - With no saved value, it is asked for; a required setting cannot
///   be left empty, an optional one can.
/// - A saved value the setting's validator refuses is reported and
///   asked for again, as if there were none.
/// - Otherwise the saved value is kept and shown (or said to be set,
///   for a sensitive one) without asking, unless `review` is true.
///   Then it is asked for with the saved value as the default: an
///   empty answer keeps it, an answer replaces it, and for an optional
///   setting `-` clears it.
pub fn ask(
    setting: &Setting<'_>,
    saved: &str,
    review: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    ask_with(setting, saved, review, &mut |prompt, sensitive| {
        read_line_from_terminal(prompt, sensitive)
    })
}

/// Apply the backspaces a terminal passed through as characters.
///
/// A tty in canonical mode erases on one character, usually DEL; a
/// terminal whose backspace sends the other (^H) has it handed to
/// `read_line` as it is, so the typo it meant to remove is kept too.
fn apply_erasures(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        match c {
            '\u{8}' | '\u{7f}' => {
                out.pop();
            }
            _ => out.push(c),
        }
    }
    out
}

/// Print `prompt` and read one answer, hidden if `sensitive`, with
/// passed-through backspaces applied and surrounding space trimmed.
fn read_line_from_terminal(prompt: &str, sensitive: bool) -> io::Result<String> {
    print!("{prompt}");
    io::stdout().flush()?;
    let raw = if sensitive {
        rpassword::read_password()?
    } else {
        let mut buf = String::new();
        io::stdin().read_line(&mut buf)?;
        buf
    };
    Ok(apply_erasures(&raw).trim().to_string())
}

/// Print the prompt for a missing value and read one answer.
fn read_answer(
    section: &str,
    label: &str,
    sensitive: bool,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(read_line_from_terminal(
        &format!("Enter {section} {label}: "),
        sensitive,
    )?)
}

/// Whether `-` clears an optional setting: only when its validator,
/// if any, refuses a lone `-`, so that no value is lost to it.
fn clears(setting: &Setting<'_>) -> bool {
    setting.optional && setting.validate.is_none_or(|v| v("-").is_err())
}

/// The prompt for a setting that has a saved value, which an empty
/// answer keeps.
fn review_prompt(setting: &Setting<'_>, saved: &str) -> String {
    let shown = if setting.sensitive { "set" } else { saved };
    let clear = if clears(setting) { ", - clears" } else { "" };
    format!(
        "Enter {} {} [{shown}; Enter keeps{clear}]: ",
        setting.section, setting.label
    )
}

/// [`ask`], reading answers through `read` (prompt, hidden?), so the
/// decisions can be tested without a terminal.
fn ask_with(
    setting: &Setting<'_>,
    saved: &str,
    review: bool,
    read: &mut dyn FnMut(&str, bool) -> io::Result<String>,
) -> Result<String, Box<dyn std::error::Error>> {
    let name = format!("{} {}", setting.section, setting.label);
    let saved_error = if saved.is_empty() {
        None
    } else {
        setting.validate.and_then(|v| v(saved).err())
    };
    let keep = !saved.is_empty() && saved_error.is_none();
    if let Some(e) = &saved_error {
        eprintln!("The saved {name} is not valid ({e}), so it is asked for again.");
    }
    if keep && !review {
        if setting.sensitive {
            println!("{name}: (set)");
        } else {
            println!("{name}: {saved}");
        }
        return Ok(saved.to_string());
    }
    if let Some(hint) = setting.hint {
        println!("{hint}");
    }
    let prompt = if keep {
        review_prompt(setting, saved)
    } else {
        format!("Enter {name}: ")
    };
    loop {
        let answer = read(&prompt, setting.sensitive)?;
        if answer.is_empty() {
            if keep {
                return Ok(saved.to_string());
            }
            if setting.optional {
                return Ok(String::new());
            }
            eprintln!("{name} cannot be empty.");
            continue;
        }
        if keep && answer == "-" && clears(setting) {
            return Ok(String::new());
        }
        if let Some(validate) = setting.validate
            && let Err(e) = validate(&answer)
        {
            eprintln!("Invalid {}: {e}", setting.label);
            continue;
        }
        return Ok(answer);
    }
}

/// Recursively merge two TOML tables: `over` wins per key;
/// nested tables merge, everything else (scalars, arrays) is
/// replaced whole.
fn merge_tables(mut base: toml::Table, over: toml::Table) -> toml::Table {
    for (key, over_value) in over {
        match (base.remove(&key), over_value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => {
                base.insert(key, toml::Value::Table(merge_tables(b, o)));
            }
            (_, o) => {
                base.insert(key, o);
            }
        }
    }
    base
}

/// Read a TOML file into a table, `Ok(None)` when it doesn't
/// exist.
fn read_optional_table(path: &Path) -> Result<Option<toml::Table>, String> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    contents
        .parse()
        .map(Some)
        .map_err(|e| format!("could not parse {}: {e}", path.display()))
}

/// Set directory permissions to 700 (owner-only access).
/// Logs to stderr if permissions are changed.
fn set_dir_permissions(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if current != 0o700 {
        eprintln!(
            "Fixing directory permissions on {}: {:03o} -> 700",
            path.display(),
            current
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Set file permissions to 600 (owner read/write only).
/// Logs to stderr if permissions are changed.
fn set_file_permissions(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt;
    let current = std::fs::metadata(path)?.permissions().mode() & 0o777;
    if current != 0o600 {
        eprintln!(
            "Fixing file permissions on {}: {:03o} -> 600",
            path.display(),
            current
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// Lay `new` over `existing`: every key in `new` replaces the same key
/// in `existing`, except that two tables merge recursively so keys
/// only `existing` has are kept.
fn overlay_items(existing: &mut toml_edit::Table, new: &toml_edit::Table) {
    for (key, value) in new {
        match (existing.get_mut(key), value.as_table()) {
            (Some(old), Some(table)) if old.is_table() => {
                overlay_items(old.as_table_mut().expect("is_table"), table)
            }
            _ => {
                existing.insert(key, value.clone());
            }
        }
    }
}

/// The system layer's root: `/etc`, or `$SANDOGASA_ETC` when set —
/// so a config a repository ships under `configs/<tool>/` can be
/// tried beneath the real user file before it is installed
/// (`SANDOGASA_ETC=$PWD/configs`).
fn system_root() -> PathBuf {
    system_root_from(std::env::var_os("SANDOGASA_ETC"))
}

fn system_root_from(override_dir: Option<std::ffi::OsString>) -> PathBuf {
    match override_dir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("/etc"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct TestConfig {
        #[serde(rename = "my-section")]
        my_section: TestSection,
    }

    #[derive(Debug, Deserialize, Serialize, PartialEq)]
    struct TestSection {
        key: String,
        #[serde(default)]
        optional: String,
    }

    #[test]
    fn system_root_is_etc_unless_overridden() {
        assert_eq!(system_root_from(None), PathBuf::from("/etc"));
        assert_eq!(system_root_from(Some("".into())), PathBuf::from("/etc"));
        assert_eq!(
            system_root_from(Some("/srv/cfg".into())),
            PathBuf::from("/srv/cfg")
        );
    }

    #[test]
    fn for_tool_path_ends_with_tool_name() {
        let cf = ConfigFile::for_tool("my-tool");
        assert!(cf.path().ends_with("my-tool/config.toml"));
    }

    #[test]
    fn merge_tables_user_wins_per_key_recursively() {
        let system: toml::Table = "a = 1\nb = 2\n[nested]\nx = 1\ny = 1\narr = [1]"
            .parse()
            .unwrap();
        let user: toml::Table = "b = 3\n[nested]\ny = 2\narr = [2, 3]".parse().unwrap();
        let merged = merge_tables(system, user);
        assert_eq!(merged["a"].as_integer(), Some(1));
        assert_eq!(merged["b"].as_integer(), Some(3));
        let nested = merged["nested"].as_table().unwrap();
        // Sibling keys survive the merge; overridden ones and
        // arrays are replaced whole.
        assert_eq!(nested["x"].as_integer(), Some(1));
        assert_eq!(nested["y"].as_integer(), Some(2));
        assert_eq!(nested["arr"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn load_layers_system_under_user() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system.toml");
        let user = dir.path().join("user.toml");
        std::fs::write(
            &system,
            "[my-section]\nkey = \"from-system\"\noptional = \"sys\"\n",
        )
        .unwrap();
        std::fs::write(&user, "[my-section]\nkey = \"from-user\"\n").unwrap();

        let cf = ConfigFile::from_path(user.clone()).with_system_path(system.clone());
        let loaded: TestConfig = cf.load().unwrap();
        // User overrides the key it sets; the system-only key
        // shows through.
        assert_eq!(loaded.my_section.key, "from-user");
        assert_eq!(loaded.my_section.optional, "sys");

        // System layer alone is enough.
        std::fs::remove_file(&user).unwrap();
        let loaded: TestConfig = cf.load().unwrap();
        assert_eq!(loaded.my_section.key, "from-system");

        // No layer at all keeps the "run config" error, and names
        // both paths — neither exists, so blaming only the user file
        // would misdirect an /etc-only deployment.
        std::fs::remove_file(&system).unwrap();
        let err = cf.load::<TestConfig>().unwrap_err().to_string();
        assert!(err.contains("Run 'config'"), "{err}");
        assert!(err.contains(&system.display().to_string()), "{err}");
        assert!(err.contains(&user.display().to_string()), "{err}");
    }

    #[test]
    fn not_found_error_names_only_the_user_file_without_a_system_layer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let err = ConfigFile::from_path(path.clone())
            .load::<TestConfig>()
            .unwrap_err()
            .to_string();
        assert!(err.contains(&path.display().to_string()), "{err}");
        assert!(!err.contains(" or "), "{err}");
    }

    #[test]
    fn try_for_tool_file_layers_a_named_file() {
        // Same directories as config.toml, different file name — for a
        // profile a package ships while users override it per key.
        let cf = ConfigFile::try_for_tool_file("t", "run.toml").expect("config dir");
        assert!(cf.path().ends_with("t/run.toml"));
        let sources = cf.describe_sources();
        assert!(sources.starts_with("/etc/t/run.toml + "), "{sources}");
        assert!(sources.ends_with("t/run.toml"), "{sources}");
    }

    #[test]
    fn try_for_tool_file_only_hardens_config_toml() {
        // A run profile is not where credentials belong, so its
        // permissions are left alone; config.toml's are not.
        assert!(
            !ConfigFile::try_for_tool_file("t", "run.toml")
                .unwrap()
                .secure
        );
        assert!(
            ConfigFile::try_for_tool_file("t", "config.toml")
                .unwrap()
                .secure
        );
        assert!(ConfigFile::try_for_tool("t").unwrap().secure);
    }

    #[test]
    fn named_file_merges_the_layers() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("system-run.toml");
        let user = dir.path().join("user-run.toml");
        std::fs::write(&system, "products = [\"Fedora\"]\nstatuses = [\"NEW\"]\n").unwrap();
        std::fs::write(&user, "statuses = [\"ASSIGNED\"]\n").unwrap();
        let merged = ConfigFile::from_path(user)
            .with_system_path(system)
            .read_merged()
            .unwrap()
            .expect("a layer exists");
        // The user file wins per key; untouched keys survive.
        assert_eq!(merged["statuses"].as_array().unwrap().len(), 1);
        assert_eq!(merged["statuses"][0].as_str(), Some("ASSIGNED"));
        assert_eq!(merged["products"][0].as_str(), Some("Fedora"));
    }

    #[test]
    fn describe_sources_names_both_layers() {
        let cf = ConfigFile::from_path(PathBuf::from("/home/u/.config/t/config.toml"))
            .with_system_path(PathBuf::from("/etc/t/config.toml"));
        assert_eq!(
            cf.describe_sources(),
            "/etc/t/config.toml + /home/u/.config/t/config.toml"
        );
    }

    #[test]
    fn from_path_stores_path() {
        let cf = ConfigFile::from_path(PathBuf::from("/tmp/test.toml"));
        assert_eq!(cf.path(), Path::new("/tmp/test.toml"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let cf = ConfigFile::from_path(dir.path().join("config.toml"));

        let config = TestConfig {
            my_section: TestSection {
                key: "value".to_string(),
                optional: "".to_string(),
            },
        };
        cf.save(&config).unwrap();

        let loaded: TestConfig = cf.load().unwrap();
        assert_eq!(loaded, config);
    }

    #[test]
    fn load_user_ignores_the_system_layer_so_save_does_not_bake_it() {
        let dir = tempfile::tempdir().unwrap();
        let system = dir.path().join("etc.toml");
        let user = dir.path().join("config.toml");
        std::fs::write(&system, "[forge]\nurl = \"https://shipped.example\"\n").unwrap();
        std::fs::write(&user, "[forge]\ntoken = \"t\"\n").unwrap();
        #[derive(Serialize, Deserialize, Default)]
        struct Forge {
            #[serde(default)]
            url: String,
            #[serde(default)]
            token: String,
        }
        #[derive(Serialize, Deserialize, Default)]
        struct Config {
            forge: Forge,
        }
        let cf = ConfigFile::from_path(user.clone()).with_system_path(system);
        let merged: Config = cf.load().unwrap();
        assert_eq!(merged.forge.url, "https://shipped.example");
        let mine: Config = cf.load_user().unwrap();
        assert_eq!(mine.forge.url, "", "the shipped value is not the user's");
        cf.save(&mine).unwrap();
        let text = std::fs::read_to_string(&user).unwrap();
        assert!(!text.contains("shipped.example"), "{text}");
        assert!(text.contains("token = \"t\""), "{text}");
        assert!(
            cf.load_user::<Config>().is_ok()
                && ConfigFile::from_path(dir.path().join("missing.toml"))
                    .load_user::<Config>()
                    .is_err()
        );
    }

    #[test]
    fn save_keeps_tables_and_comments_the_struct_does_not_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "# hand-written\n[forgejo]\ntoken = \"old\"\nextra = 1\n\n[defaults]\nnon-voting = [\"jspaleta\"]\n\n[defaults.agenda]\nhistory = 20\n",
        )
        .unwrap();
        #[derive(Serialize)]
        struct Forgejo {
            token: String,
        }
        #[derive(Serialize)]
        struct Config {
            forgejo: Forgejo,
        }
        let cf = ConfigFile::from_path(path.clone());
        cf.save(&Config {
            forgejo: Forgejo {
                token: "new".into(),
            },
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("token = \"new\""), "{text}");
        assert!(
            text.contains("extra = 1"),
            "unmodelled key inside a modelled table kept: {text}"
        );
        assert!(text.contains("non-voting = [\"jspaleta\"]"), "{text}");
        assert!(text.contains("[defaults.agenda]\nhistory = 20"), "{text}");
        assert!(text.starts_with("# hand-written"), "{text}");
    }

    #[test]
    fn a_secret_moved_to_the_keyring_is_removed_from_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[gitlab]\naccess_token = \"glpat-old\"\n\n[jira]\nemail = \"me@example.com\"\naccess_token = \"t\"\n",
        )
        .unwrap();
        let cf = ConfigFile::from_path(path.clone());
        cf.remove("gitlab", "access_token").unwrap();
        cf.remove("jira", "access_token").unwrap();
        cf.remove("nvd", "api_key").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("glpat-old") && !text.contains("[gitlab]"),
            "{text}"
        );
        assert!(!text.contains("access_token"), "{text}");
        assert!(text.contains("email = \"me@example.com\""), "{text}");
        // The last secret gone, the file goes too.
        let only = dir.path().join("only.toml");
        std::fs::write(&only, "[gitlab]\naccess_token = \"glpat-old\"\n").unwrap();
        assert!(
            ConfigFile::from_path(only.clone())
                .remove("gitlab", "access_token")
                .unwrap()
        );
        assert!(!only.exists(), "an empty config file is removed");
        // Other settings left: kept.
        let more = dir.path().join("more.toml");
        std::fs::write(
            &more,
            "[defaults]\nexplain = true\n\n[gitlab]\naccess_token = \"x\"\n",
        )
        .unwrap();
        assert!(
            !ConfigFile::from_path(more.clone())
                .remove("gitlab", "access_token")
                .unwrap()
        );
        assert_eq!(
            std::fs::read_to_string(&more).unwrap().trim(),
            "[defaults]\nexplain = true"
        );
        // No file: nothing to do, and none is made.
        let none = ConfigFile::from_path(dir.path().join("absent.toml"));
        none.remove("gitlab", "access_token").unwrap();
        assert!(!dir.path().join("absent.toml").exists());
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let cf = ConfigFile::from_path(dir.path().join("nested").join("deep").join("config.toml"));

        let config = TestConfig {
            my_section: TestSection {
                key: "k".to_string(),
                optional: "".to_string(),
            },
        };
        cf.save(&config).unwrap();
        assert!(cf.path().exists());
    }

    #[test]
    fn save_produces_section_header() {
        let dir = tempfile::tempdir().unwrap();
        let cf = ConfigFile::from_path(dir.path().join("config.toml"));

        let config = TestConfig {
            my_section: TestSection {
                key: "val".to_string(),
                optional: "".to_string(),
            },
        };
        cf.save(&config).unwrap();

        let contents = std::fs::read_to_string(cf.path()).unwrap();
        assert!(contents.contains("[my-section]"));
        assert!(contents.contains("key = \"val\""));
    }

    #[test]
    fn load_nonexistent_file_errors() {
        let cf = ConfigFile::from_path(PathBuf::from("/tmp/does-not-exist-99999.toml"));
        let result: Result<TestConfig, _> = cf.load();
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not valid toml [[[").unwrap();
        let cf = ConfigFile::from_path(path);
        let result: Result<TestConfig, _> = cf.load();
        assert!(result.is_err());
    }

    #[test]
    fn load_wrong_structure_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wrong.toml");
        std::fs::write(&path, "unrelated = \"value\"\n").unwrap();
        let cf = ConfigFile::from_path(path);
        let result: Result<TestConfig, _> = cf.load();
        assert!(result.is_err());
    }

    // ---- permissions ----

    #[test]
    fn save_sets_secure_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let config_dir = dir.path().join("secure-tool");
        let cf = ConfigFile {
            path: config_dir.join("config.toml"),
            secure: true,
            system_path: None,
        };

        let config = TestConfig {
            my_section: TestSection {
                key: "secret".to_string(),
                optional: "".to_string(),
            },
        };
        cf.save(&config).unwrap();

        let dir_perms = std::fs::metadata(&config_dir).unwrap().permissions();
        assert_eq!(dir_perms.mode() & 0o777, 0o700);

        let file_perms = std::fs::metadata(cf.path()).unwrap().permissions();
        assert_eq!(file_perms.mode() & 0o777, 0o600);
    }

    // ---- validate_email ----

    #[test]
    fn validate_email_accepts_valid() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("a@b.c").is_ok());
        assert!(validate_email("user+tag@sub.domain.org").is_ok());
    }

    #[test]
    fn validate_email_rejects_no_at() {
        assert!(validate_email("userexample.com").is_err());
    }

    #[test]
    fn validate_email_rejects_empty_local() {
        assert!(validate_email("@example.com").is_err());
    }

    #[test]
    fn validate_email_rejects_empty_domain() {
        assert!(validate_email("user@").is_err());
    }

    #[test]
    fn validate_email_rejects_domain_without_dot() {
        assert!(validate_email("user@localhost").is_err());
    }

    #[test]
    fn validate_email_rejects_control_characters_and_spaces() {
        // What a ^H-sending terminal left in a typo'd address (#56).
        assert!(validate_email("bob@exampel\u{8}\u{8}\u{8}ple.com").is_err());
        assert!(validate_email("bob smith@example.com").is_err());
    }

    // ---- ask ----

    /// Answer `ask_with` from `answers` in turn, recording each prompt.
    fn ask_scripted(
        setting: &Setting<'_>,
        saved: &str,
        review: bool,
        answers: &[&str],
    ) -> (String, Vec<String>) {
        let mut prompts = Vec::new();
        let mut next = answers.iter();
        let value = ask_with(setting, saved, review, &mut |prompt, _| {
            prompts.push(prompt.to_string());
            Ok(next
                .next()
                .expect("asked more often than answered")
                .to_string())
        })
        .unwrap();
        (value, prompts)
    }

    #[test]
    fn a_saved_value_is_kept_without_asking() {
        let key = Setting::new("Bugzilla", "API key").sensitive();
        let (value, prompts) = ask_scripted(&key, "abc", false, &[]);
        assert_eq!(value, "abc");
        assert!(prompts.is_empty());
    }

    #[test]
    fn a_missing_value_is_asked_for() {
        let key = Setting::new("Bugzilla", "API key").sensitive();
        // A required setting cannot be left empty.
        let (value, prompts) = ask_scripted(&key, "", false, &["", "abc"]);
        assert_eq!(value, "abc");
        assert_eq!(prompts, ["Enter Bugzilla API key: "; 2]);
        let fas = Setting::new("Fedora", "account").optional();
        assert_eq!(ask_scripted(&fas, "", false, &[""]).0, "");
    }

    #[test]
    fn a_saved_value_that_fails_its_check_is_asked_for_again() {
        // #56: an address saved with the ^H of a backspace in it.
        let email = Setting::new("Bugzilla", "e-mail")
            .optional()
            .validated(&validate_email);
        let saved = "bob@exampel\u{8}\u{8}\u{8}ple.com";
        let (value, prompts) =
            ask_scripted(&email, saved, false, &["bob@example", "bob@example.com"]);
        assert_eq!(value, "bob@example.com");
        // Asked as if missing: an empty answer would not keep the bad one.
        assert_eq!(prompts[0], "Enter Bugzilla e-mail: ");
        assert_eq!(ask_scripted(&email, saved, false, &[""]).0, "");
    }

    #[test]
    fn review_keeps_replaces_or_clears() {
        let email = Setting::new("Bugzilla", "e-mail")
            .optional()
            .validated(&validate_email);
        let saved = "bob@example.com";
        let (value, prompts) = ask_scripted(&email, saved, true, &[""]);
        assert_eq!(value, saved);
        assert_eq!(
            prompts,
            ["Enter Bugzilla e-mail [bob@example.com; Enter keeps, - clears]: "]
        );
        assert_eq!(
            ask_scripted(&email, saved, true, &["nope", "ann@example.com"]).0,
            "ann@example.com"
        );
        assert_eq!(ask_scripted(&email, saved, true, &["-"]).0, "");
    }

    #[test]
    fn review_never_shows_a_secret_or_clears_a_required_one() {
        let key = Setting::new("Bugzilla", "API key").sensitive();
        let (value, prompts) = ask_scripted(&key, "secret", true, &["-"]);
        // Required: "-" is an answer like any other.
        assert_eq!(value, "-");
        assert_eq!(prompts, ["Enter Bugzilla API key [set; Enter keeps]: "]);
        assert!(!prompts[0].contains("secret"));
    }

    #[test]
    fn dash_is_not_a_clear_where_it_is_a_valid_value() {
        let any = |_: &str| Ok(());
        let field = Setting::new("Odd", "field").optional().validated(&any);
        let (value, prompts) = ask_scripted(&field, "x", true, &["-"]);
        assert_eq!(value, "-");
        assert!(!prompts[0].contains("- clears"), "{prompts:?}");
        // The e-mail check refuses a lone "-", so clearing is safe there.
        assert!(validate_email("-").is_err());
    }

    // ---- apply_erasures ----

    #[test]
    fn passed_through_backspaces_erase_what_came_before() {
        // ^H from a terminal whose tty erases on DEL (#56), and DEL
        // from one reading in non-canonical mode, are both erases.
        assert_eq!(
            apply_erasures("bob@exampel\u{8}\u{8}\u{8}ple.com\n"),
            "bob@example.com\n"
        );
        assert_eq!(apply_erasures("ab\u{7f}c"), "ac");
    }

    #[test]
    fn erasing_past_the_start_leaves_nothing_rather_than_failing() {
        assert_eq!(apply_erasures("a\u{8}\u{8}\u{8}b"), "b");
        assert_eq!(apply_erasures(""), "");
    }

    // ---- ConfigFile::edit ----

    #[test]
    fn edit_keeps_comments_and_untouched_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.toml");
        std::fs::write(
            &path,
            "# why this profile exists\nproducts = [\"Fedora\"]\n\n[check.\"bodhi-check\"]\n# the tracker to block\ntracker_bug = \"CVE-Tracker\"\n",
        )
        .unwrap();

        let cf = ConfigFile::from_path(path.clone());
        cf.edit(|doc| {
            doc["check"]["bodhi-check"]["fixed_versions"]["CVE-2026-49854"] =
                toml_edit::value("6.5.6");
        })
        .unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# why this profile exists"), "{text}");
        assert!(text.contains("# the tracker to block"), "{text}");
        assert!(text.contains("CVE-2026-49854"), "{text}");
        // and it still parses, with both the old and the new value
        let table: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            table["check"]["bodhi-check"]["tracker_bug"].as_str(),
            Some("CVE-Tracker")
        );
        assert_eq!(
            table["check"]["bodhi-check"]["fixed_versions"]["CVE-2026-49854"].as_str(),
            Some("6.5.6")
        );
    }

    #[test]
    fn edit_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("run.toml");
        let cf = ConfigFile::from_path(path.clone());
        cf.edit(|doc| {
            doc["check"]["bodhi-check"]["fixed_versions"]["CVE-1"] = toml_edit::value("1")
        })
        .unwrap();

        let table: toml::Table = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            table["check"]["bodhi-check"]["fixed_versions"]["CVE-1"].as_str(),
            Some("1")
        );
    }
}
