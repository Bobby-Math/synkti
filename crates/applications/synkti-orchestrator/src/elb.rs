//! Load balancer integration for graceful draining
//!
//! Manages ALB/NLB target registration/deregistration during failover.
//!
//! ## Drain Flow
//!
//! 1. Deregister target from target group (stops new connections)
//! 2. Wait for deregistration delay (default 300s, we use 115s max)
//! 3. In-flight requests complete or timeout
//! 4. Instance is safe to stop
//!
//! ## Prerequisites
//!
//! - Target group ARN must be known
//! - IAM permissions for `elasticloadbalancingv2:DeregisterTargets`

use crate::error::{OrchestratorError, Result};
use aws_sdk_elasticloadbalancingv2::types::{TargetDescription, TargetHealthStateEnum};
use aws_sdk_elasticloadbalancingv2::Client as ElbClient;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Polling interval when waiting for target health changes
const HEALTH_POLL_INTERVAL_MS: u64 = 2000;

/// Load balancer manager for ALB/NLB operations
pub struct LoadBalancerManager {
    client: ElbClient,
}

impl LoadBalancerManager {
    /// Create a new load balancer manager
    pub fn new(client: ElbClient) -> Self {
        Self { client }
    }

    /// Create from AWS config
    pub async fn from_config(config: &aws_config::SdkConfig) -> Self {
        let client = ElbClient::new(config);
        Self::new(client)
    }

    /// Deregister an instance from a target group
    ///
    /// This tells the load balancer to stop sending new requests to this instance.
    /// Existing connections will be allowed to complete (connection draining).
    ///
    /// # Arguments
    /// - `target_group_arn`: ARN of the target group
    /// - `instance_id`: EC2 instance ID to deregister
    /// - `port`: Optional port (required if target group uses instance ID + port)
    pub async fn deregister_target(
        &self,
        target_group_arn: &str,
        instance_id: &str,
        port: Option<i32>,
    ) -> Result<()> {
        info!(
            target_group = %target_group_arn,
            instance_id = %instance_id,
            "Deregistering target from load balancer"
        );

        let mut target = TargetDescription::builder().id(instance_id);

        if let Some(p) = port {
            target = target.port(p);
        }

        self.client
            .deregister_targets()
            .target_group_arn(target_group_arn)
            .targets(target.build())
            .send()
            .await
            .map_err(|e| {
                OrchestratorError::Docker(format!("Failed to deregister target: {}", e))
            })?;

        info!(
            instance_id = %instance_id,
            "Target deregistered successfully"
        );

        Ok(())
    }

    /// Register an instance with a target group
    ///
    /// Used to add the replacement instance to the load balancer.
    pub async fn register_target(
        &self,
        target_group_arn: &str,
        instance_id: &str,
        port: Option<i32>,
    ) -> Result<()> {
        info!(
            target_group = %target_group_arn,
            instance_id = %instance_id,
            "Registering target with load balancer"
        );

        let mut target = TargetDescription::builder().id(instance_id);

        if let Some(p) = port {
            target = target.port(p);
        }

        self.client
            .register_targets()
            .target_group_arn(target_group_arn)
            .targets(target.build())
            .send()
            .await
            .map_err(|e| {
                OrchestratorError::Docker(format!("Failed to register target: {}", e))
            })?;

        info!(
            instance_id = %instance_id,
            "Target registered successfully"
        );

        Ok(())
    }

