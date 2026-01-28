//! Error types for synkti-agent

use std::time::Duration;
use thiserror::Error;

/// Result type for agent operations
pub type Result<T> = std::result::Result<T, AgentError>;

/// Error type for agent operations (alias for compatibility)
pub type OrchestratorError = AgentError;

/// Error type for agent operations
#[derive(Error, Debug)]
pub enum AgentError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Docker error: {0}")]
    Docker(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("Spot interruption: {0}")]
    SpotInterruption(String),

    #[error("Timeout after {0:?}")]
    Timeout(Duration),

    #[error("Container not found: {0}")]
    ContainerNotFound(String),

    #[error("Health check failed: {0}")]
    HealthCheck(String),

    #[error("{0}")]
    Other(String),
}
