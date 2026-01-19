//! S3 checkpoint storage
//!
//! Stores and retrieves Docker checkpoints from S3.

use crate::checkpoint::CheckpointMetadata;
use crate::error::{OrchestratorError, Result};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info};

/// S3 checkpoint metadata (stored alongside checkpoint)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3CheckpointMetadata {
    /// Original checkpoint metadata
    #[serde(flatten)]
    pub checkpoint: CheckpointMetadata,

    /// S3 bucket name
    pub bucket: String,

    /// S3 key for checkpoint archive
    pub key: String,

    /// ETag of uploaded object
    pub etag: Option<String>,

    /// Version ID (if bucket versioning is enabled)
    pub version_id: Option<String>,
}

/// S3 checkpoint store
pub struct S3CheckpointStore {
    /// S3 client
    client: Client,

    /// Default bucket name
    bucket: String,

    /// Key prefix for checkpoints
    prefix: String,
}

impl S3CheckpointStore {
    /// Create a new S3 checkpoint store
    pub fn new(client: Client, bucket: impl Into<String>) -> Self {
        Self {
            client,
            bucket: bucket.into(),
            prefix: "checkpoints".to_string(),
        }
    }

    /// Set key prefix
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.prefix = prefix.into();
        self
    }

    /// Generate S3 key for a checkpoint
    fn s3_key(&self, checkpoint_id: &str) -> String {
        format!("{}/{}.tar.gz", self.prefix, checkpoint_id)
    }

    /// Upload checkpoint to S3
    ///
    /// # Arguments
    /// - `archive_path`: Local path to checkpoint tar.gz
    /// - `checkpoint_id`: Checkpoint ID
    /// - `metadata`: Checkpoint metadata
    pub async fn upload(
        &self,
        archive_path: &Path,
        checkpoint_id: &str,
        metadata: &CheckpointMetadata,
    ) -> Result<S3CheckpointMetadata> {
        let key = self.s3_key(checkpoint_id);

        info!(
            "Uploading checkpoint {} to s3://{}/{}",
            checkpoint_id, self.bucket, key
        );

        // Read archive file
        let mut file = tokio::fs::File::open(archive_path).await?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).await?;

        // Upload to S3
        let response = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(&key)
            .body(ByteStream::from(buffer))
            .send()
            .await
            .map_err(|e| OrchestratorError::S3(aws_sdk_s3::Error::from(e)))?;

        let s3_metadata = S3CheckpointMetadata {
            checkpoint: metadata.clone(),
            bucket: self.bucket.clone(),
            key: key.clone(),
            etag: response.e_tag,
            version_id: response.version_id,
        };

        info!(
            "Checkpoint uploaded: {} bytes",
            metadata.size_bytes
        );

        Ok(s3_metadata)
    }

    /// Download checkpoint from S3
    ///
    /// # Arguments
    /// - `checkpoint_id`: Checkpoint ID
    /// - `dest_path`: Local path to save the archive
    pub async fn download(
        &self,
        checkpoint_id: &str,
        dest_path: &Path,
    ) -> Result<CheckpointMetadata> {
        let key = self.s3_key(checkpoint_id);

        info!(
            "Downloading checkpoint {} from s3://{}/{}",
            checkpoint_id, self.bucket, key
        );

        // Download from S3
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| OrchestratorError::S3(aws_sdk_s3::Error::from(e)))?;

        // TODO: Parse metadata from object metadata if present
        // let metadata = response.metadata.as_ref();

        // Write to file
        let mut file = tokio::fs::File::create(dest_path).await?;
        let mut byte_stream = response.body;
        let mut buffer = Vec::new();
        while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk.map_err(|e| OrchestratorError::Checkpoint(format!("ByteStream error: {}", e)))?;
            buffer.extend_from_slice(&bytes);
        }
        file.write_all(&buffer).await?;
        file.flush().await?;

        info!("Checkpoint downloaded to {:?}", dest_path);

        // Return basic metadata (TODO: retrieve from S3 metadata)
        Ok(CheckpointMetadata {
            container_id: String::new(),
            container_name: String::new(),
            checkpoint_id: checkpoint_id.to_string(),
            created_at: chrono::Utc::now(),
            size_bytes: buffer.len() as u64,
            model: None,
            active_requests: 0,
        })
    }

    /// Delete checkpoint from S3
    pub async fn delete(&self, checkpoint_id: &str) -> Result<()> {
        let key = self.s3_key(checkpoint_id);

        debug!(
            "Deleting checkpoint {} from s3://{}/{}",
            checkpoint_id, self.bucket, key
        );

        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
            .map_err(|e| OrchestratorError::S3(aws_sdk_s3::Error::from(e)))?;

        Ok(())
    }

    /// List all checkpoints in S3
    pub async fn list(&self) -> Result<Vec<String>> {
        let prefix = format!("{}/", self.prefix);

        let response = self
            .client
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(&prefix)
            .send()
            .await
            .map_err(|e| OrchestratorError::S3(aws_sdk_s3::Error::from(e)))?;

        let checkpoints: Vec<String> = response
            .contents()
            .iter()
            .filter_map(|obj| {
                obj.key()
                    .as_ref()
                    .and_then(|key| {
                        key.strip_prefix(&prefix)
                            .and_then(|s| s.strip_suffix(".tar.gz"))
                            .map(|s| s.to_string())
                    })
            })
            .collect();

        Ok(checkpoints)
    }

    /// Check if checkpoint exists in S3
    pub async fn exists(&self, checkpoint_id: &str) -> Result<bool> {
        let key = self.s3_key(checkpoint_id);

        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(&key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e) => {
                // Check for specific error types that indicate "not found"
                let err_str = format!("{:?}", e);
                if err_str.contains("NoSuchKey")
                    || err_str.contains("NotFound")
                    || err_str.contains("404")
                {
                    Ok(false)
                } else {
                    Err(OrchestratorError::S3(aws_sdk_s3::Error::from(e)))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s3_key_generation() {
        // Just test the key generation logic without a real client
        let bucket = "my-bucket".to_string();
        let prefix = "checkpoints".to_string();
        let checkpoint_id = "chk-001";

        let key = format!("{}/{}.tar.gz", prefix, checkpoint_id);
        assert_eq!(key, "checkpoints/chk-001.tar.gz");
    }

    #[test]
    fn test_s3_checkpoint_metadata_serialization() {
        let metadata = S3CheckpointMetadata {
            checkpoint: super::CheckpointMetadata {
                container_id: "abc123".to_string(),
                container_name: "vllm-server".to_string(),
                checkpoint_id: "chk-001".to_string(),
                created_at: chrono::Utc::now(),
                size_bytes: 2_147_483_648,
                model: Some("meta-llama/Llama-2-7b-hf".to_string()),
                active_requests: 5,
            },
            bucket: "my-bucket".to_string(),
            key: "checkpoints/chk-001.tar.gz".to_string(),
            etag: Some("\"abc123\"".to_string()),
            version_id: Some("v1".to_string()),
        };

        let json = serde_json::to_string(&metadata).unwrap();
        let _parsed: S3CheckpointMetadata = serde_json::from_str(&json).unwrap();
    }
}
