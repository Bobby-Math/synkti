//! Synkti CLI - Thin client for fleet management
//!
//! Provides commands for:
//! - Login to fleet API
//! - Deploy infrastructure
//! - View fleet status
//! - Stream logs
//! - Destroy infrastructure
//!
//! Binary: synkti

use clap::{Parser, Subcommand};
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod commands;

/// Synkti CLI - Fleet management interface
#[derive(Parser)]
#[command(name = "synkti")]
#[command(about = "CLI for managing Synkti spot instance fleets", long_about = None)]
struct Cli {
    /// Fleet API endpoint
    #[arg(long, env = "SYNKTI_API", default_value = "https://api.synkti.dev")]
    api: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with the Synkti API
    Login,

    /// Deploy infrastructure and start fleet
    Apply {
        /// Project name
        project: String,

        /// Config file path
        #[arg(short, long, default_value = "synkti.yaml")]
        config: String,
    },

    /// Show fleet status
    Status {
        /// Project name
        project: Option<String>,
    },

    /// Stream logs from fleet
    Logs {
        /// Project name
        project: String,

        /// Follow logs
        #[arg(short, long)]
        follow: bool,
    },

    /// Destroy infrastructure
    Destroy {
        /// Project name
        project: String,

        /// Skip confirmation
        #[arg(long)]
        force: bool,
    },

    /// Local development mode (single node, no fleet)
    Dev {
        /// Model to load
        #[arg(long)]
        model: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synkti=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Login => {
            info!("Login not yet implemented");
            // TODO: Implement OAuth/API key auth
        }
        Commands::Apply { project, config } => {
            info!("Deploying project '{}' with config '{}'", project, config);
            // TODO: Call fleet API to deploy
        }
        Commands::Status { project } => {
            info!("Status for project: {:?}", project);
            // TODO: Call fleet API to get status
        }
        Commands::Logs { project, follow } => {
            info!("Logs for project '{}' (follow: {})", project, follow);
            // TODO: Stream logs from fleet API
        }
        Commands::Destroy { project, force } => {
            info!("Destroying project '{}' (force: {})", project, force);
            // TODO: Call fleet API to destroy
        }
        Commands::Dev { model } => {
            info!("Starting local dev mode with model '{}'", model);
            // TODO: Run single-node vLLM locally
        }
    }

    Ok(())
}