    /// Wait for a target to become healthy
    ///
    /// Polls the target health until it reaches "healthy" state or timeout.
    pub async fn wait_for_healthy(
        &self,
        target_group_arn: &str,
        instance_id: &str,
        port: Option<i32>,
        timeout: Duration,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(HEALTH_POLL_INTERVAL_MS);

        info!(
            instance_id = %instance_id,
            timeout_secs = timeout.as_secs(),
            "Waiting for target to become healthy"
        );

        loop {
            if start.elapsed() > timeout {
                return Err(OrchestratorError::Timeout(timeout));
            }

            match self
                .get_target_health(target_group_arn, instance_id, port)
                .await
            {
                Ok(Some(TargetHealthStateEnum::Healthy)) => {
                    info!(
                        instance_id = %instance_id,
                        elapsed_secs = start.elapsed().as_secs_f64(),
                        "Target is healthy"
                    );
                    return Ok(());
                }
                Ok(Some(TargetHealthStateEnum::Unhealthy)) => {
                    debug!(
                        instance_id = %instance_id,
                        "Target still unhealthy, waiting..."
                    );
                }
                Ok(Some(TargetHealthStateEnum::Initial)) => {
                    debug!(
                        instance_id = %instance_id,
                        "Target health initializing..."
                    );
                }
                Ok(Some(TargetHealthStateEnum::Draining)) => {
                    debug!(
                        instance_id = %instance_id,
                        "Target is draining (unexpected for new registration)"
                    );
                }
                Ok(Some(state)) => {
                    debug!(
                        instance_id = %instance_id,
                        state = ?state,
                        "Target in unknown state"
                    );
                }
                Ok(None) => {
                    debug!(
                        instance_id = %instance_id,
                        "Target not found in target group"
                    );
                }
                Err(e) => {
                    warn!(
                        instance_id = %instance_id,
                        error = %e,
                        "Error checking target health"
                    );
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Wait for a target to finish draining
    ///
    /// After deregistration, the load balancer allows existing connections to complete.
    /// This waits until the target is fully drained or timeout.
    pub async fn wait_for_drained(
        &self,
        target_group_arn: &str,
        instance_id: &str,
        port: Option<i32>,
        timeout: Duration,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let poll_interval = Duration::from_millis(HEALTH_POLL_INTERVAL_MS);

        info!(
            instance_id = %instance_id,
            timeout_secs = timeout.as_secs(),
            "Waiting for target to finish draining"
        );

        loop {
            if start.elapsed() > timeout {
                warn!(
                    instance_id = %instance_id,
                    "Drain timeout reached, proceeding anyway"
                );
                return Ok(()); // Timeout is acceptable for drain
            }

            match self
                .get_target_health(target_group_arn, instance_id, port)
                .await
            {
                Ok(None) => {
                    // Target no longer in target group = fully drained
                    info!(
                        instance_id = %instance_id,
                        elapsed_secs = start.elapsed().as_secs_f64(),
                        "Target fully drained"
                    );
                    return Ok(());
                }
                Ok(Some(TargetHealthStateEnum::Draining)) => {
                    debug!(
                        instance_id = %instance_id,
                        "Target still draining..."
                    );
                }
                Ok(Some(state)) => {
                    debug!(
                        instance_id = %instance_id,
                        state = ?state,
                        "Target in unexpected state during drain"
                    );
                }
                Err(e) => {
                    // API error might mean target is gone (which is success)
                    debug!(
                        instance_id = %instance_id,
                        error = %e,
                        "Error checking target, assuming drained"
                    );
                    return Ok(());
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Get the health status of a target
    async fn get_target_health(
        &self,
        target_group_arn: &str,
        instance_id: &str,
        port: Option<i32>,
    ) -> Result<Option<TargetHealthStateEnum>> {
        let mut target = TargetDescription::builder().id(instance_id);

        if let Some(p) = port {
            target = target.port(p);
        }

        let response = self
            .client
            .describe_target_health()
            .target_group_arn(target_group_arn)
            .targets(target.build())
            .send()
            .await
            .map_err(|e| {
                OrchestratorError::Docker(format!("Failed to describe target health: {}", e))
            })?;

        // Find the matching target
        for health in response.target_health_descriptions() {
            if let Some(target) = health.target() {
                if target.id() == Some(instance_id) {
                    if let Some(health_state) = health.target_health() {
                        return Ok(health_state.state().cloned());
                    }
                }
            }
        }

        Ok(None)
    }

    /// Get all healthy targets in a target group
    pub async fn get_healthy_targets(
        &self,
        target_group_arn: &str,
    ) -> Result<Vec<String>> {
        let response = self
            .client
            .describe_target_health()
            .target_group_arn(target_group_arn)
            .send()
            .await
            .map_err(|e| {
                OrchestratorError::Docker(format!("Failed to describe target health: {}", e))
            })?;

        let healthy: Vec<String> = response
            .target_health_descriptions()
            .iter()
            .filter_map(|desc| {
                let is_healthy = desc
                    .target_health()
                    .and_then(|h| h.state())
                    .map(|s| *s == TargetHealthStateEnum::Healthy)
                    .unwrap_or(false);

                if is_healthy {
                    desc.target().and_then(|t| t.id().map(|s| s.to_string()))
                } else {
                    None
                }
            })
            .collect();

        Ok(healthy)
    }
}

/// Create an ELB client from the default AWS config
pub async fn create_elb_client() -> ElbClient {
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    ElbClient::new(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: ELB tests require actual AWS resources
    // These are placeholder tests for the API structure

    #[test]
    fn test_load_balancer_manager_creation() {
        // This just tests that the types compile correctly
        // Actual AWS tests would require mocking or integration testing
    }
}
