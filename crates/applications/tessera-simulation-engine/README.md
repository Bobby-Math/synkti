# Tessera Simulation Engine

**Status:** Phase 1 - 40% Complete ✅ (Foundation built, simulator loop pending)

Discrete-event simulator for testing spot instance orchestration policies without real cloud infrastructure.

## Purpose

Validate scheduling policies in a virtual environment before deploying to production:
- Compare cost vs. reliability tradeoffs
- Test fault tolerance under realistic preemption rates
- Generate benchmark data for grant applications
- No cloud costs - runs entirely on CPU

## Current State (Session 1 - 17 NOV 2025)

### ✅ COMPLETED MODULES

**1. Core Types** (`src/types.rs`)
- `Instance` - Cloud instance (spot/on-demand) with state tracking
- `Task` - Work unit with progress tracking
- `Event` - Simulation events (arrivals, completions, preemptions)
- `SpotPrice` - Price point with preemption probability

**2. Spot Price Generator** (`src/spot_data.rs`)
- Ornstein-Uhlenbeck mean-reverting stochastic process
- Realistic price dynamics with volatility and mean reversion
- Daily periodicity (business hours patterns)
- Preemption probability inversely correlated with price
- Both realistic and simple generation modes

**3. Scheduling Policies** (`src/policies.rs`)
- **GreedyPolicy** - Always use spot (cheapest, many preemptions)
- **OnDemandFallbackPolicy** - Spot first, fallback after N failures
- **OnDemandOnlyPolicy** - Baseline (no spot, no preemptions, 3x cost)
- Clean trait-based architecture for adding new policies

### ❌ PENDING WORK (4-6 hours)

**1. Simulator Engine** (`src/simulator.rs` - NOT CREATED YET)

Main discrete-event simulation loop:

```rust
pub struct Simulator {
    current_time: f64,
    event_queue: BinaryHeap<Event>,  // Priority queue by time
    instances: HashMap<u64, Instance>,
    tasks: HashMap<u64, Task>,
    policy: Box<dyn SchedulingPolicy>,
    spot_prices: Vec<SpotPrice>,
}

impl Simulator {
    pub fn new(policy: Box<dyn SchedulingPolicy>, spot_prices: Vec<SpotPrice>) -> Self;
    pub fn add_task(&mut self, task: Task);
    pub fn run(&mut self, duration: f64) -> SimulationResult;

    // Internal methods
    fn process_event(&mut self, event: Event);
    fn handle_task_arrival(&mut self, task_id: u64);
    fn handle_instance_preemption(&mut self, instance_id: u64);
    fn handle_task_completion(&mut self, task_id: u64);
    fn launch_instance(&mut self, instance_type: InstanceType) -> u64;
    fn assign_task_to_instance(&mut self, task_id: u64, instance_id: u64);
    fn update_task_progress(&mut self, dt: f64);
    fn check_for_preemptions(&mut self) -> Vec<u64>;
}
```

**Key algorithms:**
- Event-driven: Process events in chronological order
- Task scheduling: Assign pending tasks to available instances
- Preemption handling: Stochastic based on spot price data
- Progress tracking: Update remaining time for running tasks

**2. Metrics Collection** (`src/metrics.rs` - NOT CREATED YET)

Track simulation results:

```rust
pub struct SimulationMetrics {
    pub policy_name: String,
    pub total_cost: f64,
    pub avg_completion_time: f64,
    pub p50_completion_time: f64,
    pub p99_completion_time: f64,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub total_preemptions: usize,
    pub spot_instances_launched: usize,
    pub ondemand_instances_launched: usize,
    pub events: Vec<Event>,  // For detailed visualization
}

impl SimulationMetrics {
    pub fn from_simulation(simulator: &Simulator) -> Self;
    pub fn to_json(&self) -> Result<String, serde_json::Error>;
    pub fn save_to_file(&self, path: &str) -> Result<(), std::io::Error>;
}
```

**3. CLI Interface** (`src/main.rs` - CURRENTLY HELLO WORLD)

Command-line runner:

```rust
use clap::Parser;  // Add clap dependency

#[derive(Parser)]
struct Args {
    /// Simulation duration in hours
    #[arg(short, long, default_value = "48.0")]
    duration: f64,

    /// Policies to compare (comma-separated)
    #[arg(short, long, default_value = "greedy,fallback,ondemand")]
    policies: String,

    /// Number of tasks to simulate
    #[arg(short, long, default_value = "100")]
    tasks: usize,

    /// Output file for JSON results
    #[arg(short, long, default_value = "simulation_results.json")]
    output: String,

    /// Spot price ($/hr)
    #[arg(long, default_value = "0.30")]
    spot_price: f64,

    /// On-demand price ($/hr)
    #[arg(long, default_value = "1.00")]
    ondemand_price: f64,
}

fn main() {
    let args = Args::parse();

    // Generate synthetic spot prices
    let spot_data = generate_spot_prices(args.duration, args.spot_price, args.ondemand_price);

    // Generate tasks
    let tasks = generate_tasks(args.tasks, args.duration);

    // Run simulation for each policy
    let policies = parse_policies(&args.policies);
    let mut results = Vec::new();

    for policy in policies {
        println!("Running simulation with {} policy...", policy.name());
        let mut simulator = Simulator::new(policy, spot_data.clone());

        for task in &tasks {
            simulator.add_task(task.clone());
        }

        let result = simulator.run(args.duration);
        let metrics = SimulationMetrics::from_simulation(&simulator);

        println!("  Cost: ${:.2}", metrics.total_cost);
        println!("  Avg completion: {:.1}h", metrics.avg_completion_time);
        println!("  Preemptions: {}", metrics.total_preemptions);

        results.push(metrics);
    }

    // Export to JSON
    save_results(&results, &args.output);
    println!("Results saved to {}", args.output);
}
```

