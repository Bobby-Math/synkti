//! Migration orchestration using Kuhn-Munkres algorithm
//!
//! Plans optimal task migration from preempted instances to available instances.
//!
//! ## Algorithm
//!
//! Uses the Kuhn-Munkres bipartite matching algorithm to minimize total migration cost:
//!
//! ```text
//! cost = transfer_time = kv_cache_mb / (bandwidth_gbps × 125)
//!
//! if kv_cache > available_memory: cost = ∞ (infeasible)
//! ```
//!
//! This is adapted from the simulation engine's migration module for real AWS instances.

use crate::instance::{Ec2Instance, InstanceSpec};
use crate::error::{OrchestratorError, Result};
use pathfinding::matrix::Matrix;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task/workload that needs migration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationTask {
    /// Task ID
    pub id: u64,

    /// Container ID
    pub container_id: String,

    /// KV cache size in MB
    pub kv_cache_size_mb: f64,

    /// Model name (for informational purposes)
    pub model: Option<String>,

    /// Number of active requests
    pub active_requests: u32,
}

impl MigrationTask {
    /// Create a new migration task
    pub fn new(id: u64, container_id: impl Into<String>, kv_cache_size_mb: f64) -> Self {
        Self {
            id,
            container_id: container_id.into(),
            kv_cache_size_mb,
            model: None,
            active_requests: 0,
        }
    }

    /// Check if task can fit in available memory
    pub fn can_fit_in_memory(&self, available_mb: f64) -> bool {
        self.kv_cache_size_mb <= available_mb
    }
}

/// Target instance for migration
#[derive(Debug, Clone)]
pub struct MigrationTarget {
    /// Instance ID
    pub instance_id: String,

    /// Available GPU memory in MB
    pub available_memory_mb: f64,

    /// Network bandwidth in Gbps
    pub network_bandwidth_gbps: f64,
}

impl MigrationTarget {
    /// Create from EC2 instance
    pub fn from_instance(instance: &Ec2Instance) -> Self {
        Self {
            instance_id: instance.id.clone(),
            available_memory_mb: instance.available_memory_mb(),
            network_bandwidth_gbps: instance.network_bandwidth_gbps,
        }
    }

    /// Create from instance spec
    pub fn from_spec(spec: &InstanceSpec) -> Self {
        Self {
            instance_id: format!("pending-{}", uuid::Uuid::new_v4()),
            available_memory_mb: spec.available_memory_mb(),
            network_bandwidth_gbps: spec.network_bandwidth_gbps,
        }
    }
}

/// Migration plan from tasks to targets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationPlan {
    /// Task ID -> Instance ID mapping
    pub assignments: HashMap<u64, String>,

    /// Total estimated migration time (seconds)
    pub total_time_seconds: f64,

    /// Number of tasks that couldn't be assigned
    pub unassigned_count: usize,
}

/// Migration planner using Kuhn-Munkres algorithm
pub struct MigrationPlanner;

impl MigrationPlanner {
    /// Calculate migration cost for a single task to a single target
    ///
    /// Cost is based on transfer time: KV cache size / network bandwidth
    ///
    /// # Arguments
    /// - `task`: The task to migrate
    /// - `target`: The target instance
    ///
    /// # Returns
    /// Migration cost in seconds, or f64::INFINITY if infeasible
    pub fn migration_cost(task: &MigrationTask, target: &MigrationTarget) -> f64 {
        // Check memory feasibility first
        if !task.can_fit_in_memory(target.available_memory_mb) {
            return f64::INFINITY;
        }

        // Calculate transfer time
        // network_bandwidth_gbps * 125 = MB/s
        // transfer_time = size_mb / (bandwidth_MB_s)
        let bandwidth_mb_per_sec = target.network_bandwidth_gbps * 125.0;
        let transfer_time_sec = task.kv_cache_size_mb / bandwidth_mb_per_sec;

        transfer_time_sec
    }

    /// Build cost matrix for all task-target pairs
    fn build_cost_matrix(
        tasks: &[MigrationTask],
        targets: &[MigrationTarget],
    ) -> Vec<Vec<f64>> {
        tasks
            .iter()
            .map(|task| {
                targets
                    .iter()
                    .map(|target| Self::migration_cost(task, target))
                    .collect()
            })
            .collect()
    }

