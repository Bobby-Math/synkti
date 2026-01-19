//! # Synkti AWS Orchestrator
//!
//! Spot instance orchestration for ML inference workloads.
//!
//! ## Architecture
//!
//! ```text
//! Orchestrator (Rust)          vLLM (Docker/Python)
//! ├── Spot monitoring    ←────  Worker process
//! ├── KM scheduler            (swappable)
//! ├── Checkpoint/S3            │
//! └── Instance lifecycle  ─────┘
//! ```
//!
//! The orchestrator handles all critical logic in Rust:
//! - Spot interruption detection via EC2 metadata polling
//! - Kuhn-Munkres optimal migration scheduling
//! - Docker checkpoint management
//! - S3 checkpoint storage
//! - Instance lifecycle management
//!
//! vLLM is a worker process — swappable for TGI, TensorRT-LLM, etc.

#![warn(missing_docs)]
#![warn(clippy::all)]

pub mod checkpoint;
pub mod error;
pub mod infra;
pub mod instance;
pub mod migration;
pub mod monitor;
pub mod s3_store;
pub mod vllm;

// Public exports
pub use checkpoint::{CheckpointManager, DockerCheckpoint};
pub use error::{OrchestratorError, Result};
pub use infra::{create_owner_marker, cleanup_stale_owner, has_stale_owner, is_owner, remove_owner_marker, InfraStatus, TerraformOutputs, TerraformRunner};
pub use instance::{create_ec2_client, DEFAULT_REGION, Ec2Instance, InstanceSpec, InstanceState};
pub use migration::{MigrationPlanner, MigrationTarget};
pub use monitor::{SpotInterruptionNotice, SpotMonitor};
pub use s3_store::S3CheckpointStore;
pub use vllm::{VllmConfig, VllmContainer};
