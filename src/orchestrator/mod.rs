use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

use crate::backend::{ClaudeBackend, ClaudeResponse, ExecOptions, OutputFormat};
use crate::config::Config;
use crate::error::VarreError;
use crate::session::state::{SessionEvent, SessionState};
use crate::session::{HeadlessSession, InteractiveSession, SessionId, SessionKind, SessionStore};
use crate::tmux::TmuxWrapper;

/// Maximum consecutive failures before the circuit breaker opens.
const CIRCUIT_BREAKER_THRESHOLD: u32 = 3;

/// Default max retries for session state transitions.
const MAX_RETRIES: u32 = 3;

/// Summary information about a session.
pub struct SessionInfo {
    /// Unique session identifier.
    pub id: SessionId,
    /// Human-readable session name.
    pub name: String,
    /// Current lifecycle state.
    pub state: SessionState,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// Working directory for the session.
    pub working_dir: PathBuf,
}

/// Central orchestrator managing Claude Code sessions.
pub struct Orchestrator<B: ClaudeBackend> {
    /// Persistent session store.
    sessions: SessionStore,
    /// Name-to-ID mapping for human-friendly lookups.
    names: HashMap<String, SessionId>,
    /// Backend for executing Claude CLI commands.
    backend: Arc<B>,
    /// Application configuration.
    config: Config,
    /// Token for cooperative cancellation.
    cancel_token: CancellationToken,
    /// Consecutive backend failure counter for circuit breaker.
    consecutive_failures: AtomicU32,
}

impl<B: ClaudeBackend> Orchestrator<B> {
    /// Create a new orchestrator, loading persisted sessions from the data directory.
    pub fn new(
        config: Config,
        backend: Arc<B>,
        cancel_token: CancellationToken,
    ) -> Result<Self> {
        let data_dir = Config::data_dir();
        std::fs::create_dir_all(&data_dir).context("failed to create data directory")?;

        let sessions_path = data_dir.join("sessions.json");
        let sessions = SessionStore::load(&sessions_path)?;

        let names = load_names(&data_dir)?;

        Ok(Self {
            sessions,
            names,
            backend,
            config,
            cancel_token,
            consecutive_failures: AtomicU32::new(0),
        })
    }

    /// Create a new interactive (tmux) session with the given name and optional working directory.
    pub async fn create_interactive_session(
        &mut self,
        name: &str,
        working_dir: Option<PathBuf>,
    ) -> Result<SessionId> {
        validate_session_name(name)?;

        if self.names.contains_key(name) {
            bail!("session with name '{}' already exists", name);
        }

        let tmux = Arc::new(TmuxWrapper::new(&self.config.tmux));

        // Check tmux is available
        tmux.check_available().await?;

        let dir = working_dir.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        // Create tmux session (200x50 for Ink rendering)
        tmux.create_session(name, (200, 50)).await?;

        let session = InteractiveSession::new(dir, self.config.claude.clone(), tmux.clone());
        let id = session.id.clone();

        // Start Claude Code in the tmux session using configured binary path
        tmux.start_claude_with_binary(name, &self.config.claude.binary).await?;

        // Transition Creating -> Ready
        session
            .send_event(&SessionEvent::Spawned, MAX_RETRIES)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        self.names.insert(name.to_string(), id.clone());
        self.sessions
            .add(id.clone(), SessionKind::Interactive(session));
        self.sessions.save()?;
        save_names(&Config::data_dir(), &self.names)?;

        tracing::info!(session_name = name, session_id = %id, "interactive session created");
        Ok(id)
    }

    /// Capture output from an interactive session.
    pub async fn capture_output(&self, name: &str, lines: i32) -> Result<String> {
        let id = self
            .names
            .get(name)
            .ok_or_else(|| VarreError::SessionNotFound(name.to_string()))?;

        match self.sessions.get(id) {
            Some(SessionKind::Interactive(session)) => session.capture(name, lines).await,
            Some(SessionKind::Headless(_)) => {
                bail!("capture is only available for interactive (tmux) sessions")
            }
            None => Err(VarreError::SessionNotFound(name.to_string()).into()),
        }
    }

