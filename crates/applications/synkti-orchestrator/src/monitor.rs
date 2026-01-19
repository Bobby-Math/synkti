//! Spot instance interruption monitoring
//!
//! Polls the EC2 instance metadata endpoint for spot termination notices.
//!
//! ## Metadata Endpoint
//!
//! AWS spot instances receive a 2-minute warning before termination via the
//! instance metadata endpoint:
//!
//! ```text
//! http://169.254.169.254/latest/meta-data/spot/instance-action
//! ```
//!
//! Response format:
//! ```json
//! {
//!   "action": "terminate",
//!   "time": "2024-01-17T10:30:00Z"
//! }
//! ```
//!
//! ## Grace Period
//!
//! AWS provides a 120-second grace period between the notice and actual termination.
//! This is our window to checkpoint and migrate.

use crate::error::{OrchestratorError, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::pin::Pin;
use std::time::Duration;
use tokio::time::interval;
use tracing::{debug, info, warn};

/// EC2 instance metadata endpoint base URL
const METADATA_BASE: &str = "http://169.254.169.254";

/// Spot instance action endpoint
const SPOT_ACTION_ENDPOINT: &str = "/latest/meta-data/spot/instance-action";

/// AWS standard grace period for spot termination (seconds)
pub const GRACE_PERIOD_SECONDS: u64 = 120;

/// Spot interruption action type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotAction {
    /// Instance will be terminated
    Terminate,
    /// Instance will be stopped
    Stop,
    /// Instance will be hibernated
    Hibernate,
}

impl SpotAction {
    /// Parse from string (as returned by AWS metadata endpoint)
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "terminate" => Some(Self::Terminate),
            "stop" => Some(Self::Stop),
            "hibernate" => Some(Self::Hibernate),
            _ => None,
        }
    }
}

/// Spot interruption notice from EC2 metadata
#[derive(Debug, Clone)]
pub struct SpotInterruptionNotice {
    /// The action that will be taken
    pub action: SpotAction,

    /// When the action will occur (ISO 8601 timestamp)
    pub time: DateTime<Utc>,

    /// Time remaining until termination (seconds)
    pub seconds_until_action: u64,
}

/// Raw spot instance action response from AWS
#[derive(Debug, Deserialize)]
struct SpotInstanceAction {
    #[serde(rename = "action")]
    action: String,

    #[serde(rename = "time")]
    time: String,
}

/// Spot instance monitor
///
/// Polls the EC2 metadata endpoint for spot interruption notices.
pub struct SpotMonitor {
    /// HTTP client for metadata endpoint
    client: reqwest::Client,

    /// Polling interval
    interval: Duration,
}

impl SpotMonitor {
    /// Create a new spot monitor with default polling interval (5 seconds)
    pub fn new() -> Self {
        Self::with_interval(Duration::from_secs(5))
    }

    /// Create a new spot monitor with custom polling interval
    pub fn with_interval(interval: Duration) -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            interval,
        }
    }

    /// Check once for a spot interruption notice
    ///
    /// Returns `Ok(None)` if no notice is present (instance is safe).
    /// Returns `Ok(Some(notice))` if a termination notice was found.
    pub async fn check_notice(&self) -> Result<Option<SpotInterruptionNotice>> {
        let url = format!("{}{}", METADATA_BASE, SPOT_ACTION_ENDPOINT);

        debug!("Checking spot interruption notice at {}", url);

        let response = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                // HTTP 404 is expected when no notice is present
                if e.status() == Some(reqwest::StatusCode::NOT_FOUND) {
                    debug!("No spot interruption notice (404)");
                    return Ok(None);
                }
                // Connection refused means we're not on EC2
                if e.is_connect() {
                    warn!("Not running on EC2 (connection refused to metadata endpoint)");
                    return Ok(None);
                }
                return Err(OrchestratorError::Http(e));
            }
        };

        // Parse the response
        let action: SpotInstanceAction = response.json().await?;

        let spot_action = SpotAction::from_str(action.action.as_str())
            .ok_or_else(|| OrchestratorError::Config(format!("Unknown spot action: {}", action.action)))?;

        let time = DateTime::parse_from_rfc3339(&action.time)
            .map_err(|e| OrchestratorError::Config(format!("Invalid timestamp: {}", e)))?
            .with_timezone(&Utc);

        let now = Utc::now();
        let seconds_until = if time > now {
            (time - now).num_seconds().max(0) as u64
        } else {
            0
        };

        info!(
            "Spot interruption notice received: action={:?}, time={}, seconds_until={}",
            action, time, seconds_until
        );

        Ok(Some(SpotInterruptionNotice {
            action: spot_action,
            time,
            seconds_until_action: seconds_until,
        }))
    }

    /// Start continuous monitoring
    ///
    /// Returns a pinned stream that yields `SpotInterruptionNotice` when a notice is received.
    pub fn monitor_stream(&self) -> Pin<Box<dyn futures::Stream<Item = SpotInterruptionNotice> + Send>> {
        let client = self.client.clone();
        let interval_duration = self.interval;

        Box::pin(async_stream::stream! {
            let mut ticker = interval(interval_duration);
            loop {
                ticker.tick().await;

                let url = format!("{}{}", METADATA_BASE, SPOT_ACTION_ENDPOINT);

                match client.get(&url).send().await {
                    Ok(response) => {
                        if response.status() == reqwest::StatusCode::OK {
                            if let Ok(action) = response.json::<SpotInstanceAction>().await {
                                if let Some(spot_action) = SpotAction::from_str(&action.action) {
                                    if let Ok(time) = DateTime::parse_from_rfc3339(&action.time) {
                                        let time = time.with_timezone(&Utc);
                                        let now = Utc::now();
                                        let seconds_until = if time > now {
                                            (time - now).num_seconds().max(0) as u64
                                        } else {
                                            0
                                        };

                                        yield SpotInterruptionNotice {
                                            action: spot_action,
                                            time,
                                            seconds_until_action: seconds_until,
                                        };
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if e.status() != Some(reqwest::StatusCode::NOT_FOUND) && !e.is_connect() {
                            tracing::warn!("Error checking spot notice: {}", e);
                        }
                    }
                }
            }
        })
    }
}

impl Default for SpotMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spot_action_from_str() {
        assert_eq!(SpotAction::from_str("terminate"), Some(SpotAction::Terminate));
        assert_eq!(SpotAction::from_str("stop"), Some(SpotAction::Stop));
        assert_eq!(SpotAction::from_str("hibernate"), Some(SpotAction::Hibernate));
        assert_eq!(SpotAction::from_str("unknown"), None);
    }
}
