//! Error types for Synkti

use thiserror::Error;

/// Core error type for Synkti operations
#[derive(Error, Debug)]
pub enum SynktiError {
    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Instance not found: {0}")]
    InstanceNotFound(String),

    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Network error: {0}")]
    Network(String),

    #[error("Timeout: {0}")]
    Timeout(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Internal error: {0}")]
    Internal(String),
}
