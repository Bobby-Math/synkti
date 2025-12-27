//! Scheduling policies for task placement
//!
//! Implements multiple policies to compare:
//! - Greedy: Always use cheapest (spot) instances
//! - OnDemand Fallback: Use spot, fallback to on-demand on preemption
//! - (Future) Uniform Progress: Deadline-aware scheduling from "Can't Be Late" paper

use crate::types::{Instance, InstanceState, InstanceType, Task};

/// Scheduling policy trait
pub trait SchedulingPolicy {
    /// Decide which instance type to launch for a task
    fn select_instance_type(&mut self, task: &Task, spot_price: f64, on_demand_price: f64) -> InstanceType;

    /// Handle preemption event
    fn handle_preemption(&mut self, task: &mut Task, instance: &Instance);

    /// Get policy name
    fn name(&self) -> &str;
}

/// Greedy policy: Always use spot instances (cheapest option)
pub struct GreedyPolicy {
    pub total_preemptions: usize,
}

impl GreedyPolicy {
    pub fn new() -> Self {
        GreedyPolicy {
            total_preemptions: 0,
        }
    }
}

impl SchedulingPolicy for GreedyPolicy {
    fn select_instance_type(&mut self, _task: &Task, _spot_price: f64, _on_demand_price: f64) -> InstanceType {
        // Always choose spot (cheapest)
        InstanceType::Spot
    }

    fn handle_preemption(&mut self, task: &mut Task, _instance: &Instance) {
        self.total_preemptions += 1;
        // Task will be rescheduled on another spot instance
        task.assigned_instance = None;
    }

    fn name(&self) -> &str {
        "Greedy"
    }
}

/// OnDemand Fallback: Use spot, switch to on-demand after preemption
pub struct OnDemandFallbackPolicy {
    pub total_preemptions: usize,
    pub fallback_count: usize,
    /// Track which tasks have been preempted (task_id -> preemption count)
    preempted_tasks: std::collections::HashMap<u64, usize>,
    /// Threshold: fallback to on-demand after N preemptions
    fallback_threshold: usize,
}

impl OnDemandFallbackPolicy {
    pub fn new(fallback_threshold: usize) -> Self {
        OnDemandFallbackPolicy {
            total_preemptions: 0,
            fallback_count: 0,
            preempted_tasks: std::collections::HashMap::new(),
            fallback_threshold,
        }
    }
}

impl SchedulingPolicy for OnDemandFallbackPolicy {
    fn select_instance_type(&mut self, task: &Task, _spot_price: f64, _on_demand_price: f64) -> InstanceType {
        // Check if this task has been preempted too many times
        let preemption_count = self.preempted_tasks.get(&task.id).copied().unwrap_or(0);

        if preemption_count >= self.fallback_threshold {
            // Fallback to on-demand
            self.fallback_count += 1;
            InstanceType::OnDemand
        } else {
            // Try spot first
            InstanceType::Spot
        }
    }

    fn handle_preemption(&mut self, task: &mut Task, _instance: &Instance) {
        self.total_preemptions += 1;

        // Increment preemption count for this task
        *self.preempted_tasks.entry(task.id).or_insert(0) += 1;

        // Task will be rescheduled
        task.assigned_instance = None;
    }

    fn name(&self) -> &str {
        "OnDemandFallback"
    }
}

/// Baseline policy: Only use on-demand instances (no spot)
pub struct OnDemandOnlyPolicy;

impl OnDemandOnlyPolicy {
    pub fn new() -> Self {
        OnDemandOnlyPolicy
    }
}

impl SchedulingPolicy for OnDemandOnlyPolicy {
    fn select_instance_type(&mut self, _task: &Task, _spot_price: f64, _on_demand_price: f64) -> InstanceType {
        InstanceType::OnDemand
    }

    fn handle_preemption(&mut self, _task: &mut Task, _instance: &Instance) {
        // On-demand instances don't get preempted
        panic!("On-demand instance should not be preempted!");
    }

    fn name(&self) -> &str {
        "OnDemandOnly"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_greedy_policy() {
        let mut policy = GreedyPolicy::new();
        let task = Task::new(1, 0.0, 10.0);

        let instance_type = policy.select_instance_type(&task, 0.30, 1.00);
        assert_eq!(instance_type, InstanceType::Spot);
        assert_eq!(policy.total_preemptions, 0);
    }

    #[test]
    fn test_fallback_policy() {
        let mut policy = OnDemandFallbackPolicy::new(2);
        let mut task = Task::new(1, 0.0, 10.0);

        // First attempt: spot
        let t1 = policy.select_instance_type(&task, 0.30, 1.00);
        assert_eq!(t1, InstanceType::Spot);

        // Simulate preemption
        let mut instance = Instance::new(1, InstanceType::Spot, 0.30, 0.0);
        instance.state = InstanceState::Preempted;
        instance.end_time = Some(5.0);
        policy.handle_preemption(&mut task, &instance);

        // Second attempt: still spot (threshold = 2)
        let t2 = policy.select_instance_type(&task, 0.30, 1.00);
        assert_eq!(t2, InstanceType::Spot);

        // Simulate second preemption
        policy.handle_preemption(&mut task, &instance);

        // Third attempt: fallback to on-demand
        let t3 = policy.select_instance_type(&task, 0.30, 1.00);
        assert_eq!(t3, InstanceType::OnDemand);

        assert_eq!(policy.total_preemptions, 2);
        assert_eq!(policy.fallback_count, 1);
    }

    #[test]
    fn test_ondemand_only() {
        let mut policy = OnDemandOnlyPolicy::new();
        let task = Task::new(1, 0.0, 10.0);

        let instance_type = policy.select_instance_type(&task, 0.30, 1.00);
        assert_eq!(instance_type, InstanceType::OnDemand);
    }
}
