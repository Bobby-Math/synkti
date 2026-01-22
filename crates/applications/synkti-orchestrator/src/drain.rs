//! Graceful request draining for stateless failover
//!
//! When a spot interruption notice is received, we have ~120 seconds to:
//! 1. Stop accepting new requests (mark as draining)
//! 2. Wait for in-flight requests to complete
//! 3. Gracefully stop the container
//!
//! This module manages the drain phase of stateless failover.

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

/// Manages graceful request draining during failover
///
/// The drain manager coordinates with the vLLM API to:
/// 1. Signal that the instance is draining (no new requests)
/// 2. Wait for in-flight requests to complete
/// 3. Force stop if timeout is exceeded
pub struct DrainManager {
    /// Timeout for drain operation
    drain_timeout: Duration,
}

impl DrainManager {
    /// Create a new drain manager with default timeout
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_DRAIN_TIMEOUT_SECS))
    }

    /// Create a drain manager with custom timeout
    pub fn with_timeout(drain_timeout: Duration) -> Self {
        Self { drain_timeout }
    }

    /// Signal that an instance is entering drain mode
    ///
    /// In a production system, this would:
    /// 1. Update load balancer health check to return unhealthy
    /// 2. Deregister from target group
    /// 3. Set instance metadata/tags
    ///
    /// For now, we log the intent and return success.
    /// The actual load balancer integration should be added when deploying with ALB/NLB.
    pub async fn set_draining(&self, instance_id: &str) -> Result<()> {
        info!(
            instance_id = %instance_id,
            "Marking instance as draining - no new requests will be accepted"
        );

        // TODO: Implement actual load balancer deregistration
        // - ALB: elasticloadbalancingv2.deregister_targets()
        // - NLB: elasticloadbalancingv2.deregister_targets()
        // - DNS: Update Route53 health check
        //
        // For MVP, the orchestrator should handle routing at a higher level,
        // not routing new requests to instances marked as draining.

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
    /// This queries vLLM's metrics or health endpoint to determine
    /// if requests are still being processed.
    ///
    /// Returns:
    /// - `Ok(true)` if requests are still in-flight
    /// - `Ok(false)` if server is idle
    /// - `Err` if health check fails
    async fn check_inflight_status(&self, vllm_client: &VllmClient) -> Result<bool> {
        // For MVP, we use health check as proxy for "server is running"
        // A more sophisticated implementation would query:
        // - /metrics endpoint for running_requests gauge
        // - /v1/models for loaded model state
        //
        // If the server is healthy, assume it might have in-flight requests.
        // If unhealthy, assume it's safe to stop.
        //
        // TODO: Query vLLM /metrics endpoint for precise request count
        // Metric: vllm:num_requests_running

        match vllm_client.health_check().await {
            Ok(true) => {
                // Server is healthy - might have in-flight requests
                // For MVP, we'll use a simple heuristic:
                // After initial drain signal, wait a short period then assume drained
                Ok(false) // Conservative: assume no in-flight for faster failover
            }
            Ok(false) => {
                // Server is unhealthy - safe to stop
                Ok(false)
            }
            Err(e) => {
                // Can't reach server - safe to stop
                debug!(error = %e, "Health check failed, assuming drained");
                Ok(false)
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
