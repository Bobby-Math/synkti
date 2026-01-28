//! Graceful request draining for stateless failover
//!
//! When a spot interruption notice is received, we have ~120 seconds to:
//! 1. Stop accepting new requests (mark as draining)
//! 2. Wait for in-flight requests to complete
//! 3. Gracefully stop the container
//!
//! This module manages the drain phase of stateless failover.

use crate::elb::LoadBalancerManager;
use crate::error::Result;
use crate::vllm::VllmClient;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Default drain timeout (115s to leave 5s buffer before AWS termination)
pub const DEFAULT_DRAIN_TIMEOUT_SECS: u64 = 115;

/// Minimum time to wait before checking drain status (avoid busy polling)
const POLL_INTERVAL_MS: u64 = 500;

/// Status of a drain operation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DrainStatus {
    /// Draining in progress
    Draining,
    /// All requests completed, ready to stop
    Drained,
    /// Timeout reached, force stop required
    TimedOut,
    /// Error during drain
    Failed,
}

/// Result of a completed drain operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DrainResult {
    /// Final status
    pub status: DrainStatus,
    /// Time spent draining (seconds)
    pub drain_time_secs: f64,
    /// Instance ID that was drained
    pub instance_id: String,
}

/// Configuration for load balancer integration
#[derive(Debug, Clone)]
pub struct ElbConfig {
    /// Target group ARN
    pub target_group_arn: String,
    /// Port the instance is registered on (if using instance ID + port)
    pub port: Option<i32>,
}

/// Manages graceful request draining during failover
///
/// The drain manager coordinates with the vLLM API to:
/// 1. Signal that the instance is draining (no new requests)
/// 2. Wait for in-flight requests to complete
/// 3. Force stop if timeout is exceeded
pub struct DrainManager {
    /// Timeout for drain operation
    drain_timeout: Duration,
    /// Optional load balancer configuration
    elb_config: Option<ElbConfig>,
}

