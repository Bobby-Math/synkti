//! # Synkti AWS Orchestrator
//!
//! Spot instance orchestration for ML inference workloads.
//!
//! ## Architecture
//!
//! ```text
//! Orchestrator (Rust)          vLLM (Docker/Python)
//! ├── Spot monitoring    ←────  Worker process
//! ├── Failover manager        (swappable)
//! ├── Drain/Assignment         │
//! └── Instance lifecycle  ─────┘
//! ```
//!
//! The orchestrator handles all critical logic in Rust:
//! - Spot interruption detection via EC2 metadata polling
//! - Stateless failover with graceful draining
//! - Node assignment strategies (FIFO, LeastLoaded, Warm+LeastLoaded)
//! - Instance lifecycle management
//!
//! vLLM is a worker process — swappable for TGI, TensorRT-LLM, etc.
//!
//! ## Stateless Failover
//!
//! GPU/TPU workloads cannot use Docker checkpointing (CRIU doesn't support accelerators).
//! Instead, we use stateless failover:
//!
//! 1. **Drain**: Stop new requests, wait for in-flight to complete (115s max)
//! 2. **Select**: Choose replacement instance (FIFO, LeastLoaded, or Warm+LeastLoaded)
//! 3. **Spawn**: Start fresh container on replacement
//! 4. **Route**: Health check and update load balancer
//!
//! See [`failover`] and [`drain`] modules for details.

#![warn(missing_docs)]
#![warn(clippy::all)]
#![allow(deprecated)] // Allow deprecated items in this crate for backward compatibility

// Active modules (stateless failover)
pub mod assign;
pub mod discovery;
pub mod drain;
pub mod elb;
pub mod error;
pub mod failover;
pub mod infra;
pub mod instance;
pub mod migration;
pub mod monitor;
pub mod remote;
pub mod vllm;

// Deprecated modules (checkpoint-based migration - doesn't work with GPU/TPU)
#[deprecated(since = "0.2.0", note = "Docker checkpoint does not work with GPU/TPU. Use failover module instead.")]
pub mod checkpoint;
#[deprecated(since = "0.2.0", note = "Depends on checkpoint module which does not work with GPU/TPU. Use failover module instead.")]
pub mod s3_store;

// ============================================================================
// Public exports - New stateless failover API
// ============================================================================

// Failover orchestration
pub use failover::{FailoverConfig, FailoverManager, FailoverPhaseTimes, FailoverResult};

// Drain management
pub use drain::{DrainManager, DrainResult, DrainStatus, ElbConfig, DEFAULT_DRAIN_TIMEOUT_SECS};

// Assignment strategies
pub use assign::{
    AssignmentCandidate, AssignmentResult, AssignmentStrategy, NodeAssigner, Workload,
};

// ============================================================================
// Public exports - Core infrastructure
// ============================================================================

// Error handling
pub use error::{OrchestratorError, Result};

// Instance management
pub use instance::{
    create_ec2_client, get_gpu_ami, get_standard_ami, is_gpu_instance_type, list_workers,
    terminate_worker, Ec2Instance, InstanceSpec, InstanceState, DEFAULT_REGION,
};

// Spot monitoring
pub use monitor::{SpotInterruptionNotice, SpotMonitor, GRACE_PERIOD_SECONDS};

// vLLM container management
pub use vllm::{VllmClient, VllmConfig, VllmContainer};

// Remote execution via SSM
pub use remote::{CommandResult, CommandStatus, SsmExecutor};

// Load balancer integration
pub use elb::LoadBalancerManager;

// Peer discovery (P2P architecture)
pub use discovery::{
    tag_self_as_worker, untag_self_as_worker, DiscoveryConfig, PeerDiscovery,
    DEFAULT_CLUSTER_TAG_KEY, DEFAULT_ROLE_TAG_KEY, ROLE_WORKER,
};

// Migration planning (still useful for cost calculations)
pub use migration::{MigrationPlanner, MigrationPlan, MigrationTarget, MigrationTask};

// Infrastructure management
pub use infra::{
    cleanup_stale_owner, create_owner_marker, has_stale_owner, is_owner, remove_owner_marker,
    InfraStatus, TerraformOutputs, TerraformRunner,
};

// ============================================================================
// Deprecated exports - Checkpoint-based (for backward compatibility only)
// ============================================================================

#[allow(deprecated)]
pub use checkpoint::{CheckpointManager, CheckpointMetadata, DockerCheckpoint};
#[allow(deprecated)]
pub use s3_store::{S3CheckpointMetadata, S3CheckpointStore};
