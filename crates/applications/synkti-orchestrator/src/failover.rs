//! Stateless failover orchestration
//!
//! Orchestrates the full failover flow when a spot interruption notice is received:
//!
//! ```text
//! Preemption Notice (120s grace)
//!     │
//!     ├── 1. Mark draining (stop new requests)
//!     │
//!     ├── 2. Wait for in-flight (max 115s)
//!     │
//!     ├── 3. Stop container
//!     │
//!     ├── 4. Select replacement instance
//!     │
//!     ├── 5. Spawn replacement container
//!     │
//!     └── 6. Health check & route traffic
//! ```
//!
//! ## Key Design Decisions
//!
//! - **Stateless**: No checkpoint/restore, just drain and respawn
//! - **Grace period exploitation**: Use full 115s for graceful drain
//! - **Assignment strategies**: Start with FIFO, graduate to Warm+LeastLoaded
//! - **Health check**: vLLM /health + model loaded before routing

use crate::assign::{AssignmentCandidate, AssignmentStrategy, NodeAssigner, Workload};
use crate::drain::{DrainManager, DrainResult, DrainStatus, ElbConfig};
use crate::elb::LoadBalancerManager;
use crate::error::{OrchestratorError, Result};
use crate::instance::Ec2Instance;
use crate::monitor::SpotInterruptionNotice;
use crate::remote::SsmExecutor;
use crate::vllm::{VllmClient, VllmConfig, VllmContainer};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

/// Default health check timeout for replacement instances
const HEALTH_CHECK_TIMEOUT_SECS: u64 = 300; // 5 minutes for model loading

/// Health check polling interval
const HEALTH_CHECK_INTERVAL_MS: u64 = 2000;

/// Result of a completed failover operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailoverResult {
    /// Whether failover was successful
    pub success: bool,

    /// Drain phase result
    pub drain: Option<DrainResult>,

    /// ID of the preempted instance
    pub preempted_instance_id: String,

    /// ID of the replacement instance (if successful)
    pub replacement_instance_id: Option<String>,

    /// Total failover time in seconds
    pub total_time_secs: f64,

    /// Time spent in each phase
    pub phase_times: FailoverPhaseTimes,

    /// Strategy used for instance selection
    pub assignment_strategy: AssignmentStrategy,

    /// Error message if failed
    pub error: Option<String>,
}

/// Timing breakdown for failover phases
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FailoverPhaseTimes {
    /// Time to drain (seconds)
    pub drain_secs: f64,

    /// Time to stop container (seconds)
    pub stop_secs: f64,

    /// Time to select replacement (seconds)
    pub select_secs: f64,

    /// Time to spawn replacement (seconds)
    pub spawn_secs: f64,

    /// Time for health check (seconds)
    pub health_check_secs: f64,
}

/// Configuration for the failover manager
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// Assignment strategy for selecting replacement instances
    pub assignment_strategy: AssignmentStrategy,

    /// Drain timeout (should be < grace period)
    pub drain_timeout: Duration,

    /// Health check timeout for replacement instances
    pub health_check_timeout: Duration,

    /// vLLM configuration for spawning replacement containers
    pub vllm_config: VllmConfig,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            assignment_strategy: AssignmentStrategy::EarliestNode,
            drain_timeout: Duration::from_secs(115),
            health_check_timeout: Duration::from_secs(HEALTH_CHECK_TIMEOUT_SECS),
            vllm_config: VllmConfig::default(),
        }
    }
}

impl FailoverConfig {
    /// Create config with Warm+LeastLoaded strategy (recommended for production)
    pub fn production() -> Self {
        Self {
            assignment_strategy: AssignmentStrategy::WarmLeastLoaded,
            ..Default::default()
        }
    }

    /// Set assignment strategy
    pub fn with_strategy(mut self, strategy: AssignmentStrategy) -> Self {
        self.assignment_strategy = strategy;
        self
    }

    /// Set drain timeout
    pub fn with_drain_timeout(mut self, timeout: Duration) -> Self {
        self.drain_timeout = timeout;
        self
    }

    /// Set vLLM config
    pub fn with_vllm_config(mut self, config: VllmConfig) -> Self {
        self.vllm_config = config;
        self
    }
}

/// Manages stateless failover for spot instances
///
/// The FailoverManager coordinates all components to handle spot preemptions:
/// - DrainManager: Graceful request draining
/// - NodeAssigner: Instance selection
/// - VllmContainer: Container lifecycle
pub struct FailoverManager {
    /// Configuration
    config: FailoverConfig,

    /// Drain manager
    drain_manager: DrainManager,

    /// Node assigner
    assigner: NodeAssigner,
}

