//! Node assignment strategies for stateless failover
//!
//! When a spot preemption occurs, workloads need to be assigned to replacement instances.
//! In a stateless system, there's no checkpoint to transfer—but assignment decisions
//! still matter for latency, cost, and reliability.
//!
//! ## Strategies
//!
//! - **EarliestNode (FIFO)**: Assign to the oldest available node (deterministic, debuggable)
//! - **LeastLoaded**: Assign to the node with lowest current utilization
//! - **WarmLeastLoaded**: Prefer nodes with model already loaded, then least loaded
//!
//! ## Recommendation
//!
//! Start with `EarliestNode` for debugging, graduate to `WarmLeastLoaded` for production.

use crate::instance::Ec2Instance;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Assignment strategy types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AssignmentStrategy {
    /// Assign to the oldest available node (FIFO)
    /// Deterministic, easy to reason about, good baseline
    #[default]
    EarliestNode,

    /// Assign to the node with lowest current utilization
    /// Balances load in real-time
    LeastLoaded,

    /// Prefer nodes with model already loaded, then least loaded
    /// Best of both: warm cache + load balancing (recommended for production)
    WarmLeastLoaded,

    /// Randomly select from available nodes
    /// No coordination needed, statistically even distribution
    Random,
}

/// Workload information for assignment decisions
#[derive(Debug, Clone)]
pub struct Workload {
    /// Model ID being served
    pub model_id: String,

    /// Estimated memory requirement (MB)
    pub memory_required_mb: f64,

    /// Number of active requests
    pub active_requests: u32,
}

impl Workload {
    /// Create a new workload
    pub fn new(model_id: impl Into<String>, memory_required_mb: f64) -> Self {
        Self {
            model_id: model_id.into(),
            memory_required_mb,
            active_requests: 0,
        }
    }

    /// Set active request count
    pub fn with_active_requests(mut self, count: u32) -> Self {
        self.active_requests = count;
        self
    }
}

/// Candidate instance for assignment with additional metadata
#[derive(Debug, Clone)]
pub struct AssignmentCandidate<'a> {
    /// Reference to the EC2 instance
    pub instance: &'a Ec2Instance,

    /// Current number of active requests on this instance
    pub active_requests: u32,

    /// Models currently loaded on this instance
    pub loaded_models: HashSet<String>,
}

impl<'a> AssignmentCandidate<'a> {
    /// Create a candidate from an EC2 instance
    pub fn new(instance: &'a Ec2Instance) -> Self {
        Self {
            instance,
            active_requests: 0,
            loaded_models: HashSet::new(),
        }
    }

    /// Set the number of active requests
    pub fn with_active_requests(mut self, count: u32) -> Self {
        self.active_requests = count;
        self
    }

    /// Add a loaded model
    pub fn with_loaded_model(mut self, model_id: impl Into<String>) -> Self {
        self.loaded_models.insert(model_id.into());
        self
    }

    /// Set all loaded models
    pub fn with_loaded_models(mut self, models: HashSet<String>) -> Self {
        self.loaded_models = models;
        self
    }

    /// Check if this candidate has the required model loaded
    pub fn has_model(&self, model_id: &str) -> bool {
        self.loaded_models.contains(model_id)
    }

    /// Check if this candidate can fit the workload memory
    pub fn can_fit_memory(&self, required_mb: f64) -> bool {
        self.instance.can_fit_memory(required_mb)
    }
}

/// Node assigner that selects the best instance for a workload
pub struct NodeAssigner {
    strategy: AssignmentStrategy,
}

impl NodeAssigner {
    /// Create a new assigner with the specified strategy
    pub fn new(strategy: AssignmentStrategy) -> Self {
        Self { strategy }
    }

    /// Create an assigner with EarliestNode strategy (default)
    pub fn earliest_node() -> Self {
        Self::new(AssignmentStrategy::EarliestNode)
    }

    /// Create an assigner with LeastLoaded strategy
    pub fn least_loaded() -> Self {
        Self::new(AssignmentStrategy::LeastLoaded)
    }

