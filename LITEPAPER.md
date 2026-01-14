# Synkti: Optimal Orchestration for Volatile Compute

## Abstract

Synkti is an orchestration protocol that makes volatile spot instances production-ready through provably optimal migration and intelligent checkpoint recovery. By achieving up to 80% cost reduction while maintaining reliability, Synkti transforms GPU compute economics for AI workloads.

**Phase 1 Complete (January 2026):** Research prototype with 2,191 lines of Rust, 32 tests passing, and validated cost savings through discrete-event simulation.

---

## The Problem: Compute Scarcity, Not Just Cost

**The Real Crisis:** AI companies can't get enough compute at any price. Sam Altman (OpenAI, Dec 2025): *"We expect to fail to meet enterprise demand for compute in 2026."* The bottleneck is capacity, not willingness to pay.

**Current Paradigm:**
- **On-demand instances:** Reliable but expensive ($1-3/hr) - **limited availability**
- **Spot instances:** 70% cheaper ($0.30/hr) but unreliable (5-15% preemption rate) - **massive untapped inventory**

**The Opportunity:** Spot instances represent stranded cloud capacity—underutilized inventory that sits idle because traditional systems can't handle preemptions. Like airline seats going unsold, this capacity exists but goes unused.

**Synkti's Reframe:** 70-80% cost reduction = **4-5x capacity multiplier**. This isn't just savings—it's unlocking net new usable capacity from stranded inventory.

**Example:**
- Enterprise budget: $1M/month
- On-demand capacity: 1 trillion tokens
- **With Synkti:** Same $1M → 4-5 trillion tokens (utilizing spot)
- **OR:** Same 1 trillion tokens for $200k, reallocate $800k to more capacity

**Why Existing Solutions Fall Short:**
1. **Provably optimal migration** - Use greedy heuristics instead of optimal algorithms
2. **Grace period exploitation** - Don't intelligently use AWS's 120-second warning window
3. **Domain-agnostic design** - Tightly coupled to LLM inference workloads

## The Solution: Three Technical Innovations

### 1. Kuhn-Munkres Optimal Migration

**Problem:** When spot instances are preempted, tasks must migrate to other instances. Existing systems use greedy heuristics.

**Synkti Approach:** Hungarian algorithm (Kuhn-Munkres) for provably minimum-cost bipartite matching.

**Cost Function:**
```
cost(task, instance) = kv_cache_size_mb / (network_bandwidth_gbps × 125 MB/s)
                     = INFINITY if task doesn't fit in instance memory
```

**Result:** 7-46% cost reduction vs naive first-fit, depending on policy (7% for aggressive Greedy, 46% for conservative OnDemandFallback).

**Benchmark (200 tasks, 72 hours):**
- Greedy + Optimal KM: $415.72 (79.9% savings)
- Greedy + Naive: $446.96 (78.4% savings)
- OnDemandFallback + Optimal KM: $696.04 (66.4% savings)
- OnDemandFallback + Naive: $1,294.33 (37.4% savings)

**Impact:** Optimal migration nearly doubles the cost savings for conservative policies.

---

### 2. Grace Period Checkpoint Exploitation

**Problem:** AWS gives 120 seconds warning before terminating spot instances. How to best use this time?

**Synkti Approach:** Intelligent decision tree based on transferable data:

```
transferable_mb = network_bandwidth_gbps × 125 MB/s × 120 seconds
checkpoint_ratio = transferable_mb / kv_cache_size_mb

if checkpoint_ratio ≥ 0.8:  → Full Checkpoint (save everything)
elif checkpoint_ratio ≥ 0.3: → Partial Checkpoint (save what we can)
else:                         → Restart (overhead not worth it)
```

**Example (10 Gbps network):**
- Transferable: 10 Gbps × 125 MB/s × 120s = 150,000 MB (150 GB)
- Tasks with KV cache ≤ 120 GB can fully checkpoint
- Tasks with 40-120 GB can partially checkpoint
- Tasks with >500 GB restart from scratch

**Result:** Tasks recover up to 100% of progress, dramatically reducing completion time.

---

### 3. Domain-Agnostic Orchestration

**Problem:** SpotServe is tightly coupled to LLM inference. Can't generalize to training, batch jobs, or other GPU workloads.

