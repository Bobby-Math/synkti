//! EC2 instance management
//!
//! Manages the lifecycle of EC2 spot instances for the orchestrator.

use crate::error::{OrchestratorError, Result};
use aws_config::BehaviorVersion;
use aws_sdk_ec2::{
    types::{
        IamInstanceProfileSpecification, Instance, InstanceMarketOptionsRequest, InstanceType,
        MarketType, ResourceType, Tag, TagSpecification,
    },
    Client,
};
use aws_types::region::Region;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info};

/// Default AWS region
pub const DEFAULT_REGION: &str = "us-east-1";

/// Create EC2 client from environment
pub async fn create_ec2_client(region: Option<String>) -> Result<Client> {
    let region_str = region.unwrap_or_else(|| DEFAULT_REGION.to_string());
    debug!("Creating EC2 client for region: {}", region_str);

    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region_str))
        .load()
        .await;

    Ok(Client::new(&config))
}

/// Instance state tracking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    /// Instance is pending
    Pending,
    /// Instance is running
    Running,
    /// Instance is stopping
    Stopping,
    /// Instance is stopped
    Stopped,
    /// Instance is shutting down
    ShuttingDown,
    /// Instance is terminated
    Terminated,
}

impl InstanceState {
    /// Check if instance is active (can run workloads)
    pub fn is_active(&self) -> bool {
        matches!(self, Self::Running | Self::Pending)
    }
}

/// EC2 instance specification for launching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSpec {
    /// Instance type (e.g., "g4dn.xlarge")
    pub instance_type: String,

    /// AMI ID
    pub ami_id: String,

    /// Instance type for GPU inference
    #[serde(default = "default_instance_type")]
    pub instance_type_name: String,

    /// GPU memory in GB
    pub gpu_memory_gb: f64,

    /// Network bandwidth in Gbps
    pub network_bandwidth_gbps: f64,

    /// Spot maximum price (USD per hour)
    pub spot_max_price: Option<String>,

    /// Key pair name
    pub key_name: Option<String>,

    /// Security group IDs
    pub security_group_ids: Vec<String>,

    /// Subnet ID
    pub subnet_id: Option<String>,

    /// User data script (cloud-init)
    pub user_data: Option<String>,

    /// IAM instance profile name (for SSM access, etc.)
    pub iam_instance_profile: Option<String>,
}

fn default_instance_type() -> String {
    "g4dn.xlarge".to_string()
}

impl Default for InstanceSpec {
    fn default() -> Self {
        Self {
            instance_type: "g4dn.xlarge".to_string(),
            ami_id: String::new(), // Must be set
            instance_type_name: "g4dn.xlarge".to_string(),
            gpu_memory_gb: 16.0,
            network_bandwidth_gbps: 10.0,
            spot_max_price: None,
            key_name: None,
            security_group_ids: vec![],
            subnet_id: None,
            user_data: None,
            iam_instance_profile: None,
        }
    }
}

impl InstanceSpec {
    /// Create a new instance spec
    pub fn new(ami_id: impl Into<String>) -> Self {
        Self {
            ami_id: ami_id.into(),
            ..Default::default()
        }
    }

    /// Set instance type
    pub fn with_instance_type(mut self, instance_type: impl Into<String>) -> Self {
        self.instance_type = instance_type.into();
        self.instance_type_name = self.instance_type.clone();
        self
    }

    /// Set GPU memory
    pub fn with_gpu_memory(mut self, gb: f64) -> Self {
        self.gpu_memory_gb = gb;
        self
    }

    /// Set network bandwidth
    pub fn with_network_bandwidth(mut self, gbps: f64) -> Self {
        self.network_bandwidth_gbps = gbps;
        self
    }

    /// Set spot maximum price
    pub fn with_spot_price(mut self, price: impl Into<String>) -> Self {
        self.spot_max_price = Some(price.into());
        self
    }

    /// Set key pair
    pub fn with_key_pair(mut self, key_name: impl Into<String>) -> Self {
        self.key_name = Some(key_name.into());
        self
    }

    /// Add security group
    pub fn with_security_group(mut self, sg_id: impl Into<String>) -> Self {
        self.security_group_ids.push(sg_id.into());
        self
    }

