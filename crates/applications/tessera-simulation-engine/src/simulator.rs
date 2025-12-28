//! Discrete-event simulator for spot instance orchestration
//!
//! Simulates task scheduling, instance management, and preemption handling
//! over a configurable time period to compare scheduling policies.

use std::collections::{BinaryHeap, HashMap};
use std::cmp::Ordering;

use crate::types::{Event, Instance, InstanceState, InstanceType, Task, SpotPrice};
use crate::policies::SchedulingPolicy;
use crate::migration::MigrationPlanner;
use crate::checkpoint::CheckpointPlanner;

use serde::{Deserialize, Serialize};

/// Result of a simulation run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub policy_name: String,
    pub total_cost: f64,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub total_preemptions: usize,
    pub average_completion_time: f64,
    pub p99_completion_time: f64,
    pub checkpoints_attempted: usize,
    pub checkpoints_successful: usize,
    pub total_time_saved_hours: f64,
}

/// Timed event wrapper for priority queue ordering
#[derive(Debug, Clone)]
struct TimedEvent {
    time: f64,
    event: Event,
}

// Priority queue orders by time (earliest first)
impl Ord for TimedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        // Reverse comparison for min-heap (BinaryHeap is max-heap by default)
        other.time.partial_cmp(&self.time).unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for TimedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for TimedEvent {}

impl PartialEq for TimedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

/// Discrete-event simulator
pub struct Simulator {
    current_time: f64,
    event_queue: BinaryHeap<TimedEvent>,
    instances: HashMap<u64, Instance>,
    tasks: HashMap<u64, Task>,
    pending_tasks: Vec<u64>,
    policy: Box<dyn SchedulingPolicy>,
    spot_prices: Vec<SpotPrice>,

    // Configuration
    on_demand_price: f64,

    // ID generators
    next_instance_id: u64,

    // Metrics
    total_cost: f64,
    total_preemptions: usize,
    completed_tasks: Vec<u64>,
    checkpoints_attempted: usize,
    checkpoints_successful: usize,
    total_time_saved_hours: f64,
}

impl Simulator {
    /// Create a new simulator with given policy and spot price data
    pub fn new(
        policy: Box<dyn SchedulingPolicy>,
        spot_prices: Vec<SpotPrice>,
        on_demand_price: f64,
    ) -> Self {
        Simulator {
            current_time: 0.0,
            event_queue: BinaryHeap::new(),
            instances: HashMap::new(),
            tasks: HashMap::new(),
            pending_tasks: Vec::new(),
            policy,
            spot_prices,
            on_demand_price,
            next_instance_id: 0,
            total_cost: 0.0,
            total_preemptions: 0,
            completed_tasks: Vec::new(),
            checkpoints_attempted: 0,
            checkpoints_successful: 0,
            total_time_saved_hours: 0.0,
        }
    }

    /// Add a task to the simulation
    pub fn add_task(&mut self, task: Task) {
        let task_id = task.id;
        let arrival_time = task.arrival_time;

        self.tasks.insert(task_id, task);

        // Schedule arrival event
        self.event_queue.push(TimedEvent {
            time: arrival_time,
            event: Event::TaskArrival { task_id, time: arrival_time },
        });
    }

    /// Run the simulation for the specified duration
    pub fn run(&mut self, duration: f64) -> SimulationResult {
        while let Some(timed_event) = self.event_queue.pop() {
            if timed_event.time > duration {
                break;
            }

            self.current_time = timed_event.time;
            self.process_event(timed_event.event);
        }

        self.collect_results()
    }

    /// Process a single event
    fn process_event(&mut self, event: Event) {
        match event {
            Event::TaskArrival { task_id, .. } => self.handle_task_arrival(task_id),
            Event::TaskCompletion { task_id, .. } => self.handle_task_completion(task_id),
            Event::InstancePreemption { instance_id, .. } => self.handle_preemption(instance_id),
            Event::InstanceLaunch { instance_id, instance_type, .. } => {
                self.handle_instance_launch(instance_id, instance_type)
            },
        }
    }