**Synkti Approach:** Pluggable scheduling policies with clean abstraction boundaries.

**Architecture:**
```
Orchestrator Core (workload-agnostic)
    ├── Scheduler (assigns tasks to instances)
    ├── Migration Planner (Kuhn-Munkres algorithm)
    ├── Checkpoint Planner (grace period logic)
    └── Policy Engine ←─── Pluggable Policies
                             ├── Greedy (minimize cost)
                             ├── OnDemandFallback (reliability first)
                             ├── UniformProgress (deadline-aware)
                             └── Custom (user-defined)
```

**Benefit:** Easy to extend to training workloads, batch processing, or deadline-critical jobs.

---

## The Heterogeneous Accelerator Future

**The Optimization Locus Is Shifting:** As AI inference diversifies across custom accelerators (TPUs, LPUs, Trainium, Groq, Cerebras), the value shifts from kernel programming to intelligent orchestration.

### Why No Single Chip Wins

Physics prevents any single accelerator from optimizing all dimensions:

| Workload Type | Optimal Chip | Why |
|---------------|-------------|-----|
| Latency-critical (p99 < 100ms) | Groq LPU | Deterministic dataflow |
| High-throughput batch | Google TPU v5 | Highest FLOPS/$ |
| Cost-optimized production | AWS Trainium | AWS pricing + spot discounts |
| Research & flexibility | NVIDIA H100 | Full control + ecosystem |
| Memory-bound workloads | AMD MI300X | 192GB HBM3 vs 80GB |

**Fundamental Tradeoffs:**
- ❌ Can't be simultaneously deterministic (LPU) AND flexible (GPU)
- ❌ Can't be simultaneously highest throughput (TPU) AND lowest latency (LPU)
- ❌ Can't be simultaneously cheapest (custom) AND most ecosystem-compatible (NVIDIA)

**Result:** The future is polyglot accelerators, not consolidation.

### Synkti's Strategic Position

**Today (Phase 1):** Prove algorithms on NVIDIA GPUs (homogeneous, AWS Spot)

**Tomorrow (Phase 2-3):** Multi-accelerator orchestration across GPU/TPU/LPU/Trainium

**Orchestrator Value Proposition:**
```
Inference request arrives
    ↓
Synkti classifies workload + selects optimal chip
    ↓
    ├── p99 < 50ms? → Groq LPU (deterministic)
    ├── Batch throughput? → TPU v5 (highest FLOPS/$)
    ├── Cost-sensitive? → Trainium spot (AWS discount)
    └── Fallback → NVIDIA H100 spot
```

**Why Domain-Agnostic Architecture Matters More:** As hardware fragments, orchestration that works across ANY accelerator type becomes increasingly valuable. Synkti's pluggable policy engine is future-proof.

---

## System Architecture

Synkti's architecture implements **two-layer abstraction** to democratize access to volatile compute:

### Layer 1: Volatility Abstraction (Compute & Execution)

**Heterogeneous spot instances from:**
- Traditional cloud providers (AWS, GCP, Azure)
- Decentralized networks (Bittensor, Akash)
- Independent providers (Vast.ai, RunPod)

**Each instance runs:**
- **Synkti Data Plane** (Rust agent) - Executes tasks, reports health, handles local checkpointing
- **Application Runtime** - Your workload (ML models, batch jobs, etc.)

**What this layer does:** Makes volatile resources feel reliable through optimal migration and checkpoint recovery.

---

### Layer 2: Application Abstraction (Orchestrator Intelligence)

**Synkti Orchestrator** - The control plane that understands your workload:

1. **Application-Aware Scheduling** - Different strategies for ML inference vs batch vs streaming
2. **Predictive Preemption Management** - Forecasts failures before they occur
3. **Workload-Specific Migration** - Knows how to move your application safely (model weights, KV cache, job state)
4. **SLA-Driven Policies** - Optimizes for your latency/cost/reliability requirements

**What this layer does:** Domain-specific orchestration that maximizes spot usage while meeting application SLAs.

**Why this matters:** Current DePIN providers (Akash, io.net) give you raw compute. You still handle deployment, failures, and optimization. **Synkti gives you application-aware orchestration**—describe your workload, get optimal deployment automatically.