## Dependencies to Add

Current `Cargo.toml` has:
```toml
[dependencies]
serde = { workspace = true }
serde_json = "1.0"
rand = "0.8"
rand_distr = "0.4"
```

Need to add for CLI:
```toml
clap = { version = "4.0", features = ["derive"] }
```

## Testing Current State

```bash
cd /home/bobby/spot/tessera/crates/applications/tessera-simulation-engine

# Run tests for completed modules
cargo test

# Expected output:
# - test spot_data::tests::test_simple_generation ... ok
# - test spot_data::tests::test_ou_generation ... ok
# - test policies::tests::test_greedy_policy ... ok
# - test policies::tests::test_fallback_policy ... ok
# - test policies::tests::test_ondemand_only ... ok
```

## Expected Output After Completion

```bash
$ cargo run --release -- --duration 48 --policies greedy,fallback,ondemand --tasks 100

Generating 100 tasks over 48 hours...
Generating spot price data (Ornstein-Uhlenbeck process)...

Running simulation with Greedy policy...
  Cost: $87.30
  Avg completion: 15.2h
  Preemptions: 45

Running simulation with OnDemandFallback policy...
  Cost: $145.60
  Avg completion: 12.8h
  Preemptions: 12

Running simulation with OnDemandOnly policy...
  Cost: $480.00
  Avg completion: 12.0h
  Preemptions: 0

Results saved to simulation_results.json
```

**JSON output format:**
```json
{
  "results": [
    {
      "policy_name": "Greedy",
      "total_cost": 87.30,
      "avg_completion_time": 15.2,
      "p99_completion_time": 28.3,
      "total_preemptions": 45,
      "events": [...]
    },
    ...
  ]
}
```

## Architecture

```
Main Program (main.rs)
    ↓
Generate spot prices (spot_data.rs)
    ↓
Generate tasks (types.rs)
    ↓
For each policy:
    Create Simulator (simulator.rs)
        ↓
    Run discrete-event simulation
        - Process events in chronological order
        - Launch instances based on policy
        - Track task progress
        - Handle preemptions
        ↓
    Collect metrics (metrics.rs)
        ↓
    Export to JSON
```

## Key Algorithms

**Discrete-Event Simulation:**
1. Initialize event queue with task arrivals
2. While time < duration:
   - Pop next event from queue
   - Process event (arrival, completion, preemption)
   - Update system state
   - Generate new events as needed
3. Collect final metrics

**Preemption Model:**
- Each time step: Check spot instances against preemption probability
- If preempted: Move task to pending queue, terminate instance
- Policy decides: Retry spot or fallback to on-demand

**Cost Calculation:**
- For each instance: (end_time - start_time) × hourly_cost
- Total cost: Sum across all instances

## Next Steps (Priority Order)

1. **Create `simulator.rs`** (3-4 hours)
   - Implement discrete-event loop
   - Event processing logic
   - Instance and task management

2. **Create `metrics.rs`** (1-2 hours)
   - Metrics collection from simulator state
   - JSON serialization
   - Statistical calculations (percentiles)

3. **Update `main.rs`** (1-2 hours)
   - CLI argument parsing
   - Task generation
   - Run simulations with each policy
   - Export results

4. **Test & Debug** (1-2 hours)
   - Integration tests
   - Verify cost calculations
   - Check edge cases

**Total time to completion: 4-6 hours**

## Files Created This Session

```
src/
├── types.rs         ✅ Complete (Instance, Task, Event types)
├── spot_data.rs     ✅ Complete (OU process price generator)
├── policies.rs      ✅ Complete (3 scheduling policies)
├── simulator.rs     ❌ Not created (main simulation loop)
├── metrics.rs       ❌ Not created (metrics collection)
└── main.rs          ⚠️  Hello world stub (needs full CLI)
```

## Integration with Parent Project

This simulation is **independent** of:
- Synapse (no GPU operations)
- Axon (no ML inference)
- Cloud providers (all synthetic data)

**Pure CPU Rust** - Can develop and test entirely on local laptop.

**Purpose for Tessera:**
- Validates orchestration algorithms before production
- Generates benchmark data for papers/grants
- Tests scheduling policies under various conditions

## Related Documentation

- Parent project: `/home/bobby/spot/tessera/CLAUDE.md`
- Synapse FFI bridge: `/home/bobby/spot/synapse/CLAUDE.md`
- Research papers: SpotServe, SkyServe, "Can't Be Late"

## Contact & Context

- **Domain:** bobby-math.dev
- **GitHub:** bobby-math
- **Parent project:** Tessera (distributed GPU orchestration)
- **Phase:** Building credibility prototype for grant applications
- **Timeline:** Complete simulation in next session (4-6 hours)
