use serde::{Deserialize, Serialize};

use crate::error::VarreError;

/// Represents the lifecycle state of a Claude Code session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionState {
    /// Session is being created (spawning process).
    Creating,
    /// Session is idle and ready to accept prompts.
    Ready,
    /// Session is actively processing a prompt.
    /// Carries retry context from Error state for proper accumulation.
    Busy { retry_count: u32 },
    /// Session is waiting for user input (permission prompt).
    WaitingInput,
    /// Session encountered an error and may be retried.
    Error {
        /// Number of consecutive retries attempted.
        retry_count: u32,
        /// Description of the last error.
        last_error: String,
    },
    /// Session is permanently terminated.
    Dead,
}

/// Events that trigger state transitions in a session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum SessionEvent {
    /// Session process has been spawned successfully.
    Spawned,
    /// A prompt was sent to the session.
    PromptSent,
    /// Session completed processing.
    Completed,
    /// Session encountered a failure.
    Failed(String),
    /// Session received a permission prompt.
    PermissionPrompt,
    /// Permission prompt was resolved by the user.
    PermissionResolved,
    /// Session timed out.
    Timeout,
    /// Session was killed.
    Killed,
    /// Retry attempts exhausted.
    RetryExhausted,
}

impl SessionState {
    /// Attempt to transition to a new state given an event.
    ///
    /// Returns the new state on a valid transition, or `VarreError::InvalidTransition`
    /// if the transition is not allowed.
    pub fn transition(
        &self,
        event: &SessionEvent,
        max_retries: u32,
    ) -> Result<SessionState, VarreError> {
        match (self, event) {
            // Creating transitions
            (SessionState::Creating, SessionEvent::Spawned) => Ok(SessionState::Ready),
            (SessionState::Creating, SessionEvent::Failed(msg)) => Ok(SessionState::Error {
                retry_count: 0,
                last_error: msg.clone(),
            }),
            (SessionState::Creating, SessionEvent::Killed) => Ok(SessionState::Dead),

            // Ready transitions
            (SessionState::Ready, SessionEvent::PromptSent) => {
                Ok(SessionState::Busy { retry_count: 0 })
            }
            (SessionState::Ready, SessionEvent::Killed) => Ok(SessionState::Dead),

            // Busy transitions — retry_count is carried through
            (SessionState::Busy { retry_count }, SessionEvent::Completed) => {
                let _ = retry_count; // consumed
                Ok(SessionState::Ready)
            }
            (SessionState::Busy { retry_count }, SessionEvent::Failed(msg)) => {
                Ok(SessionState::Error {
                    retry_count: retry_count + 1,
                    last_error: msg.clone(),
                })
            }
            (SessionState::Busy { retry_count }, SessionEvent::PermissionPrompt) => {
                let _ = retry_count;
                Ok(SessionState::WaitingInput)
            }
            (SessionState::Busy { retry_count }, SessionEvent::Timeout) => {
                Ok(SessionState::Error {
                    retry_count: retry_count + 1,
                    last_error: "timeout".to_string(),
                })
            }
            (SessionState::Busy { .. }, SessionEvent::Killed) => Ok(SessionState::Dead),

            // WaitingInput transitions
            (SessionState::WaitingInput, SessionEvent::PermissionResolved) => {
                Ok(SessionState::Busy { retry_count: 0 })
            }
            (SessionState::WaitingInput, SessionEvent::Timeout) => Ok(SessionState::Error {
                retry_count: 0,
                last_error: "timeout waiting for input".to_string(),
            }),
            (SessionState::WaitingInput, SessionEvent::Killed) => Ok(SessionState::Dead),

            // Error transitions
            (
                SessionState::Error {
                    retry_count,
                    last_error: _,
                },
                SessionEvent::PromptSent,
            ) => {
                if *retry_count < max_retries {
                    Ok(SessionState::Busy {
                        retry_count: *retry_count,
                    })
                } else {
                    Err(VarreError::InvalidTransition {
                        from: format!("{:?}", self),
                        event: format!("{:?}", event),
                    })
                }
            }
            (SessionState::Error { .. }, SessionEvent::RetryExhausted) => Ok(SessionState::Dead),
            (SessionState::Error { .. }, SessionEvent::Killed) => Ok(SessionState::Dead),

            // All other transitions are invalid
            _ => Err(VarreError::InvalidTransition {
                from: format!("{:?}", self),
                event: format!("{:?}", event),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_creating_spawned_becomes_ready() {
        let state = SessionState::Creating;
        let result = state.transition(&SessionEvent::Spawned, 3).unwrap();
        assert_eq!(result, SessionState::Ready);
    }

    #[test]
    fn test_creating_failed_becomes_error() {
        let state = SessionState::Creating;
        let result = state
            .transition(&SessionEvent::Failed("boom".into()), 3)
            .unwrap();
        assert_eq!(
            result,
            SessionState::Error {
                retry_count: 0,
                last_error: "boom".into()
            }
        );
    }

    #[test]
    fn test_creating_killed_becomes_dead() {
        let state = SessionState::Creating;
        let result = state.transition(&SessionEvent::Killed, 3).unwrap();
        assert_eq!(result, SessionState::Dead);
    }

    #[test]
    fn test_ready_prompt_sent_becomes_busy() {
        let state = SessionState::Ready;
        let result = state.transition(&SessionEvent::PromptSent, 3).unwrap();
        assert_eq!(result, SessionState::Busy { retry_count: 0 });
    }

    #[test]
    fn test_busy_completed_becomes_ready() {
        let state = SessionState::Busy { retry_count: 0 };
        let result = state.transition(&SessionEvent::Completed, 3).unwrap();
        assert_eq!(result, SessionState::Ready);
    }

    #[test]
    fn test_busy_failed_becomes_error_with_retry_1() {
        let state = SessionState::Busy { retry_count: 0 };
        let result = state
            .transition(&SessionEvent::Failed("oops".into()), 3)
            .unwrap();
        assert_eq!(
            result,
            SessionState::Error {
                retry_count: 1,
                last_error: "oops".into()
            }
        );
    }

    #[test]
    fn test_retry_count_accumulates_across_cycles() {
        // Error{1} → PromptSent → Busy{1} → Failed → Error{2}
        let state = SessionState::Error {
            retry_count: 1,
            last_error: "first".into(),
        };
        let busy = state.transition(&SessionEvent::PromptSent, 3).unwrap();
        assert_eq!(busy, SessionState::Busy { retry_count: 1 });
        let error = busy
            .transition(&SessionEvent::Failed("second".into()), 3)
            .unwrap();
        assert_eq!(
            error,
            SessionState::Error {
                retry_count: 2,
                last_error: "second".into()
            }
        );
    }

    #[test]
    fn test_busy_permission_prompt_becomes_waiting() {
        let state = SessionState::Busy { retry_count: 0 };
        let result = state
            .transition(&SessionEvent::PermissionPrompt, 3)
            .unwrap();
        assert_eq!(result, SessionState::WaitingInput);
    }

    #[test]
    fn test_waiting_resolved_becomes_busy() {
        let state = SessionState::WaitingInput;
        let result = state
            .transition(&SessionEvent::PermissionResolved, 3)
            .unwrap();
        assert_eq!(result, SessionState::Busy { retry_count: 0 });
    }

    #[test]
    fn test_error_retry_within_limit() {
        let state = SessionState::Error {
            retry_count: 1,
            last_error: "oops".into(),
        };
        let result = state.transition(&SessionEvent::PromptSent, 3).unwrap();
        assert_eq!(result, SessionState::Busy { retry_count: 1 });
    }

    #[test]
    fn test_error_retry_at_limit_fails() {
        let state = SessionState::Error {
            retry_count: 3,
            last_error: "oops".into(),
        };
        let result = state.transition(&SessionEvent::PromptSent, 3);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_retry_exhausted_becomes_dead() {
        let state = SessionState::Error {
            retry_count: 3,
            last_error: "oops".into(),
        };
        let result = state
            .transition(&SessionEvent::RetryExhausted, 3)
            .unwrap();
        assert_eq!(result, SessionState::Dead);
    }

    #[test]
    fn test_dead_rejects_all_events() {
        let state = SessionState::Dead;
        assert!(state.transition(&SessionEvent::Spawned, 3).is_err());
        assert!(state.transition(&SessionEvent::PromptSent, 3).is_err());
        assert!(state.transition(&SessionEvent::Killed, 3).is_err());
    }

    #[test]
    fn test_invalid_transition_ready_completed() {
        let state = SessionState::Ready;
        let result = state.transition(&SessionEvent::Completed, 3);
        assert!(result.is_err());
    }
}