    /// Create a new headless session with the given name and optional working directory.
    pub async fn create_session(
        &mut self,
        name: &str,
        working_dir: Option<PathBuf>,
    ) -> Result<SessionId> {
        validate_session_name(name)?;

        if self.names.contains_key(name) {
            bail!("session with name '{}' already exists", name);
        }

        let dir = working_dir.unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        });

        let session = HeadlessSession::new(dir, self.config.claude.clone());
        let id = session.id.clone();

        // Transition Creating -> Ready.
        session
            .send_event(&SessionEvent::Spawned, MAX_RETRIES)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        self.names.insert(name.to_string(), id.clone());
        self.sessions
            .add(id.clone(), SessionKind::Headless(session));
        self.sessions.save()?;
        save_names(&Config::data_dir(), &self.names)?;

        tracing::info!(session_name = name, session_id = %id, "session created");
        Ok(id)
    }

    /// Send a prompt to a named session and return the response.
    pub async fn send_prompt(
        &mut self,
        name: &str,
        prompt: &str,
    ) -> Result<ClaudeResponse> {
        if prompt.trim().is_empty() {
            bail!("prompt cannot be empty");
        }

        let failures = self.consecutive_failures.load(Ordering::Relaxed);
        if failures >= CIRCUIT_BREAKER_THRESHOLD {
            return Err(VarreError::CircuitBreakerOpen {
                consecutive_failures: failures,
            }
            .into());
        }

        let id = self
            .names
            .get(name)
            .ok_or_else(|| VarreError::SessionNotFound(name.to_string()))?
            .clone();

        // Handle interactive sessions separately
        if let Some(SessionKind::Interactive(session)) = self.sessions.get(&id) {
            let state = session.state().await;
            match &state {
                SessionState::Ready => {}
                SessionState::Busy { .. } => {
                    return Err(VarreError::SessionBusy(name.to_string()).into());
                }
                _ => {
                    return Err(VarreError::SessionBusy(name.to_string()).into());
                }
            }
            session
                .send_event(&SessionEvent::PromptSent, MAX_RETRIES)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            session.send(name, prompt).await?;
            // Transition back to Ready immediately — tmux send-keys is fire-and-forget,
            // the polling task will detect the actual Claude status separately.
            if let Err(e) = session
                .send_event(&SessionEvent::Completed, MAX_RETRIES)
                .await
            {
                tracing::error!(session = name, error = %e, "failed to transition interactive session back to Ready");
            }
            // Return a synthetic response for interactive sessions
            return Ok(ClaudeResponse {
                result: "prompt sent to interactive session".to_string(),
                session_id: session.id.0.clone(),
                cost_usd: None,
                duration_ms: None,
                stderr: None,
                truncated: false,
                model: None,
            });
        }

        let session = match self.sessions.get(&id) {
            Some(SessionKind::Headless(s)) => s,
            _ => return Err(VarreError::SessionNotFound(name.to_string()).into()),
        };

        // Verify the session can accept a prompt.
        let state = session.state().await;
        match &state {
            SessionState::Ready => {}
            SessionState::Error { retry_count, .. } if *retry_count < MAX_RETRIES => {}
            SessionState::Busy { .. } => {
                return Err(VarreError::SessionBusy(name.to_string()).into());
            }
            _ => {
                return Err(VarreError::SessionBusy(name.to_string()).into());
            }
        }

        // Transition to Busy.
        session
            .send_event(&SessionEvent::PromptSent, MAX_RETRIES)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;

        let opts = ExecOptions {
            output_format: OutputFormat::Json,
            resume_session_id: session.last_session_id.clone(),
            allowed_tools: session.config.allowed_tools.clone(),
            max_turns: Some(session.config.max_turns),
            model: Some(session.config.model.clone()),
            working_dir: Some(session.working_dir.clone()),
            timeout_secs: Some(self.config.defaults.timeout_seconds),
            max_budget_usd: Some(self.config.claude.max_budget_usd),
            ..Default::default()
        };

        match self.backend.execute(prompt, opts).await {
            Ok(response) => {
                // Transition back to Ready.
                let session = match self.sessions.get(&id) {
                    Some(SessionKind::Headless(s)) => s,
                    _ => return Err(VarreError::SessionNotFound(name.to_string()).into()),
                };
                if let Err(e) = session
                    .send_event(&SessionEvent::Completed, MAX_RETRIES)
                    .await
                {
                    tracing::error!(session = name, error = %e, "failed to transition session to Ready after completion");
                }
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.sessions.save()?;
                Ok(response)
            }
            Err(e) => {
                // Transition to Error.
                let session = match self.sessions.get(&id) {
                    Some(SessionKind::Headless(s)) => s,
                    _ => return Err(VarreError::SessionNotFound(name.to_string()).into()),
                };
                if let Err(te) = session
                    .send_event(
                        &SessionEvent::Failed(e.to_string()),
                        MAX_RETRIES,
                    )
                    .await
                {
                    tracing::error!(session = name, error = %te, "failed to transition session to Error after failure");
                }
                self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                self.sessions.save()?;
                Err(e)
            }
        }
    }

    /// List all sessions with their current state.
    pub async fn list_sessions(&self) -> Vec<SessionInfo> {
        let mut infos = Vec::new();

        for (name, id) in &self.names {
            match self.sessions.get(id) {
                Some(SessionKind::Headless(session)) => {
                    let state = session.state().await;
                    infos.push(SessionInfo {
                        id: id.clone(),
                        name: name.clone(),
                        state,
                        created_at: session.created_at,
                        working_dir: session.working_dir.clone(),
                    });
                }
                Some(SessionKind::Interactive(session)) => {
                    let state = session.state().await;
                    infos.push(SessionInfo {
                        id: id.clone(),
                        name: name.clone(),
                        state,
                        created_at: session.created_at,
                        working_dir: session.working_dir.clone(),
                    });
                }
                None => {}
            }
        }

        infos
    }

    /// Kill a session by name, transitioning it to Dead and removing it.
    pub async fn kill_session(&mut self, name: &str) -> Result<()> {
        let id = self
            .names
            .get(name)
            .ok_or_else(|| VarreError::SessionNotFound(name.to_string()))?
            .clone();

        match self.sessions.get(&id) {
            Some(SessionKind::Headless(session)) => {
                session
                    .send_event(&SessionEvent::Killed, MAX_RETRIES)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
            }
            Some(SessionKind::Interactive(session)) => {
                session
                    .send_event(&SessionEvent::Killed, MAX_RETRIES)
                    .await
                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                // Kill the tmux session
                let tmux = TmuxWrapper::new(&self.config.tmux);
                let _ = tmux.kill_session(name).await;
            }
            None => {}
        }

        self.sessions.remove(&id);
        self.names.remove(name);
        self.sessions.save()?;
        save_names(&Config::data_dir(), &self.names)?;

        tracing::info!(session_name = name, "session killed");
        Ok(())
    }

}

