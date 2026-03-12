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

        // Take stdout/stderr handles before waiting, so we retain access to `child` for kill.
        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let timeout_duration = std::time::Duration::from_secs(timeout_secs);
        let status = match tokio::time::timeout(timeout_duration, child.wait()).await {
            Ok(result) => result.context("claude process failed")?,
            Err(_) => {
                // Timeout: graceful SIGTERM → grace period → SIGKILL
                #[cfg(unix)]
                {
                    if let Some(pid) = child.id() {
                        unsafe {
                            libc::kill(pid as i32, libc::SIGTERM);
                        }
                        tokio::time::sleep(std::time::Duration::from_secs(SIGTERM_GRACE_SECS))
                            .await;
                    }
                    let _ = child.kill().await;
                }

                #[cfg(not(unix))]
                {
                    let _ = child.kill().await;
                }

                bail!("claude process timed out after {timeout_secs}s");
            }
        };

        // Read captured stdout and stderr.
        use tokio::io::AsyncReadExt;
        let mut stdout_bytes_vec = Vec::new();
        if let Some(mut stdout) = stdout_handle {
            stdout.read_to_end(&mut stdout_bytes_vec).await.context("failed to read stdout")?;
        }
        let mut stderr_bytes_vec = Vec::new();
        if let Some(mut stderr) = stderr_handle {
            stderr.read_to_end(&mut stderr_bytes_vec).await.context("failed to read stderr")?;
        }

        let output = std::process::Output {
            status: status,
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

    async fn version(&self) -> Result<String> {
        let output = Command::new(&self.binary)
            .arg("--version")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("failed to run claude --version")?;

        if !output.status.success() {
            bail!(
                "claude --version exited with status {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(version)
    }
}