---

### Layer 3: Trust & Settlement (Phase 3)

**Blockchain-based settlement (Solana):**
- Provider identity and reputation
- Cryptographic work verification
- Job settlement and payment

**Why Solana:** Sub-second finality (400ms) enables real-time reputation updates. High throughput (50k+ TPS) supports thousands of job settlements per hour. Low costs ($0.00025/tx) make micropayments viable.

**Democratization angle:** Anyone can deploy applications on decentralized compute **directly**, without intermediaries. Developers own their infrastructure orchestration, not cloud providers.

## Current Status (January 2026)

### Phase 1: COMPLETE ✅

**Deliverables:**
- ✅ Discrete-event simulation engine (2,191 lines of Rust)
- ✅ Kuhn-Munkres optimal migration algorithm (443 lines, 11 tests)
- ✅ Checkpoint recovery system (382 lines, 9 tests)
- ✅ Realistic spot price generation (Ornstein-Uhlenbeck process)
- ✅ 32 comprehensive tests (100% passing)
- ✅ Benchmark validation: Up to 80% cost savings (200-task rigorous test)
- ✅ Open-source repository with documentation

**Technical Proof:**
- Optimal migration provides 7-46% cost reduction vs naive (policy-dependent)
- Checkpoint recovery maintains progress during preemptions
- Domain-agnostic design validated through pluggable policies