    /// Set subnet
    pub fn with_subnet(mut self, subnet_id: impl Into<String>) -> Self {
        self.subnet_id = Some(subnet_id.into());
        self
    }

    /// Set user data
    pub fn with_user_data(mut self, user_data: impl Into<String>) -> Self {
        self.user_data = Some(user_data.into());
        self
    }

    /// Set IAM instance profile (for SSM access, etc.)
    pub fn with_iam_profile(mut self, profile: impl Into<String>) -> Self {
        self.iam_instance_profile = Some(profile.into());
        self
    }

    /// Launch this instance spec as an EC2 instance
    pub async fn launch(&self, client: &Client, tags: Vec<(String, String)>) -> Result<Ec2Instance> {
        info!(
            "Launching instance: type={}, ami={}",
            self.instance_type, self.ami_id
        );

        let mut run_req = client
            .run_instances()
            .image_id(&self.ami_id)
            .instance_type(InstanceType::from(self.instance_type.as_str()))
            .set_security_group_ids(if self.security_group_ids.is_empty() {
                None
            } else {
                Some(self.security_group_ids.clone())
            })
            .set_key_name(self.key_name.clone())
            .set_subnet_id(self.subnet_id.clone())
            .set_user_data(self.user_data.clone())
            .min_count(1)
            .max_count(1);

        // Add IAM profile if specified (for SSM access)
        if let Some(profile) = &self.iam_instance_profile {
            debug!("Using IAM instance profile: {}", profile);
            let iam_spec = IamInstanceProfileSpecification::builder()
                .name(profile)
                .build();
            run_req = run_req.iam_instance_profile(iam_spec);
        }

        // Add spot options if specified
        if self.spot_max_price.is_some() {
            debug!("Launching as spot instance");
            let market_options = InstanceMarketOptionsRequest::builder()
                .market_type(MarketType::Spot)
                .build();
            run_req = run_req.instance_market_options(market_options);
        }

        // Add tags
        if !tags.is_empty() {
            let tag_spec = TagSpecification::builder()
                .resource_type(ResourceType::Instance)
                .set_tags(Some(
                    tags.iter()
                        .map(|(k, v)| Tag::builder().key(k).value(v).build())
                        .collect(),
                ))
                .build();
            run_req = run_req.tag_specifications(tag_spec);
        }

        let response = run_req.send().await.map_err(OrchestratorError::from_ec2)?;
        let instances = response.instances();
        let instance = instances
            .first()
            .ok_or_else(|| OrchestratorError::config("No instance in response"))?;

        let instance_id = instance
            .instance_id
            .as_ref()
            .ok_or_else(|| OrchestratorError::config("No instance ID"))?;

        info!("Instance launched: {}", instance_id);

        Ok(Ec2Instance::from_aws_instance(
            instance,
            self.gpu_memory_gb,
            self.network_bandwidth_gbps,
        )?)
    }

    /// Get available GPU memory in MB
    pub fn available_memory_mb(&self) -> f64 {
        self.gpu_memory_gb * 1024.0
    }
}

/// Wrapper around EC2 instance
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ec2Instance {
    /// Instance ID
    pub id: String,

    /// Instance type
    pub instance_type: String,

    /// Current state
    pub state: InstanceState,

    /// Public IP address
    pub public_ip: Option<String>,

    /// Private IP address
    pub private_ip: Option<String>,

    /// Launch time
    pub launch_time: DateTime<Utc>,

    /// GPU memory in GB
    pub gpu_memory_gb: f64,

    /// Network bandwidth in Gbps
    pub network_bandwidth_gbps: f64,

    /// GPU memory currently used (MB)
    pub gpu_memory_used_mb: f64,

    /// Tags
    pub tags: HashMap<String, String>,
}

