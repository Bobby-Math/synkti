# Synkti Interactive Visualizations

Interactive Plotly charts demonstrating Synkti's benchmark results from the 200-task, 72-hour simulation.

**🌐 [View Live](https://bobby-math.github.io/synkti/)** - Deployed on GitHub Pages

---

## Reproduce These Visualizations

### Generate All Visualizations

```bash
# From the crates directory
cd crates

# Generate benchmark comparison charts
cargo run --release --example visualize_benchmark_comparison

# Generate naive vs optimal comparison
cargo run --release --example visualize_naive_vs_optimal

# Generate spot price behavior simulation
cargo run --release --example visualize_spot_behavior
```

**Output:** 8 interactive HTML files in `applications/synkti-simulation-engine/visualizations/`

---

### View Locally

```bash
cd applications/synkti-simulation-engine/visualizations

# Option 1: Open directly in browser
firefox index.html &

# Option 2: Local HTTP server (recommended)
python3 -m http.server 8000
# Then visit: http://localhost:8000
```

---

## What Gets Generated

### Landing Page
- **`index.html`** - Main page with navigation and stats dashboard

### Interactive Charts
- **`benchmark_comparison.html`** - Cost/savings comparison across all policies
- **`benchmark_cost.html`** - Cost breakdown bar chart
- **`benchmark_savings.html`** - Savings percentage chart
- **`naive_vs_optimal.html`** - Direct comparison of migration algorithms ⭐
- **`naive_vs_optimal_cost.html`** - Cost comparison (naive vs optimal)
- **`naive_vs_optimal_improvement.html`** - Improvement percentage chart
- **`spot_behavior.html`** - Spot price volatility and preemption risk simulation

---

## Features

✅ **Interactive Charts** - Hover, zoom, pan (Plotly.js)
✅ **Zero Latency** - Pre-generated, instant load
✅ **Mobile Responsive** - Works on all devices
✅ **Professional Design** - Gradient header, clean layout
✅ **Self-Contained** - No external dependencies

---

## Troubleshooting

**Visualizations not generating?**
```bash
# Ensure you're in the crates directory
cd synkti/crates

# Try with explicit path
cargo run --release --manifest-path applications/synkti-simulation-engine/Cargo.toml \
  --example visualize_benchmark_comparison
```

**Charts not displaying?**
- Check browser console for errors
- Ensure all HTML files are in the same directory
- Try clearing browser cache
- Use Python HTTP server instead of opening files directly

---

## File Structure

```
visualizations/
├── index.html                              # Landing page
├── benchmark_comparison.html               # Main benchmark chart
├── benchmark_cost.html                     # Cost breakdown
├── benchmark_savings.html                  # Savings percentage
├── naive_vs_optimal.html                   # Algorithm comparison ⭐
├── naive_vs_optimal_cost.html              # Cost comparison
├── naive_vs_optimal_improvement.html       # Improvement chart
├── spot_behavior.html                      # Market simulation
└── README.md                               # This file
```

---

**Live Site:** https://bobby-math.github.io/synkti/

**Last Updated:** January 2026