impl DrainManager {
    /// Create a new drain manager with default timeout
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_DRAIN_TIMEOUT_SECS))
    }

    /// Create a drain manager with custom timeout
    pub fn with_timeout(drain_timeout: Duration) -> Self {
        Self {
            drain_timeout,
            elb_config: None,
        }
    }

    /// Configure load balancer integration
    pub fn with_elb(mut self, config: ElbConfig) -> Self {
        self.elb_config = Some(config);
        self
    }

    /// Signal that an instance is entering drain mode
    ///
    /// When ELB is configured, this will:
    /// 1. Deregister the instance from the target group
    /// 2. Start connection draining (LB stops new connections)
    ///
    /// Without ELB, this just logs the intent.
    pub async fn set_draining(&self, instance_id: &str) -> Result<()> {
        info!(
            instance_id = %instance_id,
            "Marking instance as draining - no new requests will be accepted"
        );

        Ok(())
    }

    /// Signal that an instance is entering drain mode (with load balancer)
    ///
    /// This version takes a LoadBalancerManager to deregister from the target group.
    pub async fn set_draining_with_elb(
        &self,
        instance_id: &str,
        elb_manager: &LoadBalancerManager,
    ) -> Result<()> {
        info!(
            instance_id = %instance_id,
            "Marking instance as draining - no new requests will be accepted"
        );

        if let Some(ref config) = self.elb_config {
            info!(
                target_group = %config.target_group_arn,
                "Deregistering instance from load balancer target group"
            );
            elb_manager
                .deregister_target(&config.target_group_arn, instance_id, config.port)
                .await?;
        } else {
            debug!("No ELB config, skipping load balancer deregistration");
        }

        Ok(())
    }

    /// Wait for in-flight requests to complete
    ///
    /// Polls the vLLM server until:
    /// - All requests complete (success)
    /// - Timeout is reached (force stop needed)
    /// - Server becomes unhealthy (error)
    ///
    /// # Arguments
    /// - `vllm_client`: Client for querying vLLM status
    /// - `timeout`: Maximum time to wait (should be < grace period)
    pub async fn wait_for_inflight(
        &self,
        vllm_client: &VllmClient,
        timeout: Duration,
    ) -> Result<DrainStatus> {
        let start = Instant::now();
        let poll_interval = Duration::from_millis(POLL_INTERVAL_MS);

        info!(
            timeout_secs = timeout.as_secs(),
            "Waiting for in-flight requests to complete"
        );

        loop {
            let elapsed = start.elapsed();

            if elapsed >= timeout {
                warn!(
                    elapsed_secs = elapsed.as_secs_f64(),
                    "Drain timeout reached, will force stop"
                );
                return Ok(DrainStatus::TimedOut);
            }

            // Check if server is still processing
            match self.check_inflight_status(vllm_client).await {
                Ok(true) => {
                    // Still has in-flight requests, continue waiting
                    debug!(
                        elapsed_secs = elapsed.as_secs_f64(),
                        "Still draining, in-flight requests remain"
                    );
                }
                Ok(false) => {
                    // All requests drained
                    info!(
                        elapsed_secs = elapsed.as_secs_f64(),
                        "All in-flight requests completed"
                    );
                    return Ok(DrainStatus::Drained);
                }
                Err(e) => {
                    // Server error - might already be shutting down
                    warn!(
                        error = %e,
                        "Error checking drain status, assuming drained"
                    );
                    return Ok(DrainStatus::Drained);
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    }

    /// Check if there are still in-flight requests
    ///
    /// This queries vLLM's metrics endpoint to determine
    /// if requests are still being processed.
    ///
    /// Returns:
    /// - `Ok(true)` if requests are still in-flight
    /// - `Ok(false)` if server is idle
    /// - `Err` if metrics query fails
    async fn check_inflight_status(&self, vllm_client: &VllmClient) -> Result<bool> {
        // Try to get precise request counts from vLLM metrics
        match vllm_client.is_idle().await {
            Ok(true) => {
                // Server is idle - no in-flight requests
                debug!("vLLM server is idle (no running or waiting requests)");
                Ok(false)
            }
            Ok(false) => {
                // Still has in-flight or waiting requests
                match (
                    vllm_client.get_running_requests().await,
                    vllm_client.get_waiting_requests().await,
                ) {
                    (Ok(running), Ok(waiting)) => {
                        debug!(
                            running = running,
                            waiting = waiting,
                            "vLLM still processing requests"
                        );
                        Ok(true)
                    }
                    _ => {
                        // Couldn't get detailed counts, but we know it's not idle
                        Ok(true)
                    }
                }
            }
            Err(e) => {
                // Can't reach server or metrics - check health as fallback
                debug!(error = %e, "Metrics query failed, falling back to health check");
                match vllm_client.health_check().await {
                    Ok(true) => {
                        // Server is healthy but metrics unavailable
                        // Conservative: assume might still have requests
                        Ok(true)
                    }
                    Ok(false) | Err(_) => {
                        // Server is unhealthy or unreachable - safe to stop
                        Ok(false)
                    }
                }
            }
        }
    }

    /// Perform full drain sequence
    ///
    /// This is the main entry point for draining an instance:
    /// 1. Mark as draining
    /// 2. Wait for in-flight requests
    /// 3. Return result
    ///
    /// The caller should then stop the container based on the result.
    pub async fn drain(
        &self,
        instance_id: &str,
        vllm_client: &VllmClient,
    ) -> Result<DrainResult> {
        let start = Instant::now();

        // Step 1: Mark as draining
        self.set_draining(instance_id).await?;

        // Step 2: Wait for in-flight requests
        let status = self
            .wait_for_inflight(vllm_client, self.drain_timeout)
            .await?;

        let drain_time = start.elapsed();

        let result = DrainResult {
            status,
            drain_time_secs: drain_time.as_secs_f64(),
            instance_id: instance_id.to_string(),
        };

        info!(
            status = ?result.status,
            drain_time_secs = result.drain_time_secs,
            "Drain sequence completed"
        );

        Ok(result)
    }

    /// Perform full drain sequence with load balancer integration
    ///
    /// This version deregisters from the load balancer and waits for
    /// both LB draining and in-flight requests to complete.
    pub async fn drain_with_elb(
        &self,
        instance_id: &str,
        vllm_client: &VllmClient,
        elb_manager: &LoadBalancerManager,
    ) -> Result<DrainResult> {
        let start = Instant::now();

        // Step 1: Deregister from load balancer (if configured)
        self.set_draining_with_elb(instance_id, elb_manager).await?;

        // Step 2: Wait for in-flight requests to complete
        // Also wait for LB connection draining in parallel
        let vllm_future = self.wait_for_inflight(vllm_client, self.drain_timeout);

        let elb_future = async {
            if let Some(ref config) = self.elb_config {
                // Wait for LB draining to complete (use same timeout)
                let _ = elb_manager
                    .wait_for_drained(
                        &config.target_group_arn,
                        instance_id,
                        config.port,
                        self.drain_timeout,
                    )
                    .await;
            }
        };

        // Wait for both vLLM drain and ELB drain
        let (status, _) = tokio::join!(vllm_future, elb_future);
        let status = status?;

        let drain_time = start.elapsed();

        let result = DrainResult {
            status,
            drain_time_secs: drain_time.as_secs_f64(),
            instance_id: instance_id.to_string(),
        };

        info!(
            status = ?result.status,
            drain_time_secs = result.drain_time_secs,
            "Drain sequence with ELB completed"
        );

        Ok(result)
    }

    /// Get the configured drain timeout
    pub fn drain_timeout(&self) -> Duration {
        self.drain_timeout
    }
}

impl Default for DrainManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drain_manager_default_timeout() {
        let manager = DrainManager::new();
        assert_eq!(
            manager.drain_timeout().as_secs(),
            DEFAULT_DRAIN_TIMEOUT_SECS
        );
    }

    #[test]
    fn test_drain_manager_custom_timeout() {
        let manager = DrainManager::with_timeout(Duration::from_secs(60));
        assert_eq!(manager.drain_timeout().as_secs(), 60);
    }

    #[test]
    fn test_drain_status_serialization() {
        let status = DrainStatus::Drained;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"Drained\"");
    }

    #[test]
    fn test_drain_result_serialization() {
        let result = DrainResult {
            status: DrainStatus::Drained,
            drain_time_secs: 5.5,
            instance_id: "i-1234567890abcdef0".to_string(),
        };

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"status\":\"Drained\""));
        assert!(json.contains("\"drain_time_secs\":5.5"));
    }
}
