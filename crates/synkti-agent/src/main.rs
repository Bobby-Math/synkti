//! Synkti Agent - Node binary for spot instances
//!
//! Runs on each spot instance and handles:
//! - Spot interruption monitoring (monitor.rs)
//! - Container lifecycle (vllm.rs)
//! - Graceful shutdown (drain.rs)
//!
//! Binary: synkti-agent

use clap::Parser;
use futures::StreamExt;
use std::time::Duration;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod monitor;
mod vllm;
mod drain;

/// Synkti Agent - Node binary for spot instances
#[derive(Parser)]
#[command(name = "synkti-agent")]
#[command(about = "Spot instance agent for monitoring and container management", long_about = None)]
struct Cli {
    /// Fleet API endpoint
    #[arg(long, env = "SYNKTI_FLEET_API")]
    fleet_api: Option<String>,

    /// Spot monitoring interval (seconds)
    #[arg(long, default_value = "5")]
    monitor_interval: u64,

    /// Health check port
    #[arg(long, default_value = "8080")]
    port: u16,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "synkti_agent=info,info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    info!("========================================");
    info!("Synkti Agent starting");
    info!("Version: {}", env!("CARGO_PKG_VERSION"));
    info!("Monitor interval: {}s", cli.monitor_interval);
    info!("========================================");

    // Start spot monitoring
    let monitor = monitor::SpotMonitor::with_interval(Duration::from_secs(cli.monitor_interval));
    let mut stream = monitor.monitor_stream();

    info!("Spot monitoring active");

    while let Some(notice) = stream.next().await {
        match notice.action {
            monitor::SpotAction::Terminate => {
                warn!(
                    "SPOT TERMINATION NOTICE: {} seconds until termination",
                    notice.seconds_until_action
                );
                // TODO: Notify fleet API, initiate drain
            }
            _ => {}
        }
    }

    Ok(())
}
