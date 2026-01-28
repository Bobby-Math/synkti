//! Core types shared across Synkti components

use serde::{Deserialize, Serialize};

/// Unique identifier for an instance
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct InstanceId(pub String);

impl InstanceId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Instance health status
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Unhealthy,
    Unknown,
    Starting,
    Draining,
}

/// Instance state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    Pending,
    Running,
    ShuttingDown,
    Terminated,
    Stopping,
    Stopped,
    Unknown,
}

/// Provider type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderType {
    AwsGpu,
    GcpTpu,
}

impl std::fmt::Display for ProviderType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProviderType::AwsGpu => write!(f, "aws-gpu"),
            ProviderType::GcpTpu => write!(f, "gcp-tpu"),
        }
    }
}

/// Instance type specification
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceType {
    pub name: String,
    pub provider: ProviderType,
    pub gpu_memory_gb: f64,
    pub network_bandwidth_gbps: f64,
}

/// Launch configuration for a new instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchConfig {
    pub instance_type: String,
    pub region: String,
    pub tags: Vec<(String, String)>,
    pub iam_profile: Option<String>,
}