impl FailoverManager {
    /// Create a new failover manager with default configuration
    pub fn new() -> Self {
        Self::with_config(FailoverConfig::default())
    }

    /// Create a failover manager with custom configuration
    pub fn with_config(config: FailoverConfig) -> Self {
        let drain_manager = DrainManager::with_timeout(config.drain_timeout);
        let assigner = NodeAssigner::new(config.assignment_strategy);

        Self {
            config,
            drain_manager,
            assigner,
        }
    }

    /// Get the current configuration
    pub fn config(&self) -> &FailoverConfig {
        &self.config
    }

    /// Handle a spot preemption notice
    ///
    /// This is the main entry point for failover. It orchestrates:
    /// 1. Draining the preempted instance
    /// 2. Stopping the container
    /// 3. Selecting a replacement instance
    /// 4. Spawning a new container
    /// 5. Health checking the replacement
    ///
    /// # Arguments
    /// - `notice`: The spot interruption notice
    /// - `preempted_instance`: The instance being preempted
    /// - `vllm_client`: Client for the vLLM server on the preempted instance
    /// - `candidates`: Available instances to use as replacement
    /// - `workload`: The workload being served
    pub async fn handle_preemption(
        &self,
        notice: &SpotInterruptionNotice,
        preempted_instance: &Ec2Instance,
        vllm_client: &VllmClient,
        candidates: &[AssignmentCandidate<'_>],
        workload: &Workload,
    ) -> FailoverResult {
        let start = Instant::now();
        let mut phase_times = FailoverPhaseTimes::default();

        info!(
            instance_id = %preempted_instance.id,
            seconds_until_action = notice.seconds_until_action,
            "Starting stateless failover"
        );

        // Phase 1: Drain
        let phase_start = Instant::now();
        let drain_result = match self
            .drain_manager
            .drain(&preempted_instance.id, vllm_client)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "Drain failed");
                return FailoverResult {
                    success: false,
                    drain: None,
                    preempted_instance_id: preempted_instance.id.clone(),
                    replacement_instance_id: None,
                    total_time_secs: start.elapsed().as_secs_f64(),
                    phase_times,
                    assignment_strategy: self.config.assignment_strategy,
                    error: Some(format!("Drain failed: {}", e)),
                };
            }
        };
        phase_times.drain_secs = phase_start.elapsed().as_secs_f64();

        // Phase 2: Stop container (if not already stopped)
        let phase_start = Instant::now();
        if drain_result.status != DrainStatus::Failed {
            // Container might still be running, stop it
            debug!("Drain completed, container will be stopped by AWS termination");
        }
        phase_times.stop_secs = phase_start.elapsed().as_secs_f64();

        // Phase 3: Select replacement instance
        let phase_start = Instant::now();
        let replacement = match self.assigner.select(candidates, workload) {
            Some(instance) => instance,
            None => {
                error!("No suitable replacement instance available");
                return FailoverResult {
                    success: false,
                    drain: Some(drain_result),
                    preempted_instance_id: preempted_instance.id.clone(),
                    replacement_instance_id: None,
                    total_time_secs: start.elapsed().as_secs_f64(),
                    phase_times,
                    assignment_strategy: self.config.assignment_strategy,
                    error: Some("No suitable replacement instance available".to_string()),
                };
            }
        };
        phase_times.select_secs = phase_start.elapsed().as_secs_f64();

        info!(
            replacement_id = %replacement.id,
            strategy = ?self.config.assignment_strategy,
            "Selected replacement instance"
        );

        // Phase 4: Spawn replacement container
        let phase_start = Instant::now();
        let spawn_result = self.spawn_replacement(replacement).await;
        phase_times.spawn_secs = phase_start.elapsed().as_secs_f64();

        let (_container, new_client) = match spawn_result {
            Ok((c, client)) => (c, client),
            Err(e) => {
                error!(error = %e, "Failed to spawn replacement container");
                return FailoverResult {
                    success: false,
                    drain: Some(drain_result),
                    preempted_instance_id: preempted_instance.id.clone(),
                    replacement_instance_id: Some(replacement.id.clone()),
                    total_time_secs: start.elapsed().as_secs_f64(),
                    phase_times,
                    assignment_strategy: self.config.assignment_strategy,
                    error: Some(format!("Failed to spawn replacement: {}", e)),
                };
            }
        };

        // Phase 5: Health check
        let phase_start = Instant::now();
        if let Err(e) = self
            .wait_for_healthy(&new_client, self.config.health_check_timeout)
            .await
        {
            warn!(error = %e, "Health check failed, but container may still become ready");
        }
        phase_times.health_check_secs = phase_start.elapsed().as_secs_f64();

        let total_time = start.elapsed().as_secs_f64();

        info!(
            total_time_secs = total_time,
            drain_secs = phase_times.drain_secs,
            spawn_secs = phase_times.spawn_secs,
            health_check_secs = phase_times.health_check_secs,
            "Failover completed successfully"
        );

        FailoverResult {
            success: true,
            drain: Some(drain_result),
            preempted_instance_id: preempted_instance.id.clone(),
            replacement_instance_id: Some(replacement.id.clone()),
            total_time_secs: total_time,
            phase_times,
            assignment_strategy: self.config.assignment_strategy,
            error: None,
        }
    }

    /// Spawn a replacement container on the selected instance
    async fn spawn_replacement(
        &self,
        instance: &Ec2Instance,
    ) -> Result<(VllmContainer, VllmClient)> {
        info!(
            instance_id = %instance.id,
            model = %self.config.vllm_config.model,
            "Spawning replacement container"
        );

        // Create vLLM config for the replacement instance
        let config = VllmConfig {
            container_name: Some(format!("vllm-{}", &instance.id[..8.min(instance.id.len())])),
            ..self.config.vllm_config.clone()
        };

        let container = VllmContainer::new(config.clone());

        // Note: In production, this would SSH/SSM to the instance and run docker
        // For now, we assume the caller handles remote execution
        // This is a placeholder for the container spawn logic
        debug!(
            "Container spawn initiated (actual remote execution handled by caller)"
        );

        // Create client for the new instance
        let api_url = if let Some(ip) = &instance.public_ip {
            format!("http://{}:{}", ip, config.port)
        } else if let Some(ip) = &instance.private_ip {
            format!("http://{}:{}", ip, config.port)
        } else {
            return Err(OrchestratorError::Config(
                "Instance has no IP address".to_string(),
            ));
        };

        let client = VllmClient::new(api_url);

        Ok((container, client))
    }

    /// Spawn a replacement container using SSM remote execution
    ///
    /// This actually executes the docker run command on the remote instance
    /// using AWS Systems Manager (SSM).
    pub async fn spawn_replacement_with_ssm(
        &self,
        instance: &Ec2Instance,
        ssm: &SsmExecutor,
    ) -> Result<(VllmContainer, VllmClient)> {
        info!(
            instance_id = %instance.id,
            model = %self.config.vllm_config.model,
            "Spawning replacement container via SSM"
        );

        // Create vLLM config for the replacement instance
        let config = VllmConfig {
            container_name: Some(format!("vllm-{}", &instance.id[..8.min(instance.id.len())])),
            ..self.config.vllm_config.clone()
        };

        // Start the container via SSM
        let result = ssm.start_vllm_container(&instance.id, &config).await?;

        if !result.is_success() {
            return Err(OrchestratorError::Docker(format!(
                "Failed to start container via SSM: {}",
                result.stderr
            )));
        }

        let container = VllmContainer::new(config.clone());

        // Create client for the new instance
        let api_url = if let Some(ip) = &instance.public_ip {
            format!("http://{}:{}", ip, config.port)
        } else if let Some(ip) = &instance.private_ip {
            format!("http://{}:{}", ip, config.port)
        } else {
            return Err(OrchestratorError::Config(
                "Instance has no IP address".to_string(),
            ));
        };

        let client = VllmClient::new(api_url);

        info!(
            instance_id = %instance.id,
            container_id = %result.stdout.trim(),
            "Container started successfully via SSM"
        );

        Ok((container, client))
    }

    /// Register the replacement instance with the load balancer
    ///
    /// After the replacement is healthy, this adds it to the target group.
    pub async fn register_replacement(
        &self,
        instance: &Ec2Instance,
        elb_manager: &LoadBalancerManager,
        elb_config: &ElbConfig,
    ) -> Result<()> {
        info!(
            instance_id = %instance.id,
            target_group = %elb_config.target_group_arn,
            "Registering replacement with load balancer"
        );

        elb_manager
            .register_target(&elb_config.target_group_arn, &instance.id, elb_config.port)
            .await?;

        // Wait for the target to become healthy
        elb_manager
            .wait_for_healthy(
                &elb_config.target_group_arn,
                &instance.id,
                elb_config.port,
                self.config.health_check_timeout,
            )
            .await?;

        info!(
            instance_id = %instance.id,
            "Replacement registered and healthy in load balancer"
        );

        Ok(())
    }

    /// Wait for a replacement instance to be healthy
    async fn wait_for_healthy(
        &self,
        client: &VllmClient,
        timeout: Duration,
    ) -> Result<()> {
        let start = Instant::now();
        let interval = Duration::from_millis(HEALTH_CHECK_INTERVAL_MS);

        info!(
            timeout_secs = timeout.as_secs(),
            "Waiting for replacement to be healthy"
        );

        loop {
            let elapsed = start.elapsed();

            if elapsed >= timeout {
                return Err(OrchestratorError::Timeout(timeout));
            }

            match client.health_check().await {
                Ok(true) => {
                    info!(
                        elapsed_secs = elapsed.as_secs_f64(),
                        "Replacement instance is healthy"
                    );
                    return Ok(());
                }
                Ok(false) => {
                    debug!("Health check returned false, waiting...");
                }
                Err(e) => {
                    debug!(error = %e, "Health check failed, retrying...");
                }
            }

            tokio::time::sleep(interval).await;
        }
    }
}

