//! Core traits for Synkti
//!
//! The SpotProvider trait defines the interface ALL cloud providers must implement.
//! The fleet scheduler works through this interface ONLY - never concrete types.

use async_trait::async_trait;
use std::time::Duration;

use crate::error::SynktiError;
use crate::types::*;

/// Result type for provider operations
pub type Result<T> = std::result::Result<T, SynktiError>;

/// Provider metrics
#[derive(Debug, Clone)]
pub struct ProviderMetrics {
    pub total_instances: u32,
    pub running_instances: u32,
    pub pending_instances: u32,
    pub total_gpu_memory_gb: f64,
    pub used_gpu_memory_gb: f64,
}

/// Instance filter for listing
#[derive(Debug, Clone)]
pub struct InstanceFilter {
    pub state: Option<InstanceState>,
    pub tags: Vec<(String, String)>,
}

/// Instance information
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: InstanceId,
    pub instance_type: String,
    pub state: InstanceState,
    pub health: HealthStatus,
    pub public_ip: Option<String>,
    pub private_ip: Option<String>,
    pub gpu_memory_gb: f64,
    pub launch_time: chrono::DateTime<chrono::Utc>,
}

/// All cloud providers must implement this trait.
/// The fleet scheduler works through this interface ONLY.
#[async_trait]
pub trait SpotProvider: Send + Sync {
    /// Provider identity
    fn provider_type(&self) -> ProviderType;
    fn region(&self) -> &str;

    /// Pricing (for cost optimization)
    async fn spot_price(&self, instance_type: &str) -> Result<f64>;
    async fn spot_price_history(&self, instance_type: &str, hours: u32) -> Result<Vec<f64>>;

    /// Lifecycle (for fleet management)
    async fn launch_instance(&self, config: &LaunchConfig) -> Result<InstanceId>;
    async fn terminate_instance(&self, id: &InstanceId) -> Result<()>;
    async fn list_instances(&self, filters: &[InstanceFilter]) -> Result<Vec<Instance>>;

    /// Interruption (for spot handling)
    async fn get_interruption_warning(&self, id: &InstanceId) -> Result<Option<Duration>>;
    async fn enable_interruption_monitoring(&self, id: &InstanceId) -> Result<()>;

    /// Health (for observability)
    async fn instance_health(&self, id: &InstanceId) -> Result<HealthStatus>;

    /// Metadata (for scheduling)
    fn supported_instance_types(&self) -> Vec<InstanceType>;
    fn max_model_size(&self, instance_type: &str) -> Result<usize>;
}
