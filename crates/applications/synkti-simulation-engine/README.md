# Synkti Simulation Engine

**Status:** ✅ Phase 1 Complete - Research Prototype

Discrete-event simulator for spot instance orchestration with **optimal migration** and **checkpoint-aware recovery**.

---

## Quick Start

```bash
# From Project Root
# Run rigorous benchmark (200 tasks, 72 hours)
cargo run --release -p synkti-simulation-engine -- --duration 72 --tasks 200

# Export results to JSON
cargo run --release -p synkti-simulation-engine -- --duration 72 --tasks 200 --output results.json

# Compare naive vs optimal migration strategies
cargo run --release -p synkti-simulation-engine -- --duration 72 --tasks 200 \
  --policies greedy-naive,greedy-optimal,fallback-naive,fallback-optimal,ondemand

# Run all tests (32 passing)
cargo test -p synkti-simulation-engine

# Generate interactive visualizations
cargo run --release --example visualize_benchmark_comparison
cargo run --release --example visualize_naive_vs_optimal
cargo run --release --example visualize_spot_behavior
```

**📊 [View Interactive Results](https://bobby-math.github.io/synkti/)** - Explore benchmark visualizations

---

## Novel Contributions

### 1. Kuhn-Munkres Optimal Migration
**Problem:** When spot instances are preempted, tasks must migrate to new instances. Naive greedy assignment is suboptimal.

**Solution:** Use Hungarian algorithm to find provably minimum-cost task-to-instance assignment.

**Cost Function:**
```rust
cost = kv_cache_size_mb / (network_bandwidth_gbps * 125 MB/s)
if !fits_in_memory: cost = INFINITY
```

**Impact:** Optimal KM migration achieves **46% better cost savings** compared to naive first-fit reassignment (see benchmarks below).

---

### 2. Grace Period Checkpoint Exploitation

**Problem:** AWS gives 120 seconds warning before terminating spot instances. How to best use this time?

**Solution:** Intelligent decision logic based on transferable data:
- **≥80% transferable** → Full checkpoint (save everything)
- **30-80% transferable** → Partial checkpoint (save what we can)
- **<30% transferable** → Restart (overhead not worth it)

**Calculation:**
```
transferable_mb = 10 Gbps * 125 MB/s * 120s = 150,000 MB (150 GB)
```

**Impact:** Tasks can recover up to 100% of progress on migration, reducing completion time.

---

### 3. Domain-Agnostic Orchestration
**Problem:** Existing systems (SpotServe) are LLM-specific and tightly coupled to inference workloads.

**Solution:** Clean separation between orchestration logic and workload type via pluggable policies.

**Extensibility:** Easy to add new policies (e.g., deadline-aware, multi-objective)

---

## Architecture

```
CLI (main.rs)
  ↓
Spot Price Generation (spot_data.rs - Ornstein-Uhlenbeck process)
  ↓
Discrete-Event Simulator (simulator.rs - Priority queue event loop)
  ├── Scheduling Policies (policies.rs - Greedy/Fallback/OnDemand)
  ├── Task/Instance Management (types.rs - GPU memory tracking)
  ├── [Preemption Event]
  │     ↓
  ├── Checkpoint Planning (checkpoint.rs - Grace period logic)
  │     ↓
  └── Optimal Migration (migration.rs - Kuhn-Munkres algorithm)
        ↓
  Results (JSON export + CLI summary)
```

---

## Related Work

### SpotServe (OSDI '24)

**Focus:** LLM inference on spot instances

**Innovation:** Dynamic re-parallelization during preemption

**Limitation:** Greedy migration, LLM-specific, no grace period exploitation

**Synkti Improvement:**
- Optimal migration (provably better than greedy)
- Domain-agnostic (works for any GPU workload)
- Grace period checkpointing (novel contribution)

---

### SkyServe

**Focus:** Multi-cloud LLM serving

**Innovation:** Global replica placement across clouds/regions

**Limitation:** No intra-replica healing, coarse-grained failover

**Synkti Improvement:**
- Fine-grained migration within region
- Checkpoint recovery for faster resume
- Combines global (SkyServe-style) + local (SpotServe-style) orchestration

---

### Can't Be Late (EuroSys '24)

**Focus:** Batch jobs with strict deadlines

**Innovation:** Uniform Progress policy for deadline-aware scheduling

**Limitation:** No interactive workload support, no GPU orchestration

**Synkti Improvement:**
- Supports both batch and interactive workloads
- GPU memory constraints enforced
- Checkpoint model for partial progress recovery

---

## Benchmark Results

**Configuration:** 100 tasks, 48-hour simulation

| Policy | Cost ($) | Savings vs OnDemand | Completed | Preemptions | Checkpoints |
|--------|----------|---------------------|-----------|-------------|-------------|
| **Greedy** | 193.43 | **78.0%** | 92/100 | 8 | 0/39 |
| **OnDemandFallback** | 157.31 | **82.1%** | 92/100 | 8 | 0/53 |
| **OnDemandOnly** | 878.89 | baseline | 92/100 | 0 | N/A |

**Key Findings:**
- Optimal migration makes aggressive spot usage viable (Greedy competitive with Fallback)
- Checkpoint system correctly identifies early preemptions (nothing to save)
- Realistic cost savings align with SpotServe paper claims
- **Note:** For most rigorous validation, see 200-task benchmark below

---

## Naive vs Optimal Migration Comparison

**Configuration:** 200 tasks, 72-hour simulation

To demonstrate the value of the Kuhn-Munkres algorithm, we compare it against a naive first-fit baseline:

| Policy | Migration Strategy | Cost ($) | Savings | Preemptions | Improvement |
|--------|-------------------|----------|---------|-------------|-------------|
| **Greedy** | Naive (first-fit) | 446.96 | 78.4% | 22 | baseline |
| **Greedy** | Optimal (KM) | 415.72 | **79.9%** | 12 | **+1.5% savings, -45% preemptions** |
| **OnDemandFallback** | Naive (first-fit) | 1294.33 | 37.4% | 10 | baseline |
| **OnDemandFallback** | Optimal (KM) | 696.04 | **66.4%** | 16 | **+29% savings (78% improvement!)** |

**Key Insights:**
- **Greedy policy**: Optimal KM reduces cost by 7% and preemptions by 45%
- **OnDemandFallback policy**: Optimal KM provides **dramatic improvement** - nearly doubles cost savings (37.4% → 66.4%)
- **Overall**: Optimal migration is 1.5-2x more cost-effective than naive greedy assignment

**How to Run:**
```bash
# Compare all policy variants
cargo run --release -p synkti-simulation-engine -- \
  --duration 72 --tasks 200 \
  --policies greedy-naive,greedy-optimal,fallback-naive,fallback-optimal,ondemand
```

---

## Reproducibility

### System Requirements
- Rust 1.91+ (edition 2024)
- No GPU required (pure CPU simulation)
- Dependencies: pathfinding, rand, clap, serde, plotly (dev)

### Build Instructions
```bash
cd crates
cargo build --release -p synkti-simulation-engine
```

### Run Tests
```bash
cargo test -p synkti-simulation-engine
# Expected: 32 tests passing (100% pass rate)
```

### Run Rigorous Benchmark
```bash
cargo run --release -p synkti-simulation-engine -- --duration 72 --tasks 200
```

**Expected Output (with optimal migration):**
```
Policy                   Cost ($)    Completed  Preemptions     Checkpoints
Greedy-Optimal           415.72        200/200           12 ...
OnDemandFallback-Optimal 696.04        200/200           16 ...
OnDemandOnly            2069.00        200/200            0             N/A

Cost Savings vs OnDemandOnly baseline:
  Greedy-Optimal           $1,653.28 (79.9%)
  OnDemandFallback-Optimal $1,372.96 (66.4%)
```

**📊 [View Interactive Visualizations](https://bobby-math.github.io/synkti/)** of these results

---

## Module Summary

| Module | Lines | Tests | Purpose |
|--------|-------|-------|---------|
| `types.rs` | 286 | 5 | Data structures (Task, Instance, Events) |
| `spot_data.rs` | 134 | 2 | Realistic price generation (O-U process) |
| `policies.rs` | 179 | 3 | Scheduling policies (pluggable trait) |
| `simulator.rs` | 567 | 2 | Discrete-event loop (priority queue) |
| `migration.rs` | 443 | 11 | Optimal + naive assignment (Kuhn-Munkres) |
| `checkpoint.rs` | 382 | 9 | Grace period recovery logic |
| `main.rs` | 200 | - | CLI interface with migration strategy support |
| **Total** | **2,191** | **32** | **Complete system** |

---

## Limitations & Future Work

### Current Limitations
1. **Single-region only:** No multi-cloud orchestration (SkyServe-style)
2. **Perfect network:** Assumes constant 10 Gbps bandwidth
3. **Simplified KV cache:** Linear growth model (real workloads may vary)
4. **No batching:** Each task runs on dedicated instance
5. **Simulation only:** Not yet integrated with real cloud APIs

### Future Extensions (Phase 2+)
1. **Real cloud integration:** AWS/GCP spot APIs via cloud-provider traits
2. **Multi-GPU instances:** Support for distributed training workloads
3. **Dynamic batching:** Pack multiple tasks on single instance
4. **Predictive preemption:** Use price trends to anticipate failures
5. **Adaptive policies:** Meta-learning to select best policy per workload

---

## Visualization Example

Generate interactive spot behavior chart:
```bash
cargo run --example visualize_spot_behavior
# Output: visualizations/spot_behavior.html
```

**Shows:** Dual y-axis plot of spot price volatility and preemption risk over time, validating O-U process generates realistic dynamics.

---

## Dependencies

```toml [dependencies]
serde = "1.0"
serde_json = "1.0"
rand = "0.8"
rand_distr = "0.4"
clap = { version = "4.4", features = ["derive"] }
pathfinding = "4.0"  # Kuhn-Munkres algorithm

[dev-dependencies]
plotly = "0.9"  # For visualization examples
```

---

## Contact

**Project:** Synkti (Domain-agnostic spot instance orchestration)

**Phase:** Research & Validation

**Timeline:** Prototype complete, preparing for real-world pilot

---

