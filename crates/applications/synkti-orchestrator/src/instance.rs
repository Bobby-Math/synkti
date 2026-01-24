//! EC2 instance management
//!
//! Manages the lifecycle of EC2 spot instances for the orchestrator.

use crate::error::{OrchestratorError, Result};
use aws_config::BehaviorVersion;
use aws_sdk_ec2::{
    types::{
        BlockDeviceMapping, IamInstanceProfileSpecification, Instance, InstanceMarketOptionsRequest,
        InstanceType, MarketType, ResourceType, Tag, TagSpecification, EbsBlockDevice, VolumeType,
    },
    Client,
};
use aws_types::region::Region;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

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

    /// Root volume size in GB (default 100GB for ML workloads)
    #[serde(default = "default_root_volume_size")]
    pub root_volume_size_gb: i32,
}

fn default_instance_type() -> String {
    "g4dn.xlarge".to_string()
}

fn default_root_volume_size() -> i32 {
    100 // 100GB default for ML workloads (Docker images + models)
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
            root_volume_size_gb: default_root_volume_size(),
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

    /// Set root volume size in GB
    pub fn with_root_volume_size(mut self, size_gb: i32) -> Self {
        self.root_volume_size_gb = size_gb;
        self
    }

    /// Launch this instance spec as an EC2 instance
    pub async fn launch(&self, client: &Client, tags: Vec<(String, String)>) -> Result<Ec2Instance> {
        info!(
            "Launching instance: type={}, ami={}, root_volume={}GB",
            self.instance_type, self.ami_id, self.root_volume_size_gb
        );

        // Configure root volume (100GB default for ML workloads)
        let root_device = BlockDeviceMapping::builder()
            .device_name("/dev/xvda")
            .ebs(
                EbsBlockDevice::builder()
                    .volume_size(self.root_volume_size_gb)
                    .volume_type(VolumeType::Gp3)
                    .delete_on_termination(true)
                    .build()
            )
            .build();

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
            .block_device_mappings(root_device)
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

/// Get the ECS GPU-optimized AMI ID for the current region
///
/// Uses SSM parameter to get the latest AMI with NVIDIA drivers
pub async fn get_gpu_ami(_client: &Client, region: &str) -> Result<String> {
    use aws_sdk_ssm::Client as SsmClient;

    let config = aws_config::defaults(BehaviorVersion::latest())
        .region(Region::new(region.to_string()))
        .load()
        .await;

    let ssm_client = SsmClient::new(&config);

    ssm_client
        .get_parameter()
        .name("/aws/service/ecs/optimized-ami/amazon-linux-2023/gpu/recommended/image_id")
        .send()
        .await
        .map_err(OrchestratorError::from_aws)
        .and_then(|r| {
            r.parameter
                .and_then(|p| p.value)
                .ok_or_else(|| OrchestratorError::Config("GPU AMI not found in SSM".to_string()))
        })
}

/// Get the standard AL2023 AMI ID for the current region
pub async fn get_standard_ami(client: &Client, _region: &str) -> Result<String> {
    let response = client
        .describe_images()
        .owners("amazon")
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("name")
                .values("al2023-ami-2023.*-x86_64")
                .build(),
        )
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("virtualization-type")
                .values("hvm")
                .build(),
        )
        .send()
        .await
        .map_err(OrchestratorError::from_ec2)?;

    let images = response
        .images
        .ok_or_else(|| OrchestratorError::Config("No images in response".to_string()))?;

    images
        .first()
        .and_then(|img| img.image_id.clone())
        .ok_or_else(|| OrchestratorError::Config("No AL2023 AMI found".to_string()))
}

/// List all worker instances for a given project (by tag)
pub async fn list_workers(client: &Client, project_name: &str) -> Result<Vec<Ec2Instance>> {
    debug!("Listing workers for project: {}", project_name);

    let response = client
        .describe_instances()
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("tag:SynktiCluster")
                .values(project_name)
                .build(),
        )
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("tag:SynktiRole")
                .values("worker")
                .build(),
        )
        .send()
        .await
        .map_err(OrchestratorError::from_ec2)?;

    let mut instances = Vec::new();

    for reservation in response.reservations() {
        for inst in reservation.instances() {
            let instance_type = inst
                .instance_type
                .as_ref()
                .map(|t| t.as_str())
                .unwrap_or("unknown");

            // Estimate GPU memory for this instance type
            let gpu_memory_gb = estimate_gpu_memory(instance_type);
            let network_bandwidth = estimate_network_bandwidth(instance_type);

            match Ec2Instance::from_aws_instance(inst, gpu_memory_gb, network_bandwidth) {
                Ok(ec2_inst) => instances.push(ec2_inst),
                Err(e) => warn!("Failed to parse instance: {}", e),
            }
        }
    }

    Ok(instances)
}

/// Terminate a worker instance by ID
pub async fn terminate_worker(client: &Client, instance_id: &str) -> Result<()> {
    info!("Terminating worker instance: {}", instance_id);

    client
        .terminate_instances()
        .instance_ids(instance_id)
        .send()
        .await
        .map_err(OrchestratorError::from_ec2)?;

    info!("Worker {} termination initiated", instance_id);
    Ok(())
}

/// Estimate GPU memory based on instance type
fn estimate_gpu_memory(instance_type: &str) -> f64 {
    match instance_type {
        t if t.starts_with("g4dn.xlarge") || t.starts_with("g4dn.2xlarge") => 16.0,
        t if t.starts_with("g4dn.4xlarge") || t.starts_with("g4dn.8xlarge") => 16.0,
        t if t.starts_with("g4dn.16xlarge") => 16.0,
        t if t.starts_with("g5.xlarge") || t.starts_with("g5.2xlarge") => 24.0,
        t if t.starts_with("g5.4xlarge") || t.starts_with("g5.8xlarge") => 24.0,
        t if t.starts_with("g5.12xlarge") || t.starts_with("g5.16xlarge") => 24.0,
        t if t.starts_with("g5.24xlarge") || t.starts_with("g5.48xlarge") => 24.0,
        t if t.starts_with("g6") => 24.0,
        t if t.starts_with("p3.2") => 16.0,
        t if t.starts_with("p3.8") => 64.0,
        t if t.starts_with("p3.16") => 128.0,
        t if t.starts_with("p3dn") => 256.0,
        t if t.starts_with("p4d") => 320.0,
        t if t.starts_with("p4de") => 640.0,
        t if t.starts_with("p5") => 640.0,
        _ => 0.0, // CPU instances have no GPU memory
    }
}

/// Estimate network bandwidth based on instance type (Gbps)
fn estimate_network_bandwidth(instance_type: &str) -> f64 {
    match instance_type {
        t if t.starts_with("g4dn") => 10.0,
        t if t.starts_with("g5") => 10.0,
        t if t.starts_with("g6") => 10.0,
        t if t.starts_with("p3") => 10.0,
        t if t.starts_with("p3dn") => 25.0,
        t if t.starts_with("p4d") => 25.0,
        t if t.starts_with("p4de") => 25.0,
        t if t.starts_with("p5") => 25.0,
        _ => 10.0, // Default to 10 Gbps
    }
}

/// Check if an instance type is a GPU instance
pub fn is_gpu_instance_type(instance_type: &str) -> bool {
    instance_type.starts_with("g4dn")
        || instance_type.starts_with("g5")
        || instance_type.starts_with("g6")
        || instance_type.starts_with("p3")
        || instance_type.starts_with("p3dn")
        || instance_type.starts_with("p4d")
        || instance_type.starts_with("p4de")
        || instance_type.starts_with("p5")
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
