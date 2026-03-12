use anyhow::{bail, Result};
use std::time::Duration;

use super::{ClaudeBackend, ClaudeResponse, ExecOptions};

/// Mock backend for testing without a real Claude CLI.
pub struct MockBackend {
    response: Option<ClaudeResponse>,
    error: Option<String>,
    delay: Option<Duration>,
}

impl MockBackend {
    /// Create a mock that returns a default success response.
    pub fn new() -> Self {
        Self {
            response: Some(ClaudeResponse {
                result: "mock response".into(),
                session_id: "mock-session-001".into(),
                cost_usd: Some(0.01),
                duration_ms: Some(100),
                stderr: None,
                truncated: false,
                model: Some("sonnet".into()),
            }),
            error: None,
            delay: None,
        }
    }

    /// Create a mock that returns a custom response.
    pub fn with_response(response: ClaudeResponse) -> Self {
        Self {
            response: Some(response),
            error: None,
            delay: None,
        }
    }

    /// Create a mock that returns an error.
    pub fn with_error(message: impl Into<String>) -> Self {
        Self {
            response: None,
            error: Some(message.into()),
            delay: None,
        }
    }

    /// Create a mock that sleeps before returning.
    pub fn with_delay(delay: Duration) -> Self {
        let mut mock = Self::new();
        mock.delay = Some(delay);
        mock
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeBackend for MockBackend {
    async fn execute(&self, _prompt: &str, _opts: ExecOptions) -> Result<ClaudeResponse> {
        if let Some(delay) = self.delay {
            tokio::time::sleep(delay).await;
        }

        if let Some(ref err) = self.error {
            bail!("{err}");
        }

        Ok(self
            .response
            .clone()
            .expect("MockBackend has neither response nor error"))
    }

}
