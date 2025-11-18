//! # Tessera ML Executor
//!
//! ML inference executor implementation for Tessera.
//!
//! This executor uses Axon (ML inference server) and Synapse (CUDA FFI)
//! to provide high-performance ML inference as a Tessera workload.

#![warn(missing_docs)]

use axon::server::InferenceServer;

/// ML inference task executor
pub struct MlInferenceExecutor {
    server: Option<InferenceServer>,
}

impl MlInferenceExecutor {
    /// Create a new ML inference executor
    pub fn new() -> Self {
        MlInferenceExecutor { server: None }
    }
}

impl Default for MlInferenceExecutor {
    fn default() -> Self {
        Self::new()
    }
}

// Note: TaskExecutor trait implementation will be added once tessera-core defines it

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_executor_creation() {
        let _executor = MlInferenceExecutor::new();
    }
}
