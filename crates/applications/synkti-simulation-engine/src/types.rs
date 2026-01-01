//! Core types for the simulation engine

use serde::{Deserialize, Serialize};

/// Instance type (spot or on-demand)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceType {
    Spot,
    OnDemand,
}

/// State of an instance
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InstanceState {
    Running,
    Preempted,
    Terminated,
}

/// A compute instance (spot or on-demand)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: u64,
    pub instance_type: InstanceType,
    pub state: InstanceState,
    pub hourly_cost: f64,
    pub start_time: f64,
    pub end_time: Option<f64>,

    // Resource constraints (for ML inference workloads)
    pub gpu_memory_gb: f64,           // Total GPU memory (e.g., 24 GB for A100)
    pub gpu_memory_used_mb: f64,      // Currently used GPU memory
    pub network_bandwidth_gbps: f64,  // Network bandwidth for migration cost

    // Grace period tracking
    pub preemption_warning_time: Option<f64>,  // When preemption warning was received
}

impl Instance {
    /// Create a new instance with default resource configuration
    pub fn new(
        id: u64,
        instance_type: InstanceType,
        hourly_cost: f64,
        start_time: f64,
    ) -> Self {
        // Default: g5.xlarge equivalent (A100 24GB GPU)
        let (gpu_memory_gb, network_bandwidth_gbps) = match instance_type {
            InstanceType::Spot => (24.0, 10.0),      // A100 with 10 Gbps network
            InstanceType::OnDemand => (24.0, 10.0),
        };

        Instance {
            id,
            instance_type,
            state: InstanceState::Running,
            hourly_cost,
            start_time,
            end_time: None,
            gpu_memory_gb,
            gpu_memory_used_mb: 0.0,
            network_bandwidth_gbps,
            preemption_warning_time: None,
        }
    }

    /// Calculate available GPU memory in MB
    pub fn available_memory_mb(&self) -> f64 {
        (self.gpu_memory_gb * 1000.0) - self.gpu_memory_used_mb
    }

    /// Attempt to assign a task to this instance
    /// Returns true if assignment succeeded, false if task doesn't fit
    pub fn assign_task(&mut self, task: &Task) -> bool {
        if task.can_fit_in_memory(self.available_memory_mb()) {
            self.gpu_memory_used_mb += task.kv_cache_size_mb;
            true
        } else {
            false  // Task doesn't fit
        }
    }

    /// Release a task from this instance, freeing its memory
    pub fn release_task(&mut self, task: &Task) {
        self.gpu_memory_used_mb -= task.kv_cache_size_mb;
        // Prevent negative values due to floating point errors
        self.gpu_memory_used_mb = self.gpu_memory_used_mb.max(0.0);
    }
}

/// Checkpoint state captured during grace period
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointState {
    pub tokens_saved: u64,           // How many tokens were saved
    pub kv_cache_saved_mb: f64,      // KV cache size that was saved
    pub checkpoint_time: f64,         // When checkpoint was taken
    pub transfer_complete: bool,      // Whether transfer completed within grace period
}

/// A task to be executed
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub arrival_time: f64,
    pub duration: f64,        // Time required to complete
    pub remaining_time: f64,  // Time left to complete
    pub assigned_instance: Option<u64>,
    pub start_time: Option<f64>,
    pub completion_time: Option<f64>,

    // Inference-specific fields (for LLM tasks)
    pub tokens_total: u64,             // Total tokens to generate
    pub tokens_completed: u64,         // Tokens completed so far
    pub kv_cache_size_mb: f64,         // KV cache size (state that needs checkpointing)

    // Checkpoint state
    pub checkpoint_state: Option<CheckpointState>,  // Checkpoint data if saved
    pub last_checkpoint_time: Option<f64>,          // When last checkpoint was taken
    pub checkpoint_transfer_time_sec: f64,          // How long checkpoint transfer takes

    // Migration tracking
    pub preemption_count: usize,       // How many times preempted
}

impl Task {
    pub fn new(id: u64, arrival_time: f64, duration: f64) -> Self {
        // Estimate tokens from duration (heuristic: ~100 tokens/hour for 7B model)
        let tokens_total = (duration * 100.0) as u64;

        // Estimate KV cache size (2-8 GB range, depends on context length)
        // Simple linear relationship: duration * 200 MB/hour, max 8000 MB (8 GB)
        let kv_cache_size_mb = (duration * 200.0).min(8000.0);

        Task {
            id,
            arrival_time,
            duration,
            remaining_time: duration,
            assigned_instance: None,
            start_time: None,
            completion_time: None,

            // Initialize inference fields
            tokens_total,
            tokens_completed: 0,
            kv_cache_size_mb,
            checkpoint_state: None,
            last_checkpoint_time: None,
            checkpoint_transfer_time_sec: 0.0,
            preemption_count: 0,
        }
    }

