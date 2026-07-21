// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Validator function for [`prompt_field`].
type Validator<'a> = Option<&'a dyn Fn(&str) -> Result<(), String>>;

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
        let path = dirs::config_dir()?.join(tool_name).join("config.toml");
        Some(Self {
            path,
            secure: true,
            system_path: Some(system_config_path(tool_name)),
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
        match &self.system_path {
            Some(sys) => format!("{} + {}", sys.display(), self.path.display()),
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

    /// Serialize and save the **user** config file, creating parent
    /// directories as needed — the system layer is never written.
    /// For user config files (`for_tool`), sets directory
    /// permissions to 700 and file permissions to 600.
    ///
    /// Note: `save(load()?)` would bake system-layer values into
    /// the user file; interactive `config` flows that rewrite the
    /// file should keep that in mind (acceptable today — the
    /// prompted fields are per-user anyway).
    pub fn save<T: Serialize>(&self, config: &T) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
            if self.secure {
                set_dir_permissions(parent)?;
            }
        }
        let contents = toml::to_string_pretty(config)?;
        std::fs::write(&self.path, &contents)?;
        if self.secure {
            set_file_permissions(&self.path)?;
        }
        Ok(())
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
        print!("Enter {section} {label}: ");
        io::stdout().flush()?;

        let raw = if sensitive {
            rpassword::read_password()?
        } else {
            let mut buf = String::new();
            io::stdin().read_line(&mut buf)?;
            buf
        };

        let value = raw.trim().to_string();
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

/// Validate that a string looks like an email address.
///
/// Suitable for use as a `validate` callback in [`prompt_field`].
pub fn validate_email(value: &str) -> Result<(), String> {
    if !value.contains('@') {
        return Err("must contain '@'".to_string());
    }
    let (local, domain) = value.split_once('@').unwrap();
    if local.is_empty() || domain.is_empty() || !domain.contains('.') {
        return Err("invalid email address".to_string());
    }
    Ok(())
}

/// System-wide config path for a tool: `/etc/<tool>/config.toml`.
/// Read-only from our side; owned by the admin / the distro
/// package. Layered beneath the per-user file by
/// [`ConfigFile::load`] / [`ConfigFile::read_merged`].
pub fn system_config_path(tool_name: &str) -> PathBuf {
    Path::new("/etc").join(tool_name).join("config.toml")
}

/// Recursively merge two TOML tables: `over` wins per key;
/// nested tables merge, everything else (scalars, arrays) is
/// replaced whole.
pub fn merge_tables(mut base: toml::Table, over: toml::Table) -> toml::Table {
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
    fn for_tool_path_ends_with_tool_name() {
        let cf = ConfigFile::for_tool("my-tool");
        assert!(cf.path().ends_with("my-tool/config.toml"));
    }

    #[test]
    fn system_config_path_is_under_etc() {
        assert_eq!(
            system_config_path("dbranch"),
            Path::new("/etc/dbranch/config.toml")
        );
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

        // No layer at all keeps the "run config" error.
        std::fs::remove_file(&system).unwrap();
        let err = cf.load::<TestConfig>().unwrap_err().to_string();
        assert!(err.contains("Run 'config'"), "{err}");
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
}
