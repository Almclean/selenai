use std::{
    fs,
    path::{Path, PathBuf},
};

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
    pub log_dir: Option<PathBuf>,
    pub openai: OpenAiSection,
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let path = config_path_from_env();
        Self::load_from_path(&path)
    }

    fn load_from_path(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let data = fs::read_to_string(path)
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

    pub fn resolve_log_dir(&self, workspace_root: &Path) -> PathBuf {
        let configured = self
            .log_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(".selenai/logs"));
        if configured.is_absolute() {
            configured
        } else {
            workspace_root.join(configured)
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
            log_dir: None,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn with_temp_config<F: FnOnce(&PathBuf)>(contents: Option<&str>, f: F) {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("selenai-test.toml");
        if let Some(data) = contents {
            fs::write(&path, data).expect("write config");
        }
        f(&path);
    }

    #[test]
    fn load_returns_defaults_when_missing() {
        with_temp_config(None, |path| {
            let cfg = AppConfig::load_from_path(path).expect("default config");
            assert_eq!(cfg.model_id, DEFAULT_MODEL_ID);
            assert!(matches!(cfg.provider, ProviderKind::Stub));
        });
    }

    #[test]
    fn load_normalizes_blank_model_id() {
        with_temp_config(
            Some(
                r#"
provider = "openai"
model_id = ""
streaming = false
"#,
            ),
            |path| {
                let cfg = AppConfig::load_from_path(path).expect("config");
                assert_eq!(cfg.model_id, DEFAULT_MODEL_ID);
                assert!(
                    !cfg.streaming,
                    "explicit streaming flag should be preserved"
                );
            },
        );
    }

    #[test]
    fn resolve_log_dir_honors_defaults_and_overrides() {
        let workspace = tempdir().expect("workspace");
        let cfg = AppConfig::default();
        assert_eq!(
            cfg.resolve_log_dir(workspace.path()),
            workspace.path().join(".selenai/logs")
        );

        let mut cfg = AppConfig::default();
        cfg.log_dir = Some(PathBuf::from("custom/logs"));
        assert_eq!(
            cfg.resolve_log_dir(workspace.path()),
            workspace.path().join("custom/logs")
        );

        let mut cfg = AppConfig::default();
        cfg.log_dir = Some(PathBuf::from("/var/tmp/runlogs"));
        assert_eq!(
            cfg.resolve_log_dir(workspace.path()),
            PathBuf::from("/var/tmp/runlogs")
        );
    }
}
