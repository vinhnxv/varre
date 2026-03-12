mod backend;
mod cli;
mod config;
mod error;
mod orchestrator;
mod queue;
mod session;
mod tmux;
mod tui;

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use crate::backend::CliBackend;
use crate::cli::{Cli, Commands, ConfigCommands, QueueCommands};
use crate::config::Config;
use crate::orchestrator::Orchestrator;
use crate::queue::runner::QueueRunner;
use crate::queue::{Job, PromptQueue};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config(&cli)?;

    setup_tracing(&config, cli.verbose)?;

    let cancel_token = CancellationToken::new();
    install_signal_handler(cancel_token.clone());

    match cli.command {
        Commands::New { name, mode, dir } => {
            let backend = Arc::new(CliBackend::new());
            let mut orch = Orchestrator::new(config, backend, cancel_token)?;
            match mode.as_str() {
                "headless" => {
                    let id = orch.create_session(&name, dir).await?;
                    println!("created headless session '{name}' (id: {id})");
                }
                "interactive" | "tmux" => {
                    let id = orch.create_interactive_session(&name, dir).await?;
                    println!("created interactive session '{name}' (id: {id})");
                }
                other => {
                    anyhow::bail!(
                        "unknown mode '{other}', expected 'headless' or 'interactive'"
                    );
                }
            }
        }

        Commands::Send {
            name,
            prompt,
            stream: _,
        } => {
            let backend = Arc::new(CliBackend::new());
            let mut orch = Orchestrator::new(config, backend, cancel_token)?;
            let response = orch.send_prompt(&name, &prompt).await?;
            println!("{}", response.result);
            if let Some(cost) = response.cost_usd {
                eprintln!("cost: ${cost:.4}");
            }
        }

        Commands::List => {
            let backend = Arc::new(CliBackend::new());
            let orch = Orchestrator::new(config, backend, cancel_token)?;
            let sessions = orch.list_sessions().await;
            if sessions.is_empty() {
                println!("no active sessions");
            } else {
                println!(
                    "{:<20} {:<12} {:<36} {}",
                    "NAME", "STATE", "ID", "WORKING DIR"
                );
                for info in sessions {
                    println!(
                        "{:<20} {:<12} {:<36} {}",
                        info.name,
                        format_state(&info.state),
                        info.id,
                        info.working_dir.display(),
                    );
                }
            }
        }

        Commands::Kill { name, force: _ } => {
            let backend = Arc::new(CliBackend::new());
            let mut orch = Orchestrator::new(config, backend, cancel_token)?;
            orch.kill_session(&name).await?;
            println!("killed session '{name}'");
        }

        Commands::Queue { action } => {
            let data_dir = Config::data_dir();
            std::fs::create_dir_all(&data_dir)?;
            let queue_path = data_dir.join("queue.json");

            match action {
                QueueCommands::Add {
                    prompts,
                    session,
                    force,
                } => {
                    let mut queue = PromptQueue::load(&queue_path)?;
                    let session_id =
                        session.map(crate::session::SessionId::from_string);
                    for prompt in prompts {
                        let job = Job::new(prompt.clone(), session_id.clone());
                        queue.add(job, force)?;
                        println!("queued: {prompt}");
                    }
                }

                QueueCommands::Run { concurrency: _ } => {
                    let queue = PromptQueue::load(&queue_path)?;
                    let backend = Arc::new(CliBackend::new());
                    let queue_arc = Arc::new(tokio::sync::Mutex::new(queue));
                    let runner = QueueRunner::new(queue_arc.clone(), backend);
                    let status = runner.run_all(cancel_token).await?;
                    println!(
                        "queue finished — completed: {}, failed: {}, pending: {}",
                        status.completed, status.failed, status.pending
                    );
                }

                QueueCommands::Status => {
                    let queue = PromptQueue::load(&queue_path)?;
                    let status = queue.status();
                    println!("pending:   {}", status.pending);
                    println!("running:   {}", status.running);
                    println!("completed: {}", status.completed);
                    println!("failed:    {}", status.failed);
                }

                QueueCommands::Retry { job_id } => {
                    let mut queue = PromptQueue::load(&queue_path)?;
                    let uuid =
                        Uuid::parse_str(&job_id).context("invalid job ID format")?;
                    queue.retry(uuid)?;
                    println!("retried job {job_id}");
                }

                QueueCommands::Clear => {
                    let mut queue = PromptQueue::load(&queue_path)?;
                    queue.clear_finished();
                    queue.save()?;
                    println!("cleared completed and failed jobs");
                }
            }
        }

        Commands::Config { action } => match action {
            Some(ConfigCommands::Show) | None => {
                let toml_str = toml::to_string_pretty(&config)
                    .context("failed to serialize config")?;
                println!("{toml_str}");
            }
            Some(ConfigCommands::Init) => {
                let path = Config::config_path();
                if path.exists() {
                    println!("config already exists at {}", path.display());
                } else {
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let default_config = Config::default();
                    let toml_str = toml::to_string_pretty(&default_config)
                        .context("failed to serialize default config")?;
                    std::fs::write(&path, toml_str)?;
                    println!("created config at {}", path.display());
                }
            }
            Some(ConfigCommands::Path) => {
                println!("{}", Config::config_path().display());
            }
        },

        Commands::Capture { name, lines } => {
            let backend = Arc::new(CliBackend::new());
            let orch = Orchestrator::new(config, backend, cancel_token)?;
            let output = orch.capture_output(&name, lines.unwrap_or(50)).await?;
            println!("{output}");
        }

        Commands::Tui => {
            let backend = Arc::new(CliBackend::new());
            let mut orch = Orchestrator::new(config.clone(), backend, cancel_token.clone())?;
            tui::run(config, &mut orch, cancel_token).await?;
        }
    }

    Ok(())
}

