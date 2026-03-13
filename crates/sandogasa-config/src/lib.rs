// SPDX-License-Identifier: MPL-2.0

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Manages a TOML config file at `~/.config/{tool}/config.toml`.
pub struct ConfigFile {
    path: PathBuf,
}

impl ConfigFile {
    /// Create a `ConfigFile` for the given tool name.
    ///
    /// The config path will be `~/.config/{tool_name}/config.toml`.
    pub fn for_tool(tool_name: &str) -> Self {
        let path = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("~/.config"))
            .join(tool_name)
            .join("config.toml");
        Self { path }
    }

    /// Create a `ConfigFile` with an explicit path (useful for testing).
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Return the config file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load and deserialize the config file.
    pub fn load<T: DeserializeOwned>(&self) -> Result<T, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(&self.path).map_err(|e| {
            format!(
                "Could not read {}: {e}. Run 'config' to set up.",
                self.path.display()
            )
        })?;
        let config: T = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Serialize and save the config file, creating parent directories
    /// as needed.
    pub fn save<T: Serialize>(&self, config: &T) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents = toml::to_string_pretty(config)?;
        std::fs::write(&self.path, contents)?;
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
    validate: Option<&dyn Fn(&str) -> Result<(), String>>,
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
}
