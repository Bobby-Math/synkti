//! Error types for the orchestrator

use std::time::Duration;
use thiserror::Error;

/// Orchestrator result type
pub type Result<T> = std::result::Result<T, OrchestratorError>;

/// Errors that can occur in the orchestrator
#[derive(Error, Debug)]
pub enum OrchestratorError {
    /// AWS SDK error
    #[error("AWS error: {0}")]
    Aws(#[from] aws_sdk_ec2::Error),

    /// S3 error
    #[error("S3 error: {0}")]
    S3(#[from] aws_sdk_s3::Error),

    /// HTTP client error
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Docker API error
    #[error("Docker API error: {0}")]
    Docker(String),

    /// Checkpoint error
    #[error("Checkpoint error: {0}")]
    Checkpoint(String),

    /// Migration error
    #[error("Migration error: {0}")]
    Migration(String),

    /// Timeout
    #[error("Operation timed out after {0:?}")]
    Timeout(Duration),

    /// Instance not found
    #[error("Instance {0} not found")]
    InstanceNotFound(String),

    /// No available instances for migration
    #[error("No available instances for migration")]
    NoAvailableInstances,

    /// Insufficient memory on target instance
    #[error("Insufficient memory: need {need}MB, have {have}MB")]
    InsufficientMemory { need: f64, have: f64 },

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Generic AWS service error (for SSM, etc.)
    #[error("AWS service error: {0}")]
    AwsService(String),
}

impl OrchestratorError {
    /// Create a Docker API error
    pub fn docker(msg: impl Into<String>) -> Self {
        Self::Docker(msg.into())
    }

    /// Create a checkpoint error
    pub fn checkpoint(msg: impl Into<String>) -> Self {
        Self::Checkpoint(msg.into())
    }

    /// Create a migration error
    pub fn migration(msg: impl Into<String>) -> Self {
        Self::Migration(msg.into())
    }

    /// Create a configuration error
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Convert from EC2 SDK error
    pub fn from_ec2<E>(err: E) -> Self
    where
        aws_sdk_ec2::Error: From<E>,
    {
        Self::Aws(aws_sdk_ec2::Error::from(err))
    }

    /// Convert from generic AWS SDK error
    pub fn from_aws<E>(err: E) -> Self
    where
        E: std::fmt::Display,
    {
        Self::AwsService(err.to_string())
    }
}
