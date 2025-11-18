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
}

impl Task {
    pub fn new(id: u64, arrival_time: f64, duration: f64) -> Self {
        Task {
            id,
            arrival_time,
            duration,
            remaining_time: duration,
            assigned_instance: None,
            start_time: None,
            completion_time: None,
        }
    }

    pub fn is_completed(&self) -> bool {
        self.completion_time.is_some()
    }

    pub fn is_running(&self) -> bool {
        self.assigned_instance.is_some() && self.completion_time.is_none()
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
