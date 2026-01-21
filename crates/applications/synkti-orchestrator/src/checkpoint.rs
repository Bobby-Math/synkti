//! Docker checkpoint management
//!
//! ⚠️ **DEPRECATED: Docker checkpoint does NOT work with GPU/TPU containers**
//!
//! This module was written for a warm migration approach using `docker checkpoint create`.
//! However, CRIU (Checkpoint/Restore In Userspace) cannot snapshot GPU/TPU hardware state:
//! - GPU VRAM and CUDA contexts cannot be serialized
//! - TPU HBM and matrix units cannot be serialized
//! - Docker checkpoint will fail or hang on containers actively using accelerators
//!
//! **The correct approach for GPU/TPU is stateless failover:**
//! 1. Drain: Stop accepting new requests
//! 2. Wait: Let in-flight requests complete
//! 3. Stop: Gracefully terminate container
//! 4. Respawn: Start fresh instance, load model from disk/S3
//!
//! This module is kept for reference but should NOT be used in production.
//! Use the drain/failover pattern in `failover.rs` instead.
//!
//! Manages Docker container checkpoints for state migration.

#![allow(deprecated)]

use crate::error::{OrchestratorError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::process::Command as AsyncCommand;
use std::process::Command as SyncCommand;
use tracing::{debug, info, warn};

/// Checkpoint metadata
#[deprecated(since = "0.2.0", note = "Docker checkpoint does not work with GPU/TPU. Use stateless failover instead.")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointMetadata {
    /// Container ID
    pub container_id: String,

    /// Container name
    pub container_name: String,

    /// Checkpoint ID
    pub checkpoint_id: String,

    /// Timestamp when checkpoint was created
    pub created_at: chrono::DateTime<chrono::Utc>,

    /// Estimated checkpoint size in bytes
    pub size_bytes: u64,

    /// vLLM model being served
    pub model: Option<String>,

    /// Number of active requests at checkpoint time
    pub active_requests: u32,
}

/// Docker checkpoint manager
#[deprecated(since = "0.2.0", note = "Docker checkpoint does not work with GPU/TPU. Use stateless failover instead.")]
pub struct DockerCheckpoint {
    /// Docker socket path
    socket_path: String,
}

impl DockerCheckpoint {
    /// Create a new Docker checkpoint manager
    pub fn new() -> Self {
        Self {
            socket_path: "/var/run/docker.sock".to_string(),
        }
    }

    /// Create a checkpoint for a running container
    pub async fn create_checkpoint(
        &self,
        container_id: &str,
        checkpoint_id: &str,
        exit: bool,
    ) -> Result<CheckpointMetadata> {
        info!(
            "Creating checkpoint '{}' for container '{}'",
            checkpoint_id, container_id
        );

        let mut args = vec![
            "checkpoint".to_string(),
            "create".to_string(),
            "--checkpoint-dir=/tmp/checkpoints".to_string(),
        ];

        if !exit {
            args.push("--leave=true".to_string());
        }

        args.push(container_id.to_string());
        args.push(checkpoint_id.to_string());

        let output = AsyncCommand::new("docker")
            .args(&args)
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to create checkpoint: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Docker(format!(
                "Checkpoint create failed: {}",
                stderr
            )));
        }

        // Get checkpoint size
        let size_bytes = self.get_checkpoint_size(checkpoint_id).await?;

        // Get container info for metadata
        let container_name = self
            .get_container_name(container_id)
            .await
            .unwrap_or_else(|_| container_id.to_string());

        let metadata = CheckpointMetadata {
            container_id: container_id.to_string(),
            container_name,
            checkpoint_id: checkpoint_id.to_string(),
            created_at: chrono::Utc::now(),
            size_bytes,
            model: None,
            active_requests: 0,
        };

        info!("Checkpoint created successfully: {} bytes", size_bytes);

        Ok(metadata)
    }

    /// Restore a container from a checkpoint
    pub async fn restore_checkpoint(
        &self,
        container_id: &str,
        image: &str,
        checkpoint_id: &str,
        checkpoint_dir: &str,
    ) -> Result<()> {
        info!(
            "Restoring container '{}' from checkpoint '{}'",
            container_id, checkpoint_id
        );

        let output = AsyncCommand::new("docker")
            .args([
                "start",
                "--checkpoint",
                checkpoint_id,
                "--checkpoint-dir",
                checkpoint_dir,
                container_id,
            ])
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to restore checkpoint: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Docker(format!(
                "Checkpoint restore failed: {}",
                stderr
            )));
        }

        info!("Checkpoint restored successfully");

        Ok(())
    }

    /// Delete a checkpoint
    pub async fn delete_checkpoint(&self, checkpoint_id: &str) -> Result<()> {
        debug!("Deleting checkpoint '{}'", checkpoint_id);

        let output = AsyncCommand::new("docker")
            .args(["checkpoint", "rm", checkpoint_id])
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to delete checkpoint: {}", e)))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("Failed to delete checkpoint: {}", stderr);
        }

        Ok(())
    }

    /// Get checkpoint directory size
    async fn get_checkpoint_size(&self, checkpoint_id: &str) -> Result<u64> {
        let checkpoint_path = format!("/tmp/checkpoints/{}", checkpoint_id);

        let output = SyncCommand::new("du")
            .arg("-sb")
            .arg(&checkpoint_path)
            .output()
            .map_err(|e| OrchestratorError::Io(e))?;

        if !output.status.success() {
            return Ok(0); // Unknown size
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout
            .split_whitespace()
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| OrchestratorError::Checkpoint("Cannot parse checkpoint size".to_string()))
    }

    /// Get container name
    async fn get_container_name(&self, container_id: &str) -> Result<String> {
        let output = SyncCommand::new("docker")
            .args(["inspect", "-f", "{{.Name}}", container_id])
            .output()
            .map_err(|e| OrchestratorError::Docker(format!("Failed to inspect container: {}", e)))?;

        if !output.status.success() {
            return Ok(container_id.to_string());
        }

        let name = String::from_utf8_lossy(&output.stdout);
        Ok(name.trim().trim_start_matches('/').to_string())
    }

    /// List all checkpoints for a container
    pub async fn list_checkpoints(&self, container_id: &str) -> Result<Vec<String>> {
        let output = AsyncCommand::new("docker")
            .args(["checkpoint", "ls", container_id])
            .output()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to list checkpoints: {}", e)))?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        // Parse output to extract checkpoint IDs
        let stdout = String::from_utf8_lossy(&output.stdout);
        let checkpoints: Vec<String> = stdout
            .lines()
            .skip(1) // Skip header
            .filter_map(|line| line.split_whitespace().next())
            .map(|s| s.to_string())
            .collect();

        Ok(checkpoints)
    }

    /// Export checkpoint to a tar archive
    pub async fn export_checkpoint(
        &self,
        checkpoint_id: &str,
        dest_path: &Path,
    ) -> Result<u64> {
        info!(
            "Exporting checkpoint '{}' to {:?}",
            checkpoint_id, dest_path
        );

        let checkpoint_dir = format!("/tmp/checkpoints/{}", checkpoint_id);

        let output = AsyncCommand::new("tar")
            .args([
                "czf",
                &dest_path.to_string_lossy(),
                "-C",
                "/tmp/checkpoints",
                checkpoint_id,
            ])
            .output()
            .await
            .map_err(|e| OrchestratorError::Io(e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Checkpoint(format!(
                "Failed to export checkpoint: {}",
                stderr
            )));
        }

        // Get file size
        let metadata = tokio::fs::metadata(dest_path).await?;
        Ok(metadata.len())
    }

    /// Import checkpoint from a tar archive
    pub async fn import_checkpoint(
        &self,
        archive_path: &Path,
        checkpoint_id: &str,
    ) -> Result<()> {
        info!(
            "Importing checkpoint '{}' from {:?}",
            checkpoint_id, archive_path
        );

        let output = AsyncCommand::new("tar")
            .args([
                "xzf",
                &archive_path.to_string_lossy(),
                "-C",
                "/tmp/checkpoints",
            ])
            .output()
            .await
            .map_err(|e| OrchestratorError::Io(e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(OrchestratorError::Checkpoint(format!(
                "Failed to import checkpoint: {}",
                stderr
            )));
        }

        Ok(())
    }
}

