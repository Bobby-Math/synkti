//! Optimal task migration planner using Kuhn-Munkres algorithm
//!
//! When spot instances are preempted, we need to migrate running tasks to other instances.
//! This module implements optimal assignment to minimize total migration cost.

use crate::types::{Instance, InstanceType, Task};
use pathfinding::matrix::Matrix;
use std::collections::HashMap;

/// Plans optimal task-to-instance migration using the Kuhn-Munkres algorithm
pub struct MigrationPlanner;

impl MigrationPlanner {
    /// Calculate migration cost for a single task to a single instance
    ///
    /// Cost is based on:
    /// - Transfer time: KV cache size / network bandwidth
    /// - Memory feasibility: INFINITY if task doesn't fit
    ///
    /// # Arguments
    /// - `task`: The task to migrate
    /// - `instance`: The target instance
    ///
    /// # Returns
    /// Migration cost in seconds, or f64::INFINITY if infeasible
    fn migration_cost(task: &Task, instance: &Instance) -> f64 {
        // Check memory feasibility first
        let available_memory = instance.available_memory_mb();
        if !task.can_fit_in_memory(available_memory) {
            return f64::INFINITY; // Infeasible assignment
        }

        // Calculate transfer time
        // network_bandwidth_gbps * 1000 / 8 = MB/s
        // transfer_time = size_mb / (bandwidth_MB_s)
        let bandwidth_mb_per_sec = instance.network_bandwidth_gbps * 125.0; // Gbps to MB/s
        let transfer_time_sec = task.kv_cache_size_mb / bandwidth_mb_per_sec;

        transfer_time_sec
    }

    /// Build cost matrix for all task-instance pairs
    ///
    /// # Arguments
    /// - `tasks`: Tasks that need migration
    /// - `instances`: Available instances
    ///
    /// # Returns
    /// 2D cost matrix where cost[i][j] = cost of assigning task i to instance j
    fn build_cost_matrix(tasks: &[Task], instances: &[Instance]) -> Vec<Vec<f64>> {
        tasks
            .iter()
            .map(|task| {
                instances
                    .iter()
                    .map(|instance| Self::migration_cost(task, instance))
                    .collect()
            })
            .collect()
    }

    /// Plan optimal migration using Kuhn-Munkres algorithm
    ///
    /// This finds the minimum-cost perfect matching between tasks and instances.
    ///
    /// # Arguments
    /// - `displaced_tasks`: Tasks that need to be migrated
    /// - `available_instances`: Instances that can receive tasks
    ///
    /// # Returns
    /// HashMap mapping task_id -> instance_id for optimal assignment
    ///
    /// # Algorithm
    /// 1. Build cost matrix (transfer time + memory feasibility)
    /// 2. Run Kuhn-Munkres algorithm to find minimum-cost matching
    /// 3. Return assignment as task_id -> instance_id map
    ///
    /// # Notes
    /// - If there are more tasks than instances, some tasks won't be assigned
    /// - If a task can't fit on any instance, it gets INFINITY cost and won't be assigned
    pub fn plan_optimal_migration(
        displaced_tasks: &[Task],
        available_instances: &[Instance],
    ) -> HashMap<u64, u64> {
        if displaced_tasks.is_empty() || available_instances.is_empty() {
            return HashMap::new();
        }

        // Build cost matrix
        let cost_matrix = Self::build_cost_matrix(displaced_tasks, available_instances);

        // Handle case where we have more tasks than instances
        // We need a square matrix for KM algorithm, so we'll pad with dummy instances
        let num_tasks = displaced_tasks.len();
        let num_instances = available_instances.len();
        let matrix_size = num_tasks.max(num_instances);

        // Create square matrix padded with high costs
        let mut square_matrix = vec![vec![f64::INFINITY; matrix_size]; matrix_size];
        for i in 0..num_tasks {
            for j in 0..num_instances {
                square_matrix[i][j] = cost_matrix[i][j];
            }
        }

        // Convert to integer costs for pathfinding crate (multiply by 1000 for precision)
        let int_costs: Vec<i64> = square_matrix
            .iter()
            .flat_map(|row| {
                row.iter().map(|&cost| {
                    if cost.is_infinite() {
                        1_000_000_000 // Use large value for infeasible (1 billion, much larger than realistic costs)
                    } else {
                        (cost * 1000.0) as i64 // Preserve 3 decimal places
                    }
                })
            })
            .collect();

        // Create Matrix from flattened costs
        let matrix = Matrix::from_vec(matrix_size, matrix_size, int_costs).unwrap();

        // Run Kuhn-Munkres algorithm
        let (_total_cost, assignment) = pathfinding::kuhn_munkres::kuhn_munkres(&matrix);

        // Convert assignment to task_id -> instance_id map
        let mut migration_plan = HashMap::new();
        for (task_idx, instance_idx) in assignment.iter().enumerate() {
            // Only include valid assignments (not to dummy instances and not infinite cost)
            if task_idx < num_tasks
                && *instance_idx < num_instances
                && cost_matrix[task_idx][*instance_idx] < f64::INFINITY
            {
                let task_id = displaced_tasks[task_idx].id;
                let instance_id = available_instances[*instance_idx].id;
                migration_plan.insert(task_id, instance_id);
            }
        }

        migration_plan
    }