/// Load configuration from the CLI-specified path or the default location.
fn load_config(cli: &Cli) -> Result<Config> {
    if let Some(ref path) = cli.config {
        let content =
            std::fs::read_to_string(path).context("failed to read config file")?;
        toml::from_str(&content).context("failed to parse config file")
    } else {
        Config::load()
    }
}

/// Initialize tracing with file-based output in the data directory.
fn setup_tracing(_config: &Config, verbose: bool) -> Result<()> {
    let data_dir = Config::data_dir();
    std::fs::create_dir_all(&data_dir)?;
    let log_path = data_dir.join("varre.log");

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("failed to open log file")?;

    let filter = if verbose {
        EnvFilter::new("debug")
    } else {
        EnvFilter::new("info")
    };

    tracing_subscriber::fmt()
        .with_writer(file)
        .with_env_filter(filter)
        .with_ansi(false)
        .init();

    Ok(())
}

/// Install a signal handler that cancels the token on SIGINT or SIGTERM.
fn install_signal_handler(cancel_token: CancellationToken) {
    tokio::spawn(async move {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigint =
                signal(SignalKind::interrupt()).expect("failed to register SIGINT handler");
            let mut sigterm =
                signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");

            tokio::select! {
                _ = sigint.recv() => {
                    tracing::info!("received SIGINT, shutting down");
                }
                _ = sigterm.recv() => {
                    tracing::info!("received SIGTERM, shutting down");
                }
            }
        }

        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c()
                .await
                .expect("failed to register ctrl-c handler");
            tracing::info!("received ctrl-c, shutting down");
        }

        cancel_token.cancel();
    });
}

/// Format a session state for table display.
fn format_state(state: &crate::session::state::SessionState) -> String {
    match state {
        crate::session::state::SessionState::Creating => "creating".into(),
        crate::session::state::SessionState::Ready => "ready".into(),
        crate::session::state::SessionState::Busy { .. } => "busy".into(),
        crate::session::state::SessionState::WaitingInput => "waiting".into(),
        crate::session::state::SessionState::Error { retry_count, .. } => {
            format!("error(r:{retry_count})")
        }
        crate::session::state::SessionState::Dead => "dead".into(),
    }
}
