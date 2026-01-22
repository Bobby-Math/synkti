//! Peer discovery for P2P orchestration
//!
//! In the P2P architecture, each node needs to discover its peers.
//! This module provides EC2 tag-based discovery for AWS deployments.
//!
//! ## How It Works
//!
//! 1. Each Synkti node tags itself with `SynktiCluster=<cluster-name>`
//! 2. Nodes query EC2 for other instances with the same tag
//! 3. The candidates list is populated with discovered peers
//! 4. Periodic refresh keeps the list current as nodes join/leave
//!
//! ## Future: libp2p
//!
//! For Phase 3 (DePIN/multi-cloud), this will be replaced with libp2p:
//! - mDNS for local network discovery
//! - Kademlia DHT for global discovery
//! - No cloud API dependency

use crate::error::{OrchestratorError, Result};
use crate::instance::{Ec2Instance, InstanceState};
use aws_sdk_ec2::types::Filter;
use aws_sdk_ec2::Client as Ec2Client;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Default tag key for cluster membership
pub const DEFAULT_CLUSTER_TAG_KEY: &str = "SynktiCluster";

/// Default tag key for node role
pub const DEFAULT_ROLE_TAG_KEY: &str = "SynktiRole";

/// Role value for worker nodes
pub const ROLE_WORKER: &str = "worker";

/// Refresh interval for peer discovery
const DEFAULT_REFRESH_INTERVAL_SECS: u64 = 30;

/// Configuration for peer discovery
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Cluster name (tag value for SynktiCluster)
    pub cluster_name: String,

    /// How often to refresh the peer list
    pub refresh_interval: Duration,

    /// Current instance ID (to exclude self from peers)
    pub self_instance_id: Option<String>,

    /// Tag key for cluster membership
    pub cluster_tag_key: String,

    /// Tag key for node role
    pub role_tag_key: String,
}

impl DiscoveryConfig {
    /// Create a new discovery config for a cluster
    pub fn new(cluster_name: impl Into<String>) -> Self {
        Self {
            cluster_name: cluster_name.into(),
            refresh_interval: Duration::from_secs(DEFAULT_REFRESH_INTERVAL_SECS),
            self_instance_id: None,
            cluster_tag_key: DEFAULT_CLUSTER_TAG_KEY.to_string(),
            role_tag_key: DEFAULT_ROLE_TAG_KEY.to_string(),
        }
    }

    /// Set the current instance ID (to exclude from peer list)
    pub fn with_self_instance_id(mut self, id: impl Into<String>) -> Self {
        self.self_instance_id = Some(id.into());
        self
    }

    /// Set the refresh interval
    pub fn with_refresh_interval(mut self, interval: Duration) -> Self {
        self.refresh_interval = interval;
        self
    }
}

/// Discovers peer nodes via EC2 tags
///
/// This is the AWS-specific implementation for Phase 2.
/// Each node in a Synkti cluster tags itself, and peers discover
/// each other by querying EC2 for instances with matching tags.
pub struct PeerDiscovery {
    /// EC2 client
    client: Ec2Client,

    /// Discovery configuration
    config: DiscoveryConfig,

    /// Cached list of discovered peers
    peers: Arc<RwLock<Vec<Ec2Instance>>>,
}

