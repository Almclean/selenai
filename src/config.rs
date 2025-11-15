use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

const DEFAULT_CONFIG_BASENAME: &str = "selenai.toml";
const DEFAULT_MODEL_ID: &str = "gpt-4o-mini";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub provider: ProviderKind,
    pub model_id: String,
    pub streaming: bool,
    pub allow_tool_writes: bool,
    pub openai: OpenAiSection,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let path = config_path_from_env();
        if !path.exists() {
            return Ok(Self::default());
        }

        let data = fs::read_to_string(&path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let mut cfg: AppConfig = toml::from_str(&data)
            .with_context(|| format!("invalid config format in {}", path.display()))?;
        cfg.normalize();
        Ok(cfg)
    }

    fn normalize(&mut self) {
        if self.model_id.trim().is_empty() {
            self.model_id = DEFAULT_MODEL_ID.to_string();
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            provider: ProviderKind::default(),
            model_id: DEFAULT_MODEL_ID.to_string(),
            streaming: true,
            allow_tool_writes: false,
            openai: OpenAiSection::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    #[default]
    Stub,
    OpenAi,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenAiSection {
    pub base_url: Option<String>,
    pub organization: Option<String>,
    pub project: Option<String>,
}

fn config_path_from_env() -> PathBuf {
    std::env::var("SELENAI_CONFIG")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_BASENAME))
}
