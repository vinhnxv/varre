use anyhow::{bail, Context, Result};
use std::process::Stdio;
use tokio::process::Command;

use super::{ClaudeBackend, ClaudeResponse, ExecOptions};

/// Maximum stdout size in bytes (50 MB).
const MAX_STDOUT_BYTES: usize = 50 * 1024 * 1024;

/// Grace period after SIGTERM before SIGKILL.
const SIGTERM_GRACE_SECS: u64 = 5;

/// Backend that shells out to the `claude` CLI.
pub struct CliBackend {
    /// Path or name of the claude binary.
    binary: String,
}

impl CliBackend {
    /// Create a new CLI backend using the default `claude` binary.
    pub fn new() -> Self {
        Self {
            binary: "claude".into(),
        }
    }

    /// Create a CLI backend pointing at a specific binary path.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    /// Build the argument list from prompt and options.
    fn build_args(prompt: &str, opts: &ExecOptions) -> Vec<String> {
        let mut args = vec![
            "-p".into(),
            prompt.into(),
            "--output-format".into(),
            opts.output_format.as_str().into(),
        ];

        if let Some(ref session_id) = opts.resume_session_id {
            args.push("--resume".into());
            args.push(session_id.clone());
        }

        for tool in &opts.allowed_tools {
            args.push("--allowedTools".into());
            args.push(tool.clone());
        }

        if let Some(turns) = opts.max_turns {
            args.push("--max-turns".into());
            args.push(turns.to_string());
        }

        if let Some(model) = &opts.model {
            args.push("--model".into());
            args.push(model.clone());
        }

        if let Some(prompt_text) = &opts.system_prompt {
            args.push("--system-prompt".into());
            args.push(prompt_text.clone());
        }

        if let Some(append) = &opts.append_system_prompt {
            args.push("--append-system-prompt".into());
            args.push(append.clone());
        }

        if let Some(budget) = opts.max_budget_usd {
            args.push("--max-budget".into());
            args.push(budget.to_string());
        }

        args
    }
}

impl Default for CliBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeBackend for CliBackend {
    async fn execute(&self, prompt: &str, opts: ExecOptions) -> Result<ClaudeResponse> {
        let args = Self::build_args(prompt, &opts);
        let timeout_secs = opts.timeout_secs.unwrap_or(300);

        let mut cmd = Command::new(&self.binary);
        cmd.args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(ref dir) = opts.working_dir {
            cmd.current_dir(dir);
        }

        let mut child = cmd.spawn().context("failed to spawn claude process")?;

        // Drain stdout/stderr concurrently with wait to avoid pipe deadlock.
        use tokio::io::AsyncReadExt;

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let stdout_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut stdout) = stdout_handle {
                let _ = stdout.read_to_end(&mut buf).await;
            }
            buf
        });
        let stderr_task = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(mut stderr) = stderr_handle {
                let _ = stderr.read_to_end(&mut buf).await;
            }
            buf
        });

        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let status = match tokio::time::timeout(timeout_duration, child.wait()).await {
            Ok(result) => result.context("claude process failed")?,
            Err(_) => {
                // Timeout: graceful SIGTERM → grace period → SIGKILL
                #[cfg(unix)]
                {
                    let _ = child.kill().await;
                    tokio::time::sleep(std::time::Duration::from_secs(SIGTERM_GRACE_SECS))
                        .await;
                }

                #[cfg(not(unix))]
                {
                    let _ = child.kill().await;
                }

                bail!("claude process timed out after {timeout_secs}s");
            }
        };

        let stdout_bytes_vec = stdout_task.await.unwrap_or_default();
        let stderr_bytes_vec = stderr_task.await.unwrap_or_default();

        let output = std::process::Output {
            status,
            stdout: stdout_bytes_vec,
            stderr: stderr_bytes_vec,
        };

        let stderr_text = String::from_utf8_lossy(&output.stderr);
        let stderr_opt = if stderr_text.is_empty() {
            None
        } else {
            Some(stderr_text.into_owned())
        };

        let mut truncated = false;
        let stdout_bytes = if output.stdout.len() > MAX_STDOUT_BYTES {
            truncated = true;
            &output.stdout[..MAX_STDOUT_BYTES]
        } else {
            &output.stdout
        };
        let stdout_text = String::from_utf8_lossy(stdout_bytes);

        if !output.status.success() {
            let preview: String = stdout_text.chars().take(500).collect();
            bail!(
                "claude exited with status {}: {}",
                output.status,
                preview
            );
        }

        let mut response: ClaudeResponse =
            serde_json::from_str(&stdout_text).with_context(|| {
                let preview: String = stdout_text.chars().take(500).collect();
                format!("failed to parse claude JSON output: {preview}")
            })?;

        response.truncated = truncated;
        response.stderr = stderr_opt;

        Ok(response)
    }

}