impl PeerDiscovery {
    /// Create a new peer discovery instance
    pub fn new(client: Ec2Client, config: DiscoveryConfig) -> Self {
        Self {
            client,
            config,
            peers: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// Create from AWS config
    pub async fn from_config(
        aws_config: &aws_config::SdkConfig,
        config: DiscoveryConfig,
    ) -> Self {
        let client = Ec2Client::new(aws_config);
        Self::new(client, config)
    }

    /// Discover peers once and return them
    pub async fn discover_peers(&self) -> Result<Vec<Ec2Instance>> {
        info!(
            cluster = %self.config.cluster_name,
            "Discovering peers in cluster"
        );

        // Build filters for EC2 query
        let cluster_filter = Filter::builder()
            .name(format!("tag:{}", self.config.cluster_tag_key))
            .values(&self.config.cluster_name)
            .build();

        let role_filter = Filter::builder()
            .name(format!("tag:{}", self.config.role_tag_key))
            .values(ROLE_WORKER)
            .build();

        let state_filter = Filter::builder()
            .name("instance-state-name")
            .values("running")
            .build();

        // Query EC2
        let response = self
            .client
            .describe_instances()
            .filters(cluster_filter)
            .filters(role_filter)
            .filters(state_filter)
            .send()
            .await
            .map_err(|e| OrchestratorError::Docker(format!("Failed to describe instances: {}", e)))?;

        // Parse response into Ec2Instance objects
        let mut peers = Vec::new();

        for reservation in response.reservations() {
            for instance in reservation.instances() {
                let instance_id = instance.instance_id().unwrap_or_default();

                // Skip self
                if let Some(ref self_id) = self.config.self_instance_id {
                    if instance_id == self_id {
                        debug!(instance_id = %instance_id, "Skipping self");
                        continue;
                    }
                }

                // Parse instance
                let peer = parse_ec2_instance(instance);
                if let Some(p) = peer {
                    debug!(
                        instance_id = %p.id,
                        instance_type = %p.instance_type,
                        "Discovered peer"
                    );
                    peers.push(p);
                }
            }
        }

        info!(
            cluster = %self.config.cluster_name,
            peer_count = peers.len(),
            "Discovery complete"
        );

        // Update cache
        {
            let mut cache = self.peers.write().await;
            *cache = peers.clone();
        }

        Ok(peers)
    }

    /// Get the cached peer list
    pub async fn get_peers(&self) -> Vec<Ec2Instance> {
        self.peers.read().await.clone()
    }

    /// Get a shared reference to the peer list (for use in async tasks)
    pub fn peers_ref(&self) -> Arc<RwLock<Vec<Ec2Instance>>> {
        self.peers.clone()
    }

    /// Start a background task that periodically refreshes the peer list
    pub fn start_refresh_task(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = self.config.refresh_interval;

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);

            loop {
                ticker.tick().await;

                match self.discover_peers().await {
                    Ok(peers) => {
                        debug!(peer_count = peers.len(), "Refreshed peer list");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to refresh peer list");
                    }
                }
            }
        })
    }

    /// Get the cluster name
    pub fn cluster_name(&self) -> &str {
        &self.config.cluster_name
    }
}