    pub fn is_completed(&self) -> bool {
        self.completion_time.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.assigned_instance.is_some() && self.completion_time.is_none()
    }

    /// Calculate progress percentage based on tokens completed
    pub fn progress_percentage(&self) -> f64 {
        if self.tokens_total == 0 {
            0.0
        } else {
            (self.tokens_completed as f64 / self.tokens_total as f64) * 100.0
        }
    }

    /// Check if this task can fit in available memory
    pub fn can_fit_in_memory(&self, available_memory_mb: f64) -> bool {
        self.kv_cache_size_mb <= available_memory_mb
    }
}

/// Simulation event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    TaskArrival { task_id: u64, time: f64 },
    TaskCompletion { task_id: u64, time: f64 },
    InstancePreemption { instance_id: u64, time: f64 },
    InstanceLaunch { instance_id: u64, time: f64, instance_type: InstanceType },
}

/// Spot price data point
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpotPrice {
    pub time: f64,
    pub price: f64,
    pub preemption_probability: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_creation_with_inference_fields() {
        let task = Task::new(1, 0.0, 10.0);

        // Check basic fields
        assert_eq!(task.id, 1);
        assert_eq!(task.duration, 10.0);

        // Check inference fields are initialized correctly
        assert_eq!(task.tokens_total, 1000);  // 10 hours * 100 tokens/hour
        assert_eq!(task.tokens_completed, 0);
        assert_eq!(task.kv_cache_size_mb, 2000.0);  // 10 hours * 200 MB/hour
        assert_eq!(task.preemption_count, 0);
        assert!(task.checkpoint_state.is_none());
    }

    #[test]
    fn test_task_progress_percentage() {
        let mut task = Task::new(1, 0.0, 10.0);

        // Initially 0%
        assert_eq!(task.progress_percentage(), 0.0);

        // Complete 50%
        task.tokens_completed = 500;
        assert_eq!(task.progress_percentage(), 50.0);

        // Complete 100%
        task.tokens_completed = 1000;
        assert_eq!(task.progress_percentage(), 100.0);
    }

    #[test]
    fn test_instance_memory_constraints() {
        let mut instance = Instance::new(1, InstanceType::Spot, 0.30, 0.0);
        let task = Task::new(1, 0.0, 10.0);

        // Check initial state
        assert_eq!(instance.gpu_memory_gb, 24.0);
        assert_eq!(instance.available_memory_mb(), 24000.0);

        // Assign task
        let assigned = instance.assign_task(&task);
        assert!(assigned);
        assert_eq!(instance.gpu_memory_used_mb, 2000.0);
        assert_eq!(instance.available_memory_mb(), 22000.0);

        // Release task
        instance.release_task(&task);
        assert_eq!(instance.gpu_memory_used_mb, 0.0);
        assert_eq!(instance.available_memory_mb(), 24000.0);
    }

    #[test]
    fn test_task_too_large_for_instance() {
        let mut instance = Instance::new(1, InstanceType::Spot, 0.30, 0.0);
        let mut huge_task = Task::new(1, 0.0, 100.0);  // Very long task

        // Make task larger than instance capacity
        huge_task.kv_cache_size_mb = 30000.0;  // 30 GB (exceeds 24 GB instance)

        let assigned = instance.assign_task(&huge_task);
        assert!(!assigned);  // Should fail
        assert_eq!(instance.gpu_memory_used_mb, 0.0);  // No memory allocated
    }

    #[test]
    fn test_checkpoint_state() {
        let mut task = Task::new(1, 0.0, 10.0);

        // Initially no checkpoint
        assert!(task.checkpoint_state.is_none());

        // Simulate checkpoint
        task.checkpoint_state = Some(CheckpointState {
            tokens_saved: 500,
            kv_cache_saved_mb: 1000.0,
            checkpoint_time: 5.0,
            transfer_complete: true,
        });

        assert!(task.checkpoint_state.is_some());
        let checkpoint = task.checkpoint_state.as_ref().unwrap();
        assert_eq!(checkpoint.tokens_saved, 500);
        assert_eq!(checkpoint.kv_cache_saved_mb, 1000.0);
        assert_eq!(checkpoint.checkpoint_time, 5.0);
        assert!(checkpoint.transfer_complete);
    }
}
