# Synkti

**Stateless failover orchestration for GPU/TPU spot instances**

---

## Project Status

**Phase 1 (Q4 2025):** ✅ **Complete** - Simulation Engine & Research Prototype

**Phase 2 (Q1 2026):** 🔄 **In Progress** - AWS Orchestrator & Stateless Failover

---

## What is Synkti?

Synkti is a production-grade orchestration system for managing ML inference workloads on volatile spot instances, achieving **70-90% cost reduction** while maintaining reliability.

### The Problem

GPU spot instances are 70-90% cheaper than on-demand, but get preempted with only 2 minutes warning. Traditional solutions attempt checkpoint/restore, but:

> **Docker checkpoint (CRIU) cannot snapshot GPU/TPU state.**
>
> CUDA contexts, VRAM, and TPU HBM cannot be serialized. `docker checkpoint create` will fail or hang on containers actively using accelerators.

### The Solution: Stateless Failover

Instead of fighting hardware limitations, Synkti embraces stateless architecture:

```
Spot Preemption Notice (120s grace)
    │
    ├── 1. Drain: Stop new requests, wait for in-flight (max 115s)
    │
    ├── 2. Select: Choose replacement instance (FIFO → Warm+LeastLoaded)
    │
    ├── 3. Spawn: Start fresh container on replacement
    │
    └── 4. Route: Health check, update load balancer
```

**Result:** Same 70-90% cost savings, simpler architecture, more reliable.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                    Synkti Orchestrator                       │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐  │
│  │   Monitor   │  │   Drain     │  │     Failover        │  │
│  │ (spot poll) │──│  Manager    │──│     Manager         │  │
│  └─────────────┘  └─────────────┘  └─────────────────────┘  │
│         │                │                    │              │
│         │                │         ┌──────────┴──────────┐  │
│         │                │         │   Node Assigner     │  │
│         │                │         │ (FIFO/LeastLoaded/  │  │
│         │                │         │  Warm+LeastLoaded)  │  │
│         │                │         └─────────────────────┘  │
└─────────┼────────────────┼────────────────────┼─────────────┘
          │                │                    │
          ▼                ▼                    ▼
┌─────────────────┐  ┌─────────────┐  ┌─────────────────────┐
│  EC2 Metadata   │  │   vLLM      │  │   Standby Pool      │
│  169.254.169.254│  │   /health   │  │   (replacement      │
│                 │  │   /metrics  │  │    instances)       │
└─────────────────┘  └─────────────┘  └─────────────────────┘
```

---

## Quick Start

```bash
# Clone and build
git clone git@github.com:Bobby-Math/synkti.git
cd synkti/crates/

# Run simulation (validates cost model)
cargo run --release -p synkti-simulation-engine -- --duration 72 --tasks 200

# Run orchestrator tests (35 tests)
cargo test -p synkti-orchestrator