    /// Create an assigner with WarmLeastLoaded strategy
    pub fn warm_least_loaded() -> Self {
        Self::new(AssignmentStrategy::WarmLeastLoaded)
    }

    /// Get the current strategy
    pub fn strategy(&self) -> AssignmentStrategy {
        self.strategy
    }

    /// Select the best candidate for a workload
    ///
    /// Returns `None` if no suitable candidate is available.
    pub fn select<'a>(
        &self,
        candidates: &[AssignmentCandidate<'a>],
        workload: &Workload,
    ) -> Option<&'a Ec2Instance> {
        // Filter candidates that can fit the workload
        let viable: Vec<_> = candidates
            .iter()
            .filter(|c| c.can_fit_memory(workload.memory_required_mb))
            .collect();

        if viable.is_empty() {
            return None;
        }

        match self.strategy {
            AssignmentStrategy::EarliestNode => self.select_earliest(&viable),
            AssignmentStrategy::LeastLoaded => self.select_least_loaded(&viable),
            AssignmentStrategy::WarmLeastLoaded => {
                self.select_warm_least_loaded(&viable, &workload.model_id)
            }
            AssignmentStrategy::Random => self.select_random(&viable),
        }
    }

    /// Select oldest node (FIFO)
    fn select_earliest<'a>(
        &self,
        candidates: &[&AssignmentCandidate<'a>],
    ) -> Option<&'a Ec2Instance> {
        candidates
            .iter()
            .min_by_key(|c| c.instance.launch_time)
            .map(|c| c.instance)
    }

    /// Select node with lowest current load
    fn select_least_loaded<'a>(
        &self,
        candidates: &[&AssignmentCandidate<'a>],
    ) -> Option<&'a Ec2Instance> {
        candidates
            .iter()
            .min_by_key(|c| c.active_requests)
            .map(|c| c.instance)
    }

    /// Select warm node with lowest load, fallback to least loaded
    fn select_warm_least_loaded<'a>(
        &self,
        candidates: &[&AssignmentCandidate<'a>],
        model_id: &str,
    ) -> Option<&'a Ec2Instance> {
        // First, find candidates with the model already loaded
        let warm: Vec<_> = candidates
            .iter()
            .filter(|c| c.has_model(model_id))
            .copied()
            .collect();

        if warm.is_empty() {
            // No warm candidates, fall back to least loaded
            self.select_least_loaded(candidates)
        } else {
            // Among warm candidates, select least loaded
            self.select_least_loaded(&warm)
        }
    }

    /// Select random node
    fn select_random<'a>(
        &self,
        candidates: &[&AssignmentCandidate<'a>],
    ) -> Option<&'a Ec2Instance> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        use std::time::SystemTime;

        if candidates.is_empty() {
            return None;
        }

        // Simple pseudo-random selection using current time
        let mut hasher = DefaultHasher::new();
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .hash(&mut hasher);
        let hash = hasher.finish();

        let index = (hash as usize) % candidates.len();
        Some(candidates[index].instance)
    }
}

impl Default for NodeAssigner {
    fn default() -> Self {
        Self::earliest_node()
    }
}