/// Validate a session name: must be 1-64 chars, alphanumeric + hyphens/underscores.
fn validate_session_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("session name cannot be empty");
    }
    if name.len() > 64 {
        bail!("session name too long (max 64 characters)");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("session name must contain only alphanumeric characters, hyphens, and underscores");
    }
    Ok(())
}

/// Load the name-to-ID mapping from a JSON file.
fn load_names(data_dir: &std::path::Path) -> Result<HashMap<String, SessionId>> {
    let path = data_dir.join("names.json");
    if path.exists() {
        let content = std::fs::read_to_string(&path).context("failed to read names file")?;
        let raw: HashMap<String, String> =
            serde_json::from_str(&content).context("failed to parse names file")?;
        Ok(raw
            .into_iter()
            .map(|(name, id)| (name, SessionId::from_string(id)))
            .collect())
    } else {
        Ok(HashMap::new())
    }
}

/// Persist the name-to-ID mapping to a JSON file (atomic: write tmp → fsync → rename).
fn save_names(data_dir: &std::path::Path, names: &HashMap<String, SessionId>) -> Result<()> {
    use std::io::Write;

    let path = data_dir.join("names.json");
    let tmp_path = data_dir.join("names.json.tmp");
    let raw: HashMap<&str, &str> = names
        .iter()
        .map(|(name, id)| (name.as_str(), id.as_str()))
        .collect();
    let json = serde_json::to_string_pretty(&raw).context("failed to serialize names")?;

    let mut file = std::fs::File::create(&tmp_path).context("failed to create temp names file")?;
    file.write_all(json.as_bytes())
        .context("failed to write temp names file")?;
    file.sync_all().context("failed to fsync names file")?;
    std::fs::rename(&tmp_path, &path).context("failed to rename names file")?;
    Ok(())
}