**Repository:** [github.com/bobby-math/synkti](https://github.com/bobby-math)

**Interactive Demo:** [https://bobby-math.github.io/synkti/](https://bobby-math.github.io/synkti/)

## Roadmap

**Development Philosophy:** Synkti follows a **research → product pipeline**—starting with academic-grade algorithms (Phase 1), validating with real users (Phase 2), and scaling to production (Phase 3).

### Phase 2: Production MVP + Research Validation (6 Months, Grant-Funded)

**Objective:** Deploy production system with Level 3 Prognostics Engine, validated across 243 scenarios.

**Philosophy:** Research → product pipeline means building mathematical foundation first, then applying it to production infrastructure.

**Key Deliverables:**
1. **State Space Formalization** - Mathematical framework for optimal recovery strategies
2. **Prognostics Engine** - ARIMA + FFT/DSP for proactive preemption handling (Level 3 orchestration)
3. **243-Scenario Validation** - Comprehensive testing across model size × network × volatility × context × quantization
4. **Real Cloud Integration** - AWS Spot API with state space calculator integration
5. **Production Orchestrator** - Control plane (scheduling, migration, prognostics) + data plane (instance agents for task execution and health monitoring)
6. **Pilot Program** - 3-5 early adopters running production workloads
7. **Validation Report** - Prove simulation accuracy <5% error vs reality

**Success Metrics:**
- Simulation accuracy <5% error (not just "roughly matches")
- Prognostics accuracy >70% (ARIMA or FFT)
- 243 scenarios tested (comprehensive, not cherry-picked)
- 70%+ cost reduction validated on real AWS workloads
- 3+ pilot users running production inference/training
- Open-source release with prognostics library

---

### Phase 3: Decentralized Protocol (2027)

**Objective:** Transform into permissionless, blockchain-verified compute fabric.

**Key Milestones:**
1. **Solana Smart Contracts** - Provider reputation, job settlement, payment rails
2. **Decentralized Providers** - Integration with Bittensor, Akash, independent nodes
3. **Proactive Orchestration** - Statistical forecasting and system health analysis to anticipate preemptions before they occur
4. **Cryptographic Verification** - ZK-proofs or attestations for completed work
5. **Public Launch** - Permissionless participation for users and providers
6. **Academic Publication** - Research paper at top-tier systems conference (OSDI/SOSP/EuroSys)

**Vision:** A global, permissionless marketplace where anyone can contribute compute and anyone can consume it reliably.

## Why Synkti Matters

**Capacity Expansion, Not Just Cost Reduction:** The 2026 compute crisis (Sam Altman) means enterprises need MORE capacity, not just cheaper capacity. Synkti unlocks 4-5x more usable compute from stranded spot inventory—enabling use cases that wouldn't exist otherwise (free tier users, chatbots, experimentation).

**Future-Proof for Heterogeneous Chips:** As custom accelerators proliferate (TPU, LPU, Trainium, Groq), domain-agnostic orchestration becomes MORE valuable. Synkti's architecture works across any chip type, making it the intelligent routing layer for the polyglot accelerator future.

**From Reactive to Proactive Orchestration:** Traditional autoscalers react after problems occur. Advanced systems (SpotServe) optimize the *response* to preemption. Synkti's vision is **proactive orchestration**—anticipating disruptions before they happen through statistical forecasting and system health analysis, achieving zero-downtime transitions.

**Infra-as-a-Library (Long-term Vision):** Synkti's two-layer abstraction—volatility + application—enables a future where developers describe their workload requirements and get optimal deployment automatically. Instead of configuring cloud resources manually, you specify:

```yaml
workload:
  type: ml-inference
  model: llama-70b
  latency_sla: p99 < 200ms
  cost_target: minimize
```

Synkti automatically chooses optimal instance mix (spot + on-demand), pre-warms standby instances, routes traffic based on predicted preemptions, and migrates with zero user-visible downtime. Future iterations could use machine learning on code embeddings to predict resource requirements automatically, eliminating manual manifest writing entirely. This works for **any application type**—ML inference is Phase 1, but the architecture generalizes to batch processing, streaming, training, and more. The goal: make decentralized compute as easy to deploy as AWS Lambda, but 70% cheaper and censorship-resistant.

**Open-Source Foundation:** All core algorithms and orchestration logic are open-source under AGPL-3.0, ensuring modifications are shared back while enabling commercial use and innovation.

**Path to Decentralization:** Phase 3 transforms Synkti into a permissionless protocol, creating a truly global compute fabric independent of any single provider.

---

## Related Work & Novel Contributions

| System | Focus | Key Limitation | Synkti Improvement |
|--------|-------|----------------|-------------------|
| **SpotServe** (OSDI '24) | LLM inference resilience | Greedy migration, LLM-only | Optimal KM algorithm (7-46% better), domain-agnostic |
| **SkyServe** | Multi-cloud serving | Coarse-grained failover | Fine-grained checkpoint recovery + migration |
| **Can't Be Late** (EuroSys '24) | Batch deadlines | No GPU support, no checkpointing | GPU memory constraints, grace period exploitation |

**Synkti's Novel Contributions:**
1. **Provably optimal migration** via Kuhn-Munkres algorithm (first in spot orchestration)
2. **Grace period checkpoint exploitation** with intelligent Full/Partial/Restart decision logic
3. **Domain-agnostic architecture** enabling training, inference, batch, and custom workloads

---

## About

**Developer:** Bobby - Independent protocol developer and cloud researcher

**Focus:** AI infrastructure, distributed systems, Web3

**Background:** Designing systems that make volatile resources production-ready

**Contact:**
- GitHub: [github.com/bobby-math](https://github.com/bobby-math)
- Website: [bobby-math.dev](https://bobby-math.dev)
- Email: hello@bobby-math.dev  

**License:** GNU Affero General Public License v3.0 (AGPL-3.0)

### Why AGPL-3.0? Commitment to Open Source

**The Choice:** We chose AGPL-3.0 because Synkti is foundational infrastructure that should remain a public good.

**What AGPL-3.0 Ensures:**
- ✅ All modifications must be shared back (even for network services)
- ✅ OSI-approved and FSF-approved license
- ✅ Compatible with research grants requiring open-source licensing
- ✅ Self-hosting, research, academic, and commercial use all permitted
- ✅ Contributor-friendly, enabling community growth

**Our Moat:** The real value isn't in the code—it's in the systems knowledge and network effects. Anyone can read the algorithms. Few can deploy and operate them effectively. The expertise to configure, tune, and optimize Synkti for specific workloads is the service layer.

**Philosophy:** Good protocols are built on solid foundations, shared openly. The knowledge to use them well is what creates sustainable value.

---

## Get Involved

**For Researchers:** Contribute to algorithms, run benchmarks, extend policies

**For Users:** Join pilot program (Phase 2), test on your workloads

**For Investors/Grants:** See VISION.md for funding roadmap

**Repository:** [github.com/bobby-math/synkti](https://github.com/bobby-math/synkti)