    /// Plan optimal migration using Kuhn-Munkres algorithm
    ///
    /// This finds the minimum-cost perfect matching between tasks and targets.
    ///
    /// # Arguments
    /// - `tasks`: Tasks that need migration
    /// - `targets`: Available target instances
    ///
    /// # Returns
    /// Migration plan with optimal assignments
    pub fn plan_optimal_migration(
        tasks: &[MigrationTask],
        targets: &[MigrationTarget],
    ) -> Result<MigrationPlan> {
        if tasks.is_empty() {
            return Ok(MigrationPlan {
                assignments: HashMap::new(),
                total_time_seconds: 0.0,
                unassigned_count: 0,
            });
        }

        if targets.is_empty() {
            return Err(OrchestratorError::NoAvailableInstances);
        }

        // Build cost matrix
        let cost_matrix = Self::build_cost_matrix(tasks, targets);

        // Handle case where we have more tasks than instances
        let num_tasks = tasks.len();
        let num_targets = targets.len();
        let matrix_size = num_tasks.max(num_targets);

        // Create square matrix padded with high costs
        let mut square_matrix = vec![vec![f64::INFINITY; matrix_size]; matrix_size];
        for i in 0..num_tasks {
            for j in 0..num_targets {
                square_matrix[i][j] = cost_matrix[i][j];
            }
        }

        // Convert to integer costs for pathfinding crate
        let int_costs: Vec<i64> = square_matrix
            .iter()
            .flat_map(|row| {
                row.iter().map(|&cost| {
                    if cost.is_infinite() {
                        1_000_000_000
                    } else {
                        (cost * 1000.0) as i64
                    }
                })
            })
            .collect();

        let matrix = Matrix::from_vec(matrix_size, matrix_size, int_costs)
            .map_err(|e| OrchestratorError::Migration(format!("Failed to create cost matrix: {}", e)))?;

        // Run Kuhn-Munkres algorithm
        let (_total_cost, assignment) = pathfinding::kuhn_munkres::kuhn_munkres(&matrix);

        // Convert assignment to task_id -> instance_id map
        let mut assignments = HashMap::new();
        let mut total_time = 0.0;
        let mut unassigned = 0;

        for (task_idx, target_idx) in assignment.iter().enumerate() {
            if task_idx < num_tasks && *target_idx < num_targets {
                let cost = cost_matrix[task_idx][*target_idx];

                if cost < f64::INFINITY {
                    let task_id = tasks[task_idx].id;
                    let instance_id = targets[*target_idx].instance_id.clone();
                    assignments.insert(task_id, instance_id);
                    total_time += cost;
                } else {
                    unassigned += 1;
                }
            } else if task_idx < num_tasks {
                unassigned += 1;
            }
        }

        Ok(MigrationPlan {
            assignments,
            total_time_seconds: total_time,
            unassigned_count: unassigned,
        })
    }

    /// Calculate transfer feasibility based on grace period
    ///
    /// # Arguments
    /// - `tasks`: Tasks to migrate
    /// - `targets`: Target instances
    /// - `grace_period_seconds`: Available time (usually 120s for AWS spot)
    ///
    /// # Returns
    /// True if all tasks can be transferred within grace period
    pub fn can_transfer_in_grace_period(
        tasks: &[MigrationTask],
        targets: &[MigrationTarget],
        grace_period_seconds: f64,
    ) -> bool {
        let plan = match Self::plan_optimal_migration(tasks, targets) {
            Ok(p) => p,
            Err(_) => return false,
        };

        plan.total_time_seconds <= grace_period_seconds
    }