impl Default for DockerCheckpoint {
    fn default() -> Self {
        Self::new()
    }
}

/// Checkpoint manager with S3 integration
#[deprecated(since = "0.2.0", note = "Docker checkpoint does not work with GPU/TPU. Use stateless failover instead.")]
pub struct CheckpointManager {
    docker: DockerCheckpoint,
}

impl CheckpointManager {
    /// Create a new checkpoint manager
    pub fn new() -> Self {
        Self {
            docker: DockerCheckpoint::new(),
        }
    }

    /// Create checkpoint and prepare for migration
    pub async fn prepare_migration(
        &self,
        container_id: &str,
        checkpoint_id: &str,
    ) -> Result<CheckpointMetadata> {
        // Create the checkpoint
        let metadata = self.docker.create_checkpoint(container_id, checkpoint_id, true).await?;

        // Export to tar file for S3 upload
        let archive_path = format!("/tmp/{}.tar.gz", checkpoint_id);
        self.docker
            .export_checkpoint(checkpoint_id, Path::new(&archive_path))
            .await?;

        Ok(metadata)
    }

    /// Restore from migration checkpoint
    pub async fn restore_from_migration(
        &self,
        container_id: &str,
        image: &str,
        checkpoint_id: &str,
        archive_path: &Path,
    ) -> Result<()> {
        // Import checkpoint archive
        self.docker
            .import_checkpoint(archive_path, checkpoint_id)
            .await?;

        // Restore container
        self.docker
            .restore_checkpoint(container_id, image, checkpoint_id, "/tmp/checkpoints")
            .await?;

        Ok(())
    }

    /// Cleanup checkpoint files
    pub async fn cleanup(&self, checkpoint_id: &str) -> Result<()> {
        let _ = self.docker.delete_checkpoint(checkpoint_id).await;

        let archive_path = format!("/tmp/{}.tar.gz", checkpoint_id);
        let _ = tokio::fs::remove_file(archive_path).await;

        Ok(())
    }
}

impl Default for CheckpointManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checkpoint_metadata_serialization() {
        let metadata = CheckpointMetadata {
            container_id: "abc123".to_string(),
            container_name: "vllm-server".to_string(),
            checkpoint_id: "chk-001".to_string(),
            created_at: chrono::Utc::now(),
            size_bytes: 2_147_483_648, // 2GB
            model: Some("meta-llama/Llama-2-7b-hf".to_string()),
            active_requests: 5,
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let _parsed: CheckpointMetadata = serde_json::from_str(&json).unwrap();
    }
}