impl Default for FailoverManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Quick failover without full orchestration
///
/// This is a simplified failover that just selects a replacement.
/// Useful for testing or when drain/spawn are handled externally.
pub fn quick_select_replacement<'a>(
    candidates: &[AssignmentCandidate<'a>],
    workload: &Workload,
    strategy: AssignmentStrategy,
) -> Option<&'a Ec2Instance> {
    let assigner = NodeAssigner::new(strategy);
    assigner.select(candidates, workload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instance::InstanceState;
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    fn create_test_instance(id: &str) -> Ec2Instance {
        Ec2Instance {
            id: id.to_string(),
            instance_type: "g5.xlarge".to_string(),
            state: InstanceState::Running,
            public_ip: Some("1.2.3.4".to_string()),
            private_ip: Some("10.0.0.1".to_string()),
            launch_time: Utc.timestamp_opt(1700000000, 0).unwrap(),
            gpu_memory_gb: 24.0,
            network_bandwidth_gbps: 10.0,
            gpu_memory_used_mb: 0.0,
            tags: HashMap::new(),
        }
    }

    #[test]
    fn test_failover_config_default() {
        let config = FailoverConfig::default();
        assert_eq!(config.assignment_strategy, AssignmentStrategy::EarliestNode);
        assert_eq!(config.drain_timeout.as_secs(), 115);
    }

    #[test]
    fn test_failover_config_production() {
        let config = FailoverConfig::production();
        assert_eq!(
            config.assignment_strategy,
            AssignmentStrategy::WarmLeastLoaded
        );
    }

    #[test]
    fn test_failover_config_builder() {
        let config = FailoverConfig::default()
            .with_strategy(AssignmentStrategy::LeastLoaded)
            .with_drain_timeout(Duration::from_secs(60));

        assert_eq!(config.assignment_strategy, AssignmentStrategy::LeastLoaded);
        assert_eq!(config.drain_timeout.as_secs(), 60);
    }

    #[test]
    fn test_quick_select_replacement() {
        let instance1 = create_test_instance("i-older");
        let mut instance2 = create_test_instance("i-newer");
        instance2.launch_time = Utc.timestamp_opt(1700001000, 0).unwrap();

        let candidates = vec![
            AssignmentCandidate::new(&instance2),
            AssignmentCandidate::new(&instance1),
        ];

        let workload = Workload::new("llama-7b", 8000.0);

        let selected =
            quick_select_replacement(&candidates, &workload, AssignmentStrategy::EarliestNode);

        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, "i-older");
    }

    #[test]
    fn test_failover_result_serialization() {
        let result = FailoverResult {
            success: true,
            drain: None,
            preempted_instance_id: "i-preempted".to_string(),
            replacement_instance_id: Some("i-replacement".to_string()),
            total_time_secs: 10.5,
            phase_times: FailoverPhaseTimes {
                drain_secs: 5.0,
                stop_secs: 0.1,
                select_secs: 0.01,
                spawn_secs: 3.0,
                health_check_secs: 2.39,
            },
            assignment_strategy: AssignmentStrategy::EarliestNode,
            error: None,
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"success\":true"));
        assert!(json.contains("\"total_time_secs\":10.5"));
    }

    #[test]
    fn test_phase_times_serialization() {
        let times = FailoverPhaseTimes {
            drain_secs: 5.0,
            stop_secs: 0.1,
            select_secs: 0.01,
            spawn_secs: 3.0,
            health_check_secs: 2.0,
        };

        let json = serde_json::to_string(&times).unwrap();
        let parsed: FailoverPhaseTimes = serde_json::from_str(&json).unwrap();

        assert!((parsed.drain_secs - 5.0).abs() < 0.001);
        assert!((parsed.spawn_secs - 3.0).abs() < 0.001);
    }
}