    /// Estimate checkpoint ratio for graceful migration
    ///
    /// Calculates how much of the KV cache can be transferred in the grace period.
    ///
    /// # Returns
    /// Ratio (0.0 - 1.0) of total KV cache that can be transferred
    pub fn checkpoint_ratio(
        tasks: &[MigrationTask],
        targets: &[MigrationTarget],
        grace_period_seconds: f64,
    ) -> f64 {
        if tasks.is_empty() || targets.is_empty() {
            return 0.0;
        }

        let total_kv_mb: f64 = tasks.iter().map(|t| t.kv_cache_size_mb).sum();

        if total_kv_mb == 0.0 {
            return 1.0;
        }

        // Calculate total bandwidth available
        let total_bandwidth_mb_s: f64 = targets
            .iter()
            .map(|t| t.network_bandwidth_gbps * 125.0)
            .sum();

        let transferable_mb = total_bandwidth_mb_s * grace_period_seconds;

        (transferable_mb / total_kv_mb).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_task(id: u64, kv_cache_mb: f64) -> MigrationTask {
        MigrationTask::new(id, format!("container-{}", id), kv_cache_mb)
    }

    fn create_test_target(id: &str, memory_gb: f64, bandwidth_gbps: f64) -> MigrationTarget {
        MigrationTarget {
            instance_id: id.to_string(),
            available_memory_mb: memory_gb * 1024.0,
            network_bandwidth_gbps: bandwidth_gbps,
        }
    }

    #[test]
    fn test_migration_cost_calculation() {
        let task = create_test_task(1, 2000.0); // 2GB KV cache
        let target = create_test_target("i-1", 24.0, 10.0); // 10 Gbps network

        let cost = MigrationPlanner::migration_cost(&task, &target);

        // Expected: 2000 MB / (10 * 125) = 2000 / 1250 = 1.6 seconds
        assert!((cost - 1.6).abs() < 0.01);
    }

    #[test]
    fn test_migration_cost_infeasible() {
        let task = create_test_task(1, 30_000.0); // 30GB KV cache
        let target = create_test_target("i-1", 24.0, 10.0); // Only 24GB available

        let cost = MigrationPlanner::migration_cost(&task, &target);

        assert!(cost.is_infinite());
    }

    #[test]
    fn test_optimal_migration() {
        let tasks = vec![
            create_test_task(1, 1000.0),  // Small task
            create_test_task(2, 4000.0),  // Medium task
        ];

        let targets = vec![
            create_test_target("i-1", 24.0, 10.0),
            create_test_target("i-2", 24.0, 10.0),
        ];

        let plan = MigrationPlanner::plan_optimal_migration(&tasks, &targets).unwrap();

        assert_eq!(plan.assignments.len(), 2);
        assert!(plan.assignments.contains_key(&1));
        assert!(plan.assignments.contains_key(&2));
        assert_eq!(plan.unassigned_count, 0);
    }

    #[test]
    fn test_migration_with_insufficient_targets() {
        let tasks = vec![
            create_test_task(1, 1000.0),
            create_test_task(2, 1000.0),
            create_test_task(3, 1000.0), // 3 tasks
        ];

        let targets = vec![
            create_test_target("i-1", 24.0, 10.0),
            create_test_target("i-2", 24.0, 10.0), // Only 2 targets
        ];

        let plan = MigrationPlanner::plan_optimal_migration(&tasks, &targets).unwrap();

        // Should assign 2 tasks, 1 unassigned
        assert_eq!(plan.assignments.len(), 2);
        assert_eq!(plan.unassigned_count, 1);
    }

    #[test]
    fn test_can_transfer_in_grace_period() {
        let tasks = vec![create_test_task(1, 2000.0)]; // 2GB, ~1.6s at 10Gbps
        let targets = vec![create_test_target("i-1", 24.0, 10.0)];

        assert!(MigrationPlanner::can_transfer_in_grace_period(&tasks, &targets, 120.0));
    }

    #[test]
    fn test_checkpoint_ratio() {
        let tasks = vec![create_test_task(1, 15_000.0)]; // 15GB
        let targets = vec![create_test_target("i-1", 24.0, 1.0)]; // 1Gbps = 125MB/s

        // At 1Gbps, can transfer 125 MB/s * 120s = 15GB in grace period
        // Ratio should be ~1.0
        let ratio = MigrationPlanner::checkpoint_ratio(&tasks, &targets, 120.0);
        assert!(ratio >= 0.99 && ratio <= 1.01);
    }

    #[test]
    fn test_no_available_instances() {
        let tasks = vec![create_test_task(1, 1000.0)];
        let targets = vec![];

        let result = MigrationPlanner::plan_optimal_migration(&tasks, &targets);
        assert!(matches!(result, Err(OrchestratorError::NoAvailableInstances)));
    }
}