    /// Plan naive greedy migration (baseline for comparison)
    ///
    /// Uses simple first-fit algorithm: for each task, assign to first instance with enough memory.
    /// This is the baseline strategy that optimal KM migration improves upon.
    ///
    /// # Arguments
    /// - `displaced_tasks`: Tasks that need to be migrated
    /// - `available_instances`: Instances that can receive tasks
    ///
    /// # Returns
    /// HashMap mapping task_id -> instance_id for naive assignment
    ///
    /// # Algorithm
    /// 1. For each task, iterate through instances in order
    /// 2. Assign to first instance that has enough memory
    /// 3. Track memory usage to prevent over-allocation
    ///
    /// # Notes
    /// - Much simpler than KM algorithm, but suboptimal
    /// - Does not minimize transfer time, just finds first feasible assignment
    pub fn plan_naive_migration(
        displaced_tasks: &[Task],
        available_instances: &[Instance],
    ) -> HashMap<u64, u64> {
        if displaced_tasks.is_empty() || available_instances.is_empty() {
            return HashMap::new();
        }

        let mut assignment = HashMap::new();

        // Track memory usage per instance (instance_id -> used_memory_mb)
        let mut instance_memory_used: HashMap<u64, f64> = available_instances
            .iter()
            .map(|inst| (inst.id, inst.gpu_memory_used_mb))
            .collect();

        // For each task, find first instance that can fit it
        for task in displaced_tasks {
            for instance in available_instances {
                let current_used = instance_memory_used.get(&instance.id).unwrap_or(&0.0);
                let available = (instance.gpu_memory_gb * 1024.0) - current_used;

                // Check if task fits in available memory
                if task.can_fit_in_memory(available) {
                    // Assign task to this instance
                    assignment.insert(task.id, instance.id);

                    // Update memory usage tracking
                    *instance_memory_used.get_mut(&instance.id).unwrap() += task.kv_cache_size_mb;

                    // Move to next task
                    break;
                }
            }
            // If no instance found, task remains unassigned
        }

        assignment
    }