/// Parse an AWS EC2 instance into our Ec2Instance type
fn parse_ec2_instance(instance: &aws_sdk_ec2::types::Instance) -> Option<Ec2Instance> {
    let id = instance.instance_id()?.to_string();
    let instance_type = instance
        .instance_type()
        .map(|t| t.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let state = instance
        .state()
        .and_then(|s| s.name())
        .map(|n| match n.as_str() {
            "running" => InstanceState::Running,
            "pending" => InstanceState::Pending,
            "stopping" => InstanceState::Stopping,
            "stopped" => InstanceState::Stopped,
            "shutting-down" => InstanceState::ShuttingDown,
            "terminated" => InstanceState::Terminated,
            _ => InstanceState::Pending, // Default to Pending for unknown states
        })
        .unwrap_or(InstanceState::Pending);

    let public_ip = instance.public_ip_address().map(|s| s.to_string());
    let private_ip = instance.private_ip_address().map(|s| s.to_string());

    let launch_time = instance
        .launch_time()
        .and_then(|t| {
            chrono::DateTime::from_timestamp(t.secs(), t.subsec_nanos())
        })
        .unwrap_or_else(chrono::Utc::now);

    // Parse tags into HashMap
    let mut tags = HashMap::new();
    for tag in instance.tags() {
        if let (Some(key), Some(value)) = (tag.key(), tag.value()) {
            tags.insert(key.to_string(), value.to_string());
        }
    }

    // Estimate GPU memory based on instance type
    let gpu_memory_gb = estimate_gpu_memory(&instance_type);

    Some(Ec2Instance {
        id,
        instance_type,
        state,
        public_ip,
        private_ip,
        launch_time,
        gpu_memory_gb,
        network_bandwidth_gbps: 10.0, // Approximate
        gpu_memory_used_mb: 0.0,
        tags,
    })
}

/// Estimate GPU memory based on instance type
fn estimate_gpu_memory(instance_type: &str) -> f64 {
    match instance_type {
        // G4dn instances (T4 GPU - 16GB)
        t if t.starts_with("g4dn") => 16.0,
        // G5 instances (A10G GPU - 24GB)
        t if t.starts_with("g5") => 24.0,
        // G6 instances (L4 GPU - 24GB)
        t if t.starts_with("g6") => 24.0,
        // P3 instances (V100 GPU - 16/32GB)
        t if t.starts_with("p3.2") => 16.0,
        t if t.starts_with("p3.8") => 64.0,  // 4x16GB
        t if t.starts_with("p3.16") => 128.0, // 8x16GB
        t if t.starts_with("p3dn") => 256.0, // 8x32GB
        // P4 instances (A100 GPU - 40/80GB)
        t if t.starts_with("p4d") => 320.0, // 8x40GB
        t if t.starts_with("p4de") => 640.0, // 8x80GB
        // P5 instances (H100 GPU - 80GB)
        t if t.starts_with("p5") => 640.0, // 8x80GB
        // Default
        _ => 16.0,
    }
}

/// Tag the current instance as a Synkti worker
///
/// This should be called when a node starts up to make itself
/// discoverable by other nodes in the cluster.
pub async fn tag_self_as_worker(
    client: &Ec2Client,
    instance_id: &str,
    cluster_name: &str,
) -> Result<()> {
    use aws_sdk_ec2::types::Tag;

    info!(
        instance_id = %instance_id,
        cluster = %cluster_name,
        "Tagging self as Synkti worker"
    );

    let cluster_tag = Tag::builder()
        .key(DEFAULT_CLUSTER_TAG_KEY)
        .value(cluster_name)
        .build();

    let role_tag = Tag::builder()
        .key(DEFAULT_ROLE_TAG_KEY)
        .value(ROLE_WORKER)
        .build();

    client
        .create_tags()
        .resources(instance_id)
        .tags(cluster_tag)
        .tags(role_tag)
        .send()
        .await
        .map_err(|e| OrchestratorError::Docker(format!("Failed to tag instance: {}", e)))?;

    info!(
        instance_id = %instance_id,
        "Successfully tagged as Synkti worker"
    );

    Ok(())
}

/// Remove Synkti worker tags from an instance
///
/// Call this during graceful shutdown to remove the instance
/// from the discoverable peer list.
pub async fn untag_self_as_worker(
    client: &Ec2Client,
    instance_id: &str,
) -> Result<()> {
    use aws_sdk_ec2::types::Tag;

    info!(
        instance_id = %instance_id,
        "Removing Synkti worker tags"
    );

    let cluster_tag = Tag::builder()
        .key(DEFAULT_CLUSTER_TAG_KEY)
        .build();

    let role_tag = Tag::builder()
        .key(DEFAULT_ROLE_TAG_KEY)
        .build();

    client
        .delete_tags()
        .resources(instance_id)
        .tags(cluster_tag)
        .tags(role_tag)
        .send()
        .await
        .map_err(|e| OrchestratorError::Docker(format!("Failed to remove tags: {}", e)))?;

    info!(
        instance_id = %instance_id,
        "Successfully removed Synkti worker tags"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovery_config_builder() {
        let config = DiscoveryConfig::new("my-cluster")
            .with_self_instance_id("i-12345")
            .with_refresh_interval(Duration::from_secs(60));

        assert_eq!(config.cluster_name, "my-cluster");
        assert_eq!(config.self_instance_id, Some("i-12345".to_string()));
        assert_eq!(config.refresh_interval, Duration::from_secs(60));
    }

    #[test]
    fn test_gpu_memory_estimation() {
        assert_eq!(estimate_gpu_memory("g4dn.xlarge"), 16.0);
        assert_eq!(estimate_gpu_memory("g5.2xlarge"), 24.0);
        assert_eq!(estimate_gpu_memory("p3.2xlarge"), 16.0);
        assert_eq!(estimate_gpu_memory("p4d.24xlarge"), 320.0);
        assert_eq!(estimate_gpu_memory("t3.medium"), 16.0); // default
    }

    #[test]
    fn test_default_tags() {
        assert_eq!(DEFAULT_CLUSTER_TAG_KEY, "SynktiCluster");
        assert_eq!(DEFAULT_ROLE_TAG_KEY, "SynktiRole");
        assert_eq!(ROLE_WORKER, "worker");
    }
}