impl Ec2Instance {
    /// Create from AWS instance
    pub fn from_aws_instance(
        instance: &Instance,
        gpu_memory_gb: f64,
        network_bandwidth_gbps: f64,
    ) -> Result<Self> {
        use aws_sdk_ec2::types::InstanceStateName;

        let state_name = instance
            .state
            .as_ref()
            .and_then(|s| s.name.as_ref())
            .ok_or_else(|| OrchestratorError::Config("Missing instance state".to_string()))?;

        let state = match state_name {
            InstanceStateName::Pending => InstanceState::Pending,
            InstanceStateName::Running => InstanceState::Running,
            InstanceStateName::Stopping => InstanceState::Stopping,
            InstanceStateName::Stopped => InstanceState::Stopped,
            InstanceStateName::ShuttingDown => InstanceState::ShuttingDown,
            InstanceStateName::Terminated => InstanceState::Terminated,
            _ => InstanceState::Pending,
        };

        // Convert AWS DateTime to chrono DateTime
        let launch_time = instance
            .launch_time
            .as_ref()
            .map(|dt| {
                chrono::DateTime::from_timestamp(dt.secs(), dt.subsec_nanos() as u32)
                    .unwrap_or_else(|| chrono::Utc::now())
            })
            .unwrap_or_else(|| chrono::Utc::now());

        Ok(Self {
            id: instance
                .instance_id
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            instance_type: instance
                .instance_type
                .as_ref()
                .map(|t| t.as_str().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            state,
            public_ip: instance.public_ip_address.clone(),
            private_ip: instance.private_ip_address.clone(),
            launch_time,
            gpu_memory_gb,
            network_bandwidth_gbps,
            gpu_memory_used_mb: 0.0,
            tags: HashMap::new(),
        })
    }

    /// Get available GPU memory in MB
    pub fn available_memory_mb(&self) -> f64 {
        (self.gpu_memory_gb * 1024.0) - self.gpu_memory_used_mb
    }

    /// Check if instance can fit a given memory requirement
    pub fn can_fit_memory(&self, required_mb: f64) -> bool {
        self.available_memory_mb() >= required_mb
    }

    /// Update GPU memory usage
    pub fn with_memory_used(mut self, used_mb: f64) -> Self {
        self.gpu_memory_used_mb = used_mb;
        self
    }

    /// Wait until the instance is running
    pub async fn wait_until_running(
        &mut self,
        client: &Client,
        timeout: Duration,
    ) -> Result<()> {
        info!(
            "Waiting for instance {} to be running (timeout: {:?})",
            self.id, timeout
        );

        let start = std::time::Instant::now();

        while start.elapsed() < timeout {
            self.refresh_state(client).await?;

            if self.state == InstanceState::Running {
                info!("Instance {} is now running", self.id);
                return Ok(());
            }

            if matches!(
                self.state,
                InstanceState::Terminated | InstanceState::ShuttingDown
            ) {
                return Err(OrchestratorError::Config(format!(
                    "Instance {} terminated while waiting for running state",
                    self.id
                )));
            }

            tokio::time::sleep(Duration::from_secs(5)).await;
        }

        Err(OrchestratorError::Timeout(timeout))
    }

    /// Refresh the instance state from AWS
    pub async fn refresh_state(&mut self, client: &Client) -> Result<()> {
        debug!("Refreshing state for instance {}", self.id);

        let response = client
            .describe_instances()
            .instance_ids(&self.id)
            .send()
            .await
            .map_err(OrchestratorError::from_ec2)?;

        let reservations = response.reservations();
        let reservation = reservations
            .first()
            .ok_or_else(|| OrchestratorError::InstanceNotFound(self.id.clone()))?;

        let instances = reservation.instances();
        let instance = instances
            .first()
            .ok_or_else(|| OrchestratorError::InstanceNotFound(self.id.clone()))?;

        let updated = Ec2Instance::from_aws_instance(
            instance,
            self.gpu_memory_gb,
            self.network_bandwidth_gbps,
        )?;

        self.state = updated.state;
        self.public_ip = updated.public_ip;
        self.private_ip = updated.private_ip;

        Ok(())
    }

    /// Terminate this instance
    pub async fn terminate(&self, client: &Client) -> Result<()> {
        info!("Terminating instance {}", self.id);

        client
            .terminate_instances()
            .instance_ids(&self.id)
            .send()
            .await
            .map_err(OrchestratorError::from_ec2)?;

        info!("Instance {} termination initiated", self.id);
        Ok(())
    }
}

/// Predefined instance specs for common GPU instances
pub mod specs {
    use super::InstanceSpec;

    /// g4dn.xlarge (1x T4 GPU, 16GB VRAM, 10 Gbps network)
    pub fn g4dn_xlarge(ami_id: &str) -> InstanceSpec {
        InstanceSpec::new(ami_id)
            .with_instance_type("g4dn.xlarge")
            .with_gpu_memory(16.0)
            .with_network_bandwidth(10.0)
    }