/// Result of an assignment operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssignmentResult {
    /// Selected instance ID
    pub instance_id: String,

    /// Strategy used for selection
    pub strategy: AssignmentStrategy,

    /// Whether the selected instance has the model warm
    pub is_warm: bool,

    /// Number of candidates considered
    pub candidates_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::collections::HashMap;

    fn create_test_instance(id: &str, launch_offset_secs: i64) -> Ec2Instance {
        use crate::instance::InstanceState;

        let launch_time = Utc.timestamp_opt(1700000000 + launch_offset_secs, 0).unwrap();

        Ec2Instance {
            id: id.to_string(),
            instance_type: "g5.xlarge".to_string(),
            state: InstanceState::Running,
            public_ip: None,
            private_ip: Some("10.0.0.1".to_string()),
            launch_time,
            gpu_memory_gb: 24.0,
            network_bandwidth_gbps: 10.0,
            gpu_memory_used_mb: 0.0,
            tags: HashMap::new(),
        }
    }

    #[test]
    fn test_earliest_node_selection() {
        let instance1 = create_test_instance("i-oldest", 0);
        let instance2 = create_test_instance("i-newest", 1000);

        let candidates = vec![
            AssignmentCandidate::new(&instance2), // Newer first
            AssignmentCandidate::new(&instance1), // Older second
        ];

        let assigner = NodeAssigner::earliest_node();
        let workload = Workload::new("llama-7b", 8000.0);

        let selected = assigner.select(&candidates, &workload);

        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, "i-oldest"); // Should select oldest
    }

    #[test]
    fn test_least_loaded_selection() {
        let instance1 = create_test_instance("i-busy", 0);
        let instance2 = create_test_instance("i-idle", 100);

        let candidates = vec![
            AssignmentCandidate::new(&instance1).with_active_requests(10),
            AssignmentCandidate::new(&instance2).with_active_requests(2),
        ];

        let assigner = NodeAssigner::least_loaded();
        let workload = Workload::new("llama-7b", 8000.0);

        let selected = assigner.select(&candidates, &workload);

        assert!(selected.is_some());
        assert_eq!(selected.unwrap().id, "i-idle"); // Should select least loaded
    }

    #[test]
    fn test_warm_least_loaded_selection() {
        let instance1 = create_test_instance("i-cold", 0);
        let instance2 = create_test_instance("i-warm", 100);
        let instance3 = create_test_instance("i-warm-busy", 200);

        let candidates = vec![
            AssignmentCandidate::new(&instance1).with_active_requests(0),
            AssignmentCandidate::new(&instance2)
                .with_active_requests(2)
                .with_loaded_model("llama-7b"),
            AssignmentCandidate::new(&instance3)
                .with_active_requests(5)
                .with_loaded_model("llama-7b"),
        ];

        let assigner = NodeAssigner::warm_least_loaded();
        let workload = Workload::new("llama-7b", 8000.0);

        let selected = assigner.select(&candidates, &workload);

        assert!(selected.is_some());
        // Should select warm instance with least load (i-warm has 2 requests)
        assert_eq!(selected.unwrap().id, "i-warm");
    }

    #[test]
    fn test_warm_fallback_to_least_loaded() {
        let instance1 = create_test_instance("i-busy", 0);
        let instance2 = create_test_instance("i-idle", 100);

        // Neither has the model loaded
        let candidates = vec![
            AssignmentCandidate::new(&instance1).with_active_requests(10),
            AssignmentCandidate::new(&instance2).with_active_requests(1),
        ];

        let assigner = NodeAssigner::warm_least_loaded();
        let workload = Workload::new("llama-7b", 8000.0);

        let selected = assigner.select(&candidates, &workload);

        assert!(selected.is_some());
        // No warm instances, should fall back to least loaded
        assert_eq!(selected.unwrap().id, "i-idle");
    }

    #[test]
    fn test_no_viable_candidates() {
        let instance = create_test_instance("i-small", 0);

        // Instance has 24GB, workload needs 30GB
        let candidates = vec![AssignmentCandidate::new(&instance)];

        let assigner = NodeAssigner::earliest_node();
        let workload = Workload::new("llama-70b", 30000.0); // 30GB needed

        let selected = assigner.select(&candidates, &workload);

        assert!(selected.is_none()); // No instance can fit the workload
    }

    #[test]
    fn test_assignment_strategy_serialization() {
        let strategy = AssignmentStrategy::WarmLeastLoaded;
        let json = serde_json::to_string(&strategy).unwrap();
        assert_eq!(json, "\"WarmLeastLoaded\"");

        let parsed: AssignmentStrategy = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, AssignmentStrategy::WarmLeastLoaded);
    }

    #[test]
    fn test_workload_builder() {
        let workload = Workload::new("llama-7b", 8000.0).with_active_requests(5);

        assert_eq!(workload.model_id, "llama-7b");
        assert_eq!(workload.memory_required_mb, 8000.0);
        assert_eq!(workload.active_requests, 5);
    }
}
