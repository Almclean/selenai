use std::collections::HashMap;
use std::path::Path;
use std::fs;
use anyhow::Result;
use serde::Deserialize;

#[derive(Debug, Deserialize, Default)]
pub struct MacroConfig {
    pub macros: HashMap<String, String>,
}

impl MacroConfig {
    pub fn load() -> Result<Self> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = Path::new(&home).join(".config/selenai/macros.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = fs::read_to_string(&path)?;
        let config: MacroConfig = toml::from_str(&content)?;
        Ok(config)
    }
}
