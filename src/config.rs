use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct NodejsFpsConfig {
    pub tracker_bug: String,
    pub products: Vec<String>,
    pub components: Vec<String>,
    pub statuses: Vec<String>,
}

impl NodejsFpsConfig {
    pub fn from_file(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = std::fs::read_to_string(path)?;
        let config: Self = toml::from_str(&contents)?;
        Ok(config)
    }
}