    /// Calculate total migration cost for a given assignment
    ///
    /// Useful for comparing greedy vs optimal strategies
    pub fn calculate_total_cost(
        tasks: &[Task],
        instances: &[Instance],
        assignment: &HashMap<u64, u64>,
    ) -> f64 {
        let mut total_cost = 0.0;

        for (task_id, instance_id) in assignment {
            // Find the task and instance
            let task = tasks.iter().find(|t| t.id == *task_id);
            let instance = instances.iter().find(|i| i.id == *instance_id);

            if let (Some(task), Some(instance)) = (task, instance) {
                let cost = Self::migration_cost(task, instance);
                if !cost.is_infinite() {
                    total_cost += cost;
                }
            }
        }

        total_cost
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_migration_cost_calculation() {
        let task = Task::new(1, 0.0, 10.0); // 10 hour task -> ~2 GB KV cache
        let instance = Instance::new(100, InstanceType::Spot, 0.30, 0.0); // 10 Gbps network

        let cost = MigrationPlanner::migration_cost(&task, &instance);

        // Expected: 2000 MB / (10 Gbps * 125 MB/s) = 2000 / 1250 = 1.6 seconds
        assert!((cost - 1.6).abs() < 0.01, "Cost should be ~1.6 seconds");
    }

    #[test]
    fn test_migration_cost_memory_infeasible() {
        // Create a task with huge KV cache (100 GB)
        let mut task = Task::new(1, 0.0, 10.0);
        task.kv_cache_size_mb = 100_000.0; // 100 GB

        let instance = Instance::new(100, InstanceType::Spot, 0.30, 0.0); // Only 24 GB GPU memory

        let cost = MigrationPlanner::migration_cost(&task, &instance);

        assert!(cost.is_infinite(), "Should be infeasible due to memory");
    }

    #[test]
    fn test_optimal_migration_small_example() {
        // Create 2 tasks with different KV cache sizes
        let task1 = Task::new(1, 0.0, 5.0); // Small task (1 GB cache)
        let task2 = Task::new(2, 0.0, 20.0); // Large task (4 GB cache)

        // Create 2 instances with different bandwidth
        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0); // Fast network (10 Gbps)
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0); // Same network

        let tasks = vec![task1.clone(), task2.clone()];
        let instances = vec![instance1.clone(), instance2.clone()];

