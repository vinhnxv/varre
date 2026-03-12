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
use crate::session::{HeadlessSession, SessionId, SessionKind, SessionStore};

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

    /// Create a new headless session with the given name and optional working directory.
    pub async fn create_session(
        &mut self,
        name: &str,
        working_dir: Option<PathBuf>,
    ) -> Result<SessionId> {
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

        let session = match self.sessions.get(&id) {
            Some(SessionKind::Headless(s)) => s,
            None => return Err(VarreError::SessionNotFound(name.to_string()).into()),
        };

        // Verify the session can accept a prompt.
        let state = session.state().await;
        match &state {
            SessionState::Ready => {}
            SessionState::Error { retry_count, .. } if *retry_count < MAX_RETRIES => {}
            SessionState::Busy => {
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
            ..Default::default()
        };

        match self.backend.execute(prompt, opts).await {
            Ok(response) => {
                // Transition back to Ready.
                let session = match self.sessions.get(&id) {
                    Some(SessionKind::Headless(s)) => s,
                    None => return Err(VarreError::SessionNotFound(name.to_string()).into()),
                };
                let _ = session
                    .send_event(&SessionEvent::Completed, MAX_RETRIES)
                    .await;
                self.consecutive_failures.store(0, Ordering::Relaxed);
                self.sessions.save()?;
                Ok(response)
            }
            Err(e) => {
                // Transition to Error.
                let session = match self.sessions.get(&id) {
                    Some(SessionKind::Headless(s)) => s,
                    None => return Err(VarreError::SessionNotFound(name.to_string()).into()),
                };
                let _ = session
                    .send_event(
                        &SessionEvent::Failed(e.to_string()),
                        MAX_RETRIES,
                    )
                    .await;
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
            if let Some(SessionKind::Headless(session)) = self.sessions.get(id) {
                let state = session.state().await;
                infos.push(SessionInfo {
                    id: id.clone(),
                    name: name.clone(),
                    state,
                    created_at: session.created_at,
                    working_dir: session.working_dir.clone(),
                });
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

        if let Some(SessionKind::Headless(session)) = self.sessions.get(&id) {
            session
                .send_event(&SessionEvent::Killed, MAX_RETRIES)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }

        self.sessions.remove(&id);
        self.names.remove(name);
        self.sessions.save()?;
        save_names(&Config::data_dir(), &self.names)?;

        tracing::info!(session_name = name, "session killed");
        Ok(())
    }

    /// Return a reference to the cancellation token.
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel_token
    }
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

/// Persist the name-to-ID mapping to a JSON file.
fn save_names(data_dir: &std::path::Path, names: &HashMap<String, SessionId>) -> Result<()> {
    let path = data_dir.join("names.json");
    let raw: HashMap<&str, &str> = names
        .iter()
        .map(|(name, id)| (name.as_str(), id.as_str()))
        .collect();
    let json = serde_json::to_string_pretty(&raw).context("failed to serialize names")?;
    std::fs::write(&path, json).context("failed to write names file")?;
    Ok(())
}
