use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "varre")]
#[command(about = "Multi-session Claude Code orchestrator")]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Config file path
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Enable verbose logging
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new Claude Code session
    New {
        /// Session name
        name: String,

        /// Session mode
        #[arg(long, default_value = "headless")]
        mode: String,

        /// Working directory for the session
        #[arg(long)]
        dir: Option<PathBuf>,
    },

    /// Send a prompt to a session
    Send {
        /// Session name
        name: String,

        /// Prompt text
        prompt: String,

        /// Use streaming output
        #[arg(long)]
        stream: bool,
    },

    /// Capture output from a session
    Capture {
        /// Session name
        name: String,

        /// Number of lines to capture
        #[arg(long)]
        lines: Option<i32>,
    },

    /// List active sessions
    List,

    /// Kill a session
    Kill {
        /// Session name
        name: String,

        /// Force kill without confirmation
        #[arg(short, long)]
        force: bool,
    },

    /// Queue management
    Queue {
        #[command(subcommand)]
        action: QueueCommands,
    },

    /// Launch interactive TUI
    Tui,

    /// Show or initialize configuration
    Config {
        #[command(subcommand)]
        action: Option<ConfigCommands>,
    },
}

#[derive(Subcommand)]
pub enum QueueCommands {
    /// Add prompts to the queue
    Add {
        /// Prompts to queue
        prompts: Vec<String>,

        /// Target session name
        #[arg(long)]
        session: Option<String>,

        /// Force add even if duplicate detected
        #[arg(long)]
        force: bool,
    },

    /// Run the queue
    Run {
        /// Maximum concurrent sessions
        #[arg(long)]
        concurrency: Option<usize>,
    },

    /// Show queue status
    Status,

    /// Retry a failed job
    Retry {
        /// Job ID to retry
        job_id: String,
    },

    /// Clear completed/failed jobs
    Clear,
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Show current configuration
    Show,

    /// Initialize default config file
    Init,

    /// Show config file path
    Path,
}
