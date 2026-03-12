use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub defaults: DefaultsConfig,
    pub claude: ClaudeConfig,
    pub tmux: TmuxConfig,
    pub tui: TuiConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DefaultsConfig {
    pub mode: SessionMode,
    pub max_concurrency: usize,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMode {
    Headless,
    Interactive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ClaudeConfig {
    pub allowed_tools: Vec<String>,
    pub max_turns: u32,
    pub max_budget_usd: f64,
    pub model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TmuxConfig {
    pub prompt_marker: String,
    pub poll_interval_ms: u64,
    pub send_delay_ms: u64,
    pub session_prefix: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TuiConfig {
    pub refresh_rate_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            defaults: DefaultsConfig::default(),
            claude: ClaudeConfig::default(),
            tmux: TmuxConfig::default(),
            tui: TuiConfig::default(),
        }
    }
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            mode: SessionMode::Headless,
            max_concurrency: 3,
            timeout_seconds: 300,
        }
    }
}

impl Default for ClaudeConfig {
    fn default() -> Self {
        Self {
            allowed_tools: vec!["Read".into(), "Edit".into(), "Bash".into()],
            max_turns: 50,
            max_budget_usd: 5.0,
            model: "sonnet".into(),
        }
    }
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            prompt_marker: "❯".into(),
            poll_interval_ms: 1000,
            send_delay_ms: 300,
            session_prefix: "varre-".into(),
        }
    }
}

impl Default for TuiConfig {
    fn default() -> Self {
        Self {
            refresh_rate_ms: 100,
        }
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let content =
                std::fs::read_to_string(&path).context("failed to read config file")?;
            toml::from_str(&content).context("failed to parse config file")
        } else {
            Ok(Self::default())
        }
    }

    pub fn config_path() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("varre")
            .join("config.toml")
    }

    pub fn data_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("varre")
    }
}