# Build orchestrator
cargo build --release -p synkti-orchestrator
```

---

## Key Components

### Simulation Engine (Phase 1 - Complete)

Discrete-event simulator that validates scheduling policies against synthetic spot data:

| Policy | Cost | Savings | Preemptions |
|--------|------|---------|-------------|
| Greedy + Optimal KM | $416 | **80%** | 12 |
| OnDemand Fallback | $696 | **66%** | 16 |
| OnDemand Only | $2,069 | baseline | 0 |

### Orchestrator (Phase 2 - In Progress)

| Module | Purpose | Status |
|--------|---------|--------|
| `monitor.rs` | Spot interruption detection (metadata polling) | ✅ Complete |
| `instance.rs` | EC2 lifecycle management | ✅ Complete |
| `vllm.rs` | vLLM container management | ✅ Complete |
| `drain.rs` | Graceful request draining (115s timeout) | ✅ Complete |
| `assign.rs` | Node assignment strategies | ✅ Complete |
| `failover.rs` | Failover orchestration | ✅ Complete |
| `pool.rs` | Standby instance pool | ⏳ Planned |
| `main.rs` integration | Wire failover to monitor | ⏳ Planned |

### Assignment Strategies

```rust
pub enum AssignmentStrategy {
    EarliestNode,      // FIFO - deterministic, debuggable (start here)
    LeastLoaded,       // Load-aware - balances traffic
    WarmLeastLoaded,   // Hybrid - prefer warm cache + load balance (recommended)
    Random,            // Statistical distribution
}
```

**Recommendation:** Start with `EarliestNode` for debugging, graduate to `WarmLeastLoaded` for production.

---

## Why Stateless Failover?

| Factor | Checkpoint Migration | Stateless Failover |
|--------|---------------------|-------------------|
| **GPU/TPU Support** | ❌ CRIU can't snapshot | ✅ Works with any accelerator |
| **Complexity** | High (serialize, transfer, restore) | Low (drain, respawn) |
| **Development time** | 1-2 months | 1-2 weeks |
| **Cost savings** | 70-90% | 70-90% (same!) |
| **Failure modes** | Many (partial checkpoint, corrupt state) | Few (just retry) |

**The 70% savings comes from spot pricing, not migration strategy.** Ship the simple thing first.

---

## Repository Structure

```
synkti/
├── crates/
│   └── applications/
│       ├── synkti-orchestrator/      ← AWS ORCHESTRATOR
│       │   ├── src/
│       │   │   ├── drain.rs          (graceful draining)
│       │   │   ├── assign.rs         (node assignment)
│       │   │   ├── failover.rs       (orchestration)
│       │   │   ├── monitor.rs        (spot detection)
│       │   │   ├── instance.rs       (EC2 lifecycle)
│       │   │   ├── vllm.rs           (container management)
│       │   │   └── migration.rs      (KM cost calculation)
│       │   └── Cargo.toml
│       │
│       └── synkti-simulation-engine/ ← SIMULATION ENGINE
│           ├── src/
│           │   ├── simulator.rs      (discrete-event loop)
│           │   ├── policies.rs       (scheduling policies)
│           │   └── migration.rs      (KM algorithm)
│           └── README.md
│
├── LITEPAPER.md                      (vision & roadmap)

```

---

## Related Work

| System | Focus | Limitation | Synkti Approach |
|--------|-------|-----------|-----------------|
| **SpotServe** (OSDI '24) | LLM inference | Assumes checkpoint works | Stateless failover |
| **SkyServe** | Multi-cloud | No intra-replica healing | Grace period drain |
| **Can't Be Late** (EuroSys '24) | Batch deadlines | No GPU support | GPU-native design |

---

## What's Next (Stateless Failover Phase 2)

1. **Remote Execution** - SSM/SSH to spawn containers on remote instances
2. **Load Balancer Integration** - ALB/NLB deregistration during drain
3. **vLLM Metrics** - Query `/metrics` for actual in-flight request count
4. **main.rs Integration** - Wire failover to spot monitor
5. **Standby Pool** - Maintain warm replacement instances

---

## Benchmark Results

**Simulation Validation (200 tasks, 72 hours):**

- **80% cost reduction** with greedy spot scheduling
- **Kuhn-Munkres** provides 7-46% improvement over naive assignment (validated in simulation)
- **Stateless failover** uses simple heuristics (`WarmLeastLoaded`, `LeastLoaded`) for 1→N selection
- KM algorithm retained for future N→M batch preemption scenarios

**Real-World Expectation:**

- 70-90% cost savings (spot vs on-demand pricing)
- <120s total failover time (within AWS grace period)
- Client retries hit new instance with fresh model state

---

## Contact

**Author:** Bobby ([github.com/bobby-math](https://github.com/bobby-math))

**Website:** www.bobby-math.dev

**License:** GNU Affero General Public License v3.0 (AGPL-3.0)

---