        let assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);

        // Should assign both tasks
        assert_eq!(assignment.len(), 2, "Should assign both tasks");
        assert!(assignment.contains_key(&1), "Task 1 should be assigned");
        assert!(assignment.contains_key(&2), "Task 2 should be assigned");
    }

    #[test]
    fn test_optimal_migration_with_memory_constraints() {
        // Create 2 tasks that fit
        let task1 = Task::new(1, 0.0, 5.0); // 1 GB cache
        let task2 = Task::new(2, 0.0, 10.0); // 2 GB cache

        // Create 2 instances (24 GB each)
        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0);
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0);

        let tasks = vec![task1, task2];
        let instances = vec![instance1, instance2];

        let assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);

        // Should assign both tasks
        assert_eq!(assignment.len(), 2, "Should assign both tasks");
        assert!(assignment.contains_key(&1), "Task 1 should be assigned");
        assert!(assignment.contains_key(&2), "Task 2 should be assigned");
    }

    #[test]
    fn test_migration_filters_too_large_tasks() {
        // Create task that's too large for any instance
        let mut large_task = Task::new(999, 0.0, 50.0);
        large_task.kv_cache_size_mb = 30_000.0; // 30 GB - exceeds 24 GB GPU memory

        let instance = Instance::new(100, InstanceType::Spot, 0.30, 0.0);

        let tasks = vec![large_task];
        let instances = vec![instance];

        let assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);

        // Should not assign the too-large task
        assert_eq!(assignment.len(), 0, "Should not assign task that's too large");
    }

    #[test]
    fn test_empty_inputs() {
        let tasks = vec![];
        let instances = vec![Instance::new(100, InstanceType::Spot, 0.30, 0.0)];
        let assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);
        assert!(assignment.is_empty());

        let tasks = vec![Task::new(1, 0.0, 10.0)];
        let instances = vec![];
        let assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);
        assert!(assignment.is_empty());
    }

    #[test]
    fn test_calculate_total_cost() {
        let task1 = Task::new(1, 0.0, 5.0); // 1 GB cache
        let task2 = Task::new(2, 0.0, 10.0); // 2 GB cache

        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0);
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0);

        let tasks = vec![task1.clone(), task2.clone()];
        let instances = vec![instance1.clone(), instance2.clone()];

        let mut assignment = HashMap::new();
        assignment.insert(1, 100);
        assignment.insert(2, 101);

        let total_cost = MigrationPlanner::calculate_total_cost(&tasks, &instances, &assignment);

        // Cost1: 1000 MB / 1250 MB/s = 0.8s
        // Cost2: 2000 MB / 1250 MB/s = 1.6s
        // Total: 2.4s
        assert!(
            (total_cost - 2.4).abs() < 0.01,
            "Total cost should be ~2.4 seconds"
        );
    }

    #[test]
    fn test_naive_migration_simple() {
        let task1 = Task::new(1, 0.0, 5.0); // 1 GB cache
        let task2 = Task::new(2, 0.0, 10.0); // 2 GB cache

        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0);
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0);

        let tasks = vec![task1, task2];
        let instances = vec![instance1, instance2];

        let assignment = MigrationPlanner::plan_naive_migration(&tasks, &instances);

        // Should assign both tasks
        assert_eq!(assignment.len(), 2, "Should assign both tasks");
        assert!(assignment.contains_key(&1), "Task 1 should be assigned");
        assert!(assignment.contains_key(&2), "Task 2 should be assigned");
    }

    #[test]
    fn test_naive_migration_memory_constraint() {
        // Create tasks that together exceed one instance's memory
        let task1 = Task::new(1, 0.0, 50.0); // ~10 GB cache
        let task2 = Task::new(2, 0.0, 50.0); // ~10 GB cache
        let task3 = Task::new(3, 0.0, 50.0); // ~10 GB cache

        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0); // 24 GB GPU
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0); // 24 GB GPU

        let tasks = vec![task1, task2, task3];
        let instances = vec![instance1, instance2];

        let assignment = MigrationPlanner::plan_naive_migration(&tasks, &instances);

        // Should assign all 3 tasks (2 on first instance, 1 on second)
        assert_eq!(assignment.len(), 3, "Should assign all 3 tasks");
    }

    #[test]
    fn test_naive_vs_optimal_comparison() {
        // Create scenario where optimal is better than naive
        // 2 tasks with different sizes, 2 instances with same bandwidth
        let task1 = Task::new(1, 0.0, 5.0); // Small: 1 GB cache
        let task2 = Task::new(2, 0.0, 40.0); // Large: 8 GB cache

        let instance1 = Instance::new(100, InstanceType::Spot, 0.30, 0.0);
        let instance2 = Instance::new(101, InstanceType::Spot, 0.30, 0.0);

        let tasks = vec![task1.clone(), task2.clone()];
        let instances = vec![instance1.clone(), instance2.clone()];

        // Naive: assigns in order (task1->inst1, task2->inst2)
        let naive_assignment = MigrationPlanner::plan_naive_migration(&tasks, &instances);

        // Optimal: KM finds best assignment
        let optimal_assignment = MigrationPlanner::plan_optimal_migration(&tasks, &instances);

        // Both should assign both tasks
        assert_eq!(naive_assignment.len(), 2, "Naive should assign both");
        assert_eq!(optimal_assignment.len(), 2, "Optimal should assign both");

        // Calculate costs
        let naive_cost = MigrationPlanner::calculate_total_cost(&tasks, &instances, &naive_assignment);
        let optimal_cost = MigrationPlanner::calculate_total_cost(&tasks, &instances, &optimal_assignment);

        // In this symmetric case, costs should be equal (both instances identical)
        // But we're testing that the functions work correctly
        assert!(naive_cost > 0.0, "Naive cost should be positive");
        assert!(optimal_cost > 0.0, "Optimal cost should be positive");
        assert!(
            (naive_cost - optimal_cost).abs() < 0.1,
            "With identical instances, costs should be similar"
        );
    }

    #[test]
    fn test_naive_migration_empty_inputs() {
        let tasks = vec![];
        let instances = vec![Instance::new(100, InstanceType::Spot, 0.30, 0.0)];
        let assignment = MigrationPlanner::plan_naive_migration(&tasks, &instances);
        assert!(assignment.is_empty());

        let tasks = vec![Task::new(1, 0.0, 10.0)];
        let instances = vec![];
        let assignment = MigrationPlanner::plan_naive_migration(&tasks, &instances);
        assert!(assignment.is_empty());
    }
}