    /// Handle task arrival
    fn handle_task_arrival(&mut self, task_id: u64) {
        // Add to pending queue
        self.pending_tasks.push(task_id);

        // Try to assign pending tasks
        self.assign_pending_tasks();
    }

    /// Attempt to assign all pending tasks to instances
    fn assign_pending_tasks(&mut self) {
        let mut assigned_tasks = Vec::new();
        let mut tasks_needing_instances = Vec::new();

        // First pass: collect information without holding borrows
        for &task_id in &self.pending_tasks {
            if let Some(task) = self.tasks.get(&task_id) {
                // Find an instance with available memory
                let instance_id = self.find_available_instance(task);

                if instance_id.is_some() {
                    assigned_tasks.push((task_id, instance_id.unwrap()));
                } else {
                    // No available instance, need to launch one
                    tasks_needing_instances.push(task_id);
                }
            }
        }

        // Second pass: perform assignments
        for (task_id, inst_id) in assigned_tasks.iter() {
            if let Some(task) = self.tasks.get_mut(task_id) {
                if let Some(instance) = self.instances.get_mut(inst_id) {
                    if instance.assign_task(task) {
                        task.assigned_instance = Some(*inst_id);
                        task.start_time = Some(self.current_time);

                        // Schedule completion event
                        let completion_time = self.current_time + task.remaining_time;
                        self.event_queue.push(TimedEvent {
                            time: completion_time,
                            event: Event::TaskCompletion {
                                task_id: *task_id,
                                time: completion_time,
                            },
                        });
                    }
                }
            }
        }

        // Third pass: launch instances for tasks that need them
        for task_id in tasks_needing_instances {
            if let Some(task) = self.tasks.get(&task_id).cloned() {
                self.launch_instance_for_task(&task);
            }
        }

        // Remove assigned tasks from pending queue
        let assigned_ids: Vec<u64> = assigned_tasks.iter().map(|(id, _)| *id).collect();
        self.pending_tasks.retain(|&id| !assigned_ids.contains(&id));
    }

    /// Find an available instance that can fit the task
    fn find_available_instance(&self, task: &Task) -> Option<u64> {
        for (id, instance) in &self.instances {
            if instance.state == InstanceState::Running
                && task.can_fit_in_memory(instance.available_memory_mb()) {
                return Some(*id);
            }
        }
        None
    }

    /// Launch a new instance for a task
    fn launch_instance_for_task(&mut self, task: &Task) {
        let current_spot_price = self.get_spot_price_at(self.current_time);

        // Ask policy which instance type to use
        let instance_type = self.policy.select_instance_type(
            task,
            current_spot_price,
            self.on_demand_price,
        );

        let hourly_cost = match instance_type {
            InstanceType::Spot => current_spot_price,
            InstanceType::OnDemand => self.on_demand_price,
        };

        // Create new instance
        let instance_id = self.next_instance_id;
        self.next_instance_id += 1;

        let instance = Instance::new(instance_id, instance_type, hourly_cost, self.current_time);
        self.instances.insert(instance_id, instance);

        // Schedule instance launch event (immediate)
        self.event_queue.push(TimedEvent {
            time: self.current_time,
            event: Event::InstanceLaunch {
                instance_id,
                time: self.current_time,
                instance_type,
            },
        });

        // Schedule preemption for spot instances (simplified model)
        if instance_type == InstanceType::Spot {
            self.schedule_potential_preemption(instance_id);
        }
    }

    /// Schedule potential preemption for a spot instance
    fn schedule_potential_preemption(&mut self, instance_id: u64) {
        // Simplified: Use average preemption rate from spot prices
        // In reality, this would sample from the preemption probability distribution
        let avg_preemption_rate = 0.05; // 5% per hour baseline

        // Randomly determine if/when preemption occurs
        // For now: simple exponential distribution
        let hours_until_preemption = -f64::ln(rand::random::<f64>()) / avg_preemption_rate;
        let preemption_time = self.current_time + hours_until_preemption;

        self.event_queue.push(TimedEvent {
            time: preemption_time,
            event: Event::InstancePreemption {
                instance_id,
                time: preemption_time,
            },
        });
    }

