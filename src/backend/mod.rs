pub mod cli;
pub mod mock;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub use cli::CliBackend;
pub use mock::MockBackend;

/// Output format for Claude CLI responses.
#[derive(Debug, Clone, Default)]
pub enum OutputFormat {
    #[default]
    Json,
    StreamJson,
}

impl OutputFormat {
    /// Return the CLI flag value for this format.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Json => "json",
            Self::StreamJson => "stream-json",
        }
    }
}

/// Options for executing a Claude CLI command.
#[derive(Debug, Clone, Default)]
pub struct ExecOptions {
    pub output_format: OutputFormat,
    pub resume_session_id: Option<String>,
    pub allowed_tools: Vec<String>,
    pub max_turns: Option<u32>,
    pub max_budget_usd: Option<f64>,
    pub model: Option<String>,
    pub working_dir: Option<PathBuf>,
    pub timeout_secs: Option<u64>,
    pub system_prompt: Option<String>,
    pub append_system_prompt: Option<String>,
}

/// Parsed response from Claude CLI JSON output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeResponse {
    pub result: String,
    pub session_id: String,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub stderr: Option<String>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub model: Option<String>,
}

/// Backend trait for executing Claude CLI commands.
#[allow(async_fn_in_trait)]
pub trait ClaudeBackend: Send + Sync {
    /// Execute a prompt and return the parsed response.
    async fn execute(&self, prompt: &str, opts: ExecOptions) -> Result<ClaudeResponse>;

    /// Return the Claude CLI version string.
    async fn version(&self) -> Result<String>;
}
