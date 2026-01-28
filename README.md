# Synkti

**ML inference on spot instances. 70-85% cost reduction through orchestration.**

---

## Status

This project is actively being restructured. The codebase is transitioning to a cleaner workspace layout. Updates coming soon.

**What works now:**
- Simulation engine for validating scheduling policies
- Core types and traits for spot orchestration
- Agent scaffolding for spot instance monitoring

**In progress:**
- CLI for fleet management
- Full agent implementation
- Documentation

---

## What is Synkti?

Synkti orchestrates ML inference workloads on spot instances. Spot instances are 70-90% cheaper than on-demand, but can be terminated with only 2 minutes notice.

The key insight: you don't need complex checkpoint/restore mechanisms. GPU state can't be checkpointed anyway (CUDA contexts, VRAM). Instead, Synkti uses stateless failover - when a spot instance gets preempted, we drain in-flight requests and spawn a fresh container on a replacement instance.

Same cost savings, simpler architecture, actually works with GPUs.

---

## Repository Structure

```
crates/
├── synkti-core/        # Shared types and traits
├── synkti-agent/       # Runs on spot instances (monitoring, container lifecycle)
├── synkti-cli/         # Command-line interface
└── synkti-simulation/  # Policy testing and cost modeling
```

---

## Quick Start

```bash
git clone https://github.com/Bobby-Math/synkti.git
cd synkti/crates

# Run the simulation engine
cargo run -p synkti-simulation -- --duration 48 --tasks 100

# Build everything
cargo build --workspace
```

---

## License

AGPL-3.0

---

## Contact

Bobby - [github.com/Bobby-Math](https://github.com/Bobby-Math)