    /// Handle instance launch
    fn handle_instance_launch(&mut self, _instance_id: u64, _instance_type: InstanceType) {
        // Instance already created in launch_instance_for_task
        // This event is mainly for logging/metrics

        // Try to assign pending tasks now that new instance is available
        self.assign_pending_tasks();
    }

    /// Handle task completion
    fn handle_task_completion(&mut self, task_id: u64) {
        if let Some(task) = self.tasks.get_mut(&task_id) {
            // Skip if already completed
            if task.is_completed() {
                return;
            }

            task.completion_time = Some(self.current_time);

            // Release instance resources
            if let Some(instance_id) = task.assigned_instance {
                if let Some(instance) = self.instances.get_mut(&instance_id) {
                    instance.release_task(task);

                    // Update cost
                    let runtime = self.current_time - task.start_time.unwrap_or(0.0);
                    self.total_cost += instance.hourly_cost * runtime;
                }
            }

            // Mark as completed (only once)
            if !self.completed_tasks.contains(&task_id) {
                self.completed_tasks.push(task_id);
            }
        }
    }

    /// Handle instance preemption
    fn handle_preemption(&mut self, instance_id: u64) {
        if let Some(instance) = self.instances.get(&instance_id).cloned() {
            // Only preempt if it's a running spot instance
            if instance.instance_type != InstanceType::Spot
                || instance.state != InstanceState::Running {
                return;
            }

            // Find all tasks on this instance and reschedule them
            let affected_task_ids: Vec<u64> = self.tasks
                .iter()
                .filter(|(_, t)| t.assigned_instance == Some(instance_id) && !t.is_completed())
                .map(|(id, _)| *id)
                .collect();

            // Plan and execute checkpoints for all affected tasks
            for task_id in &affected_task_ids {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    self.checkpoints_attempted += 1;

                    // Plan checkpoint based on grace period
                    let checkpoint_decision = CheckpointPlanner::plan_checkpoint(task, &instance);

                    // Execute checkpoint
                    CheckpointPlanner::execute_checkpoint(task, &checkpoint_decision, self.current_time);

                    // Track success
                    match checkpoint_decision {
                        crate::checkpoint::CheckpointDecision::FullCheckpoint { .. }
                        | crate::checkpoint::CheckpointDecision::PartialCheckpoint { .. } => {
                            self.checkpoints_successful += 1;
                        }
                        _ => {}
                    }
                }
            }

            // Now update instance state
            if let Some(instance) = self.instances.get_mut(&instance_id) {
                instance.state = InstanceState::Preempted;
                instance.end_time = Some(self.current_time);
            }

            self.total_preemptions += 1;

            // Update task state for all affected tasks
            for task_id in &affected_task_ids {
                if let Some(task) = self.tasks.get_mut(task_id) {
                    task.preemption_count += 1;
                    task.assigned_instance = None;

                    // Notify policy
                    if let Some(instance) = self.instances.get(&instance_id) {
                        self.policy.handle_preemption(task, instance);
                    }
                }
            }

            // Use optimal migration planning (Kuhn-Munkres algorithm)
            self.migrate_tasks_optimally(&affected_task_ids);
        }
    }

    /// Migrate tasks using optimal assignment (Kuhn-Munkres algorithm)
    fn migrate_tasks_optimally(&mut self, displaced_task_ids: &[u64]) {
        if displaced_task_ids.is_empty() {
            return;
        }

        // Collect displaced tasks
        let displaced_tasks: Vec<Task> = displaced_task_ids
            .iter()
            .filter_map(|id| self.tasks.get(id).cloned())
            .collect();

        // Collect available running instances
        let available_instances: Vec<Instance> = self.instances
            .values()
            .filter(|inst| inst.state == InstanceState::Running)
            .cloned()
            .collect();

        // Find optimal migration assignment
        let migration_plan = MigrationPlanner::plan_optimal_migration(
            &displaced_tasks,
            &available_instances
        );

        // Apply the migration plan
        let mut assigned_task_ids = Vec::new();
        for (task_id, instance_id) in migration_plan {
            if let Some(task) = self.tasks.get_mut(&task_id) {
                if let Some(instance) = self.instances.get_mut(&instance_id) {
                    // Apply checkpoint recovery if available
                    let time_saved = CheckpointPlanner::apply_checkpoint_recovery(task);
                    self.total_time_saved_hours += time_saved;

                    if instance.assign_task(task) {
                        task.assigned_instance = Some(instance_id);
                        task.start_time = Some(self.current_time);

                        // Schedule completion event (accounting for checkpoint recovery)
                        let completion_time = self.current_time + task.remaining_time;
                        self.event_queue.push(TimedEvent {
                            time: completion_time,
                            event: Event::TaskCompletion {
                                task_id,
                                time: completion_time,
                            },
                        });

                        assigned_task_ids.push(task_id);
                    }
                }
            }
        }

        // Tasks that couldn't be assigned optimally need new instances
        for task_id in displaced_task_ids {
            if !assigned_task_ids.contains(task_id) {
                // Add to pending queue for instance launch
                self.pending_tasks.push(*task_id);
            }
        }

        // Launch instances for unassigned tasks
        self.assign_pending_tasks();
    }

    /// Get spot price at a specific time
    fn get_spot_price_at(&self, time: f64) -> f64 {
        // Find the spot price entry for this time
        // Assumes spot_prices is sorted by time
        for price in &self.spot_prices {
            if price.time >= time {
                return price.price;
            }
        }

        // Default to last price if beyond range
        self.spot_prices.last()
            .map(|p| p.price)
            .unwrap_or(0.30) // Default fallback
    }

    /// Collect simulation results
    fn collect_results(&self) -> SimulationResult {
        let total_tasks = self.tasks.len();
        let completed_tasks = self.completed_tasks.len();

        // Calculate completion times
        let mut completion_times: Vec<f64> = self.completed_tasks
            .iter()
            .filter_map(|&id| {
                self.tasks.get(&id).and_then(|t| {
                    if let (Some(start), Some(end)) = (t.start_time, t.completion_time) {
                        Some(end - start)
                    } else {
                        None
                    }
                })
            })
            .collect();

        completion_times.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let average_completion_time = if !completion_times.is_empty() {
            completion_times.iter().sum::<f64>() / completion_times.len() as f64
        } else {
            0.0
        };

        let p99_completion_time = if !completion_times.is_empty() {
            let idx = ((completion_times.len() as f64 * 0.99) as usize).min(completion_times.len() - 1);
            completion_times[idx]
        } else {
            0.0
        };

        SimulationResult {
            policy_name: self.policy.name().to_string(),
            total_cost: self.total_cost,
            total_tasks,
            completed_tasks,
            total_preemptions: self.total_preemptions,
            average_completion_time,
            p99_completion_time,
            checkpoints_attempted: self.checkpoints_attempted,
            checkpoints_successful: self.checkpoints_successful,
            total_time_saved_hours: self.total_time_saved_hours,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policies::{GreedyPolicy, OnDemandOnlyPolicy};
    use crate::spot_data::SpotPriceGenerator;

    #[test]
    fn test_simulator_creation() {
        let policy = Box::new(GreedyPolicy::new());
        let spot_prices = SpotPriceGenerator::generate_simple(10.0, 0.30, 0.05);

        let simulator = Simulator::new(policy, spot_prices, 1.00);

        assert_eq!(simulator.current_time, 0.0);
        assert_eq!(simulator.total_cost, 0.0);
        assert_eq!(simulator.completed_tasks.len(), 0);
    }

    #[test]
    fn test_simple_task_completion() {
        let policy = Box::new(OnDemandOnlyPolicy::new());
        let spot_prices = SpotPriceGenerator::generate_simple(10.0, 0.30, 0.05);

        let mut simulator = Simulator::new(policy, spot_prices, 1.00);

        // Add a simple task
        let task = Task::new(1, 0.0, 1.0);  // 1 hour duration
        simulator.add_task(task);

        // Run simulation
        let result = simulator.run(10.0);

        // Should complete 1 task with on-demand only (no preemptions)
        assert_eq!(result.completed_tasks, 1);
        assert_eq!(result.total_preemptions, 0);
        assert!(result.total_cost > 0.0);  // Should have some cost
    }
}