    /// g4dn.2xlarge (1x T4 GPU, 16GB VRAM, up to 10 Gbps network)
    pub fn g4dn_2xlarge(ami_id: &str) -> InstanceSpec {
        InstanceSpec::new(ami_id)
            .with_instance_type("g4dn.2xlarge")
            .with_gpu_memory(16.0)
            .with_network_bandwidth(10.0)
    }

    /// g5.xlarge (1x A10G GPU, 24GB VRAM, up to 10 Gbps network)
    pub fn g5_xlarge(ami_id: &str) -> InstanceSpec {
        InstanceSpec::new(ami_id)
            .with_instance_type("g5.xlarge")
            .with_gpu_memory(24.0)
            .with_network_bandwidth(10.0)
    }

    /// g5.2xlarge (1x A10G GPU, 24GB VRAM, up to 10 Gbps network)
    pub fn g5_2xlarge(ami_id: &str) -> InstanceSpec {
        InstanceSpec::new(ami_id)
            .with_instance_type("g5.2xlarge")
            .with_gpu_memory(24.0)
            .with_network_bandwidth(10.0)
    }

    /// p3.2xlarge (1x V100 GPU, 16GB VRAM, up to 10 Gbps network)
    pub fn p3_2xlarge(ami_id: &str) -> InstanceSpec {
        InstanceSpec::new(ami_id)
            .with_instance_type("p3.2xlarge")
            .with_gpu_memory(16.0)
            .with_network_bandwidth(10.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instance_state_from_aws() {
        use aws_sdk_ec2::types::InstanceStateName;

        assert_eq!(InstanceState::Running.is_active(), true);
        assert_eq!(InstanceState::Pending.is_active(), true);
        assert_eq!(InstanceState::Terminated.is_active(), false);
        assert_eq!(InstanceState::Stopped.is_active(), false);
    }

    #[test]
    fn test_instance_state_is_active() {
        assert!(InstanceState::Running.is_active());
        assert!(InstanceState::Pending.is_active());
        assert!(!InstanceState::Terminated.is_active());
        assert!(!InstanceState::Stopped.is_active());
    }

    #[test]
    fn test_instance_spec_builder() {
        let spec = InstanceSpec::new("ami-12345")
            .with_instance_type("g5.xlarge")
            .with_gpu_memory(24.0)
            .with_network_bandwidth(10.0)
            .with_spot_price("0.50");

        assert_eq!(spec.ami_id, "ami-12345");
        assert_eq!(spec.instance_type_name, "g5.xlarge");
        assert_eq!(spec.gpu_memory_gb, 24.0);
        assert_eq!(spec.network_bandwidth_gbps, 10.0);
        assert_eq!(spec.spot_max_price, Some("0.50".to_string()));
    }

    #[test]
    fn test_ec2_instance_available_memory() {
        let instance = Ec2Instance {
            id: "i-123".to_string(),
            instance_type: "g4dn.xlarge".to_string(),
            state: InstanceState::Running,
            public_ip: Some("1.2.3.4".to_string()),
            private_ip: Some("10.0.0.1".to_string()),
            launch_time: Utc::now(),
            gpu_memory_gb: 16.0,
            network_bandwidth_gbps: 10.0,
            gpu_memory_used_mb: 4096.0,
            tags: HashMap::new(),
        };

        // 16GB = 16384 MB, 4096 MB used = 12288 MB available
        assert_eq!(instance.available_memory_mb(), 12288.0);
    }

    #[test]
    fn test_can_fit_memory() {
        let instance = Ec2Instance {
            id: "i-123".to_string(),
            instance_type: "g4dn.xlarge".to_string(),
            state: InstanceState::Running,
            public_ip: None,
            private_ip: Some("10.0.0.1".to_string()),
            launch_time: Utc::now(),
            gpu_memory_gb: 16.0,
            network_bandwidth_gbps: 10.0,
            gpu_memory_used_mb: 4096.0,
            tags: HashMap::new(),
        };

        assert!(instance.can_fit_memory(8000.0));
        assert!(!instance.can_fit_memory(15000.0));
    }
}
