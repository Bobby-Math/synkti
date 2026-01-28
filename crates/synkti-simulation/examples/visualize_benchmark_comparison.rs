//! Visualize benchmark comparison results
//!
//! Generates interactive HTML chart showing:
//! - Cost comparison across policies (200-task, 72-hour simulation)
//! - Savings percentage visualization
//! - Preemption counts
//!
//! Usage:
//!   cargo run --example visualize_benchmark_comparison
//!   Open visualizations/benchmark_comparison.html in browser

use plotly::{
    color::NamedColor,
    common::{Marker, Mode},
    layout::{Axis, BarMode, Layout},
    Bar, Plot, Scatter,
};

fn main() {
    println!("🎨 Generating benchmark comparison visualization...");

    // Benchmark data from 200-task, 72-hour simulation
    let policies = vec![
        "Greedy\n(Naive)",
        "Greedy\n(Optimal KM)",
        "OnDemand\nFallback (Naive)",
        "OnDemand\nFallback (Optimal)",
        "OnDemand\nOnly",
    ];

    let costs = vec![446.96, 415.72, 1294.33, 696.04, 2069.0];
    let savings_pct = vec![78.4, 79.9, 37.4, 66.4, 0.0];
    let preemptions = vec![22, 12, 10, 16, 0];

    // Colors: naive=orange, optimal=green, baseline=gray
    let colors = vec![
        "rgb(255, 140, 0)",  // Naive - Orange
        "rgb(34, 139, 34)",  // Optimal - Green
        "rgb(255, 140, 0)",  // Naive - Orange
        "rgb(34, 139, 34)",  // Optimal - Green
        "rgb(128, 128, 128)", // Baseline - Gray
    ];

    println!("   Creating cost comparison chart...");

    // Create cost bar chart
    let cost_trace = Bar::new(policies.clone(), costs.clone())
        .name("Cost ($)")
        .marker(Marker::new().color_array(colors.clone()));

    let mut cost_plot = Plot::new();
    cost_plot.add_trace(cost_trace);

    let cost_layout = Layout::new()
        .title("Cost Comparison: 200 Tasks, 72 Hours")
        .x_axis(Axis::new().title("Policy (Migration Strategy)"))
        .y_axis(Axis::new()
            .title("Total Cost ($)")
            .grid_color(NamedColor::LightGray)
        )
        .hover_mode(plotly::layout::HoverMode::X)
        .show_legend(false);

    cost_plot.set_layout(cost_layout);

    // Save cost comparison
    let cost_path = "applications/synkti-simulation-engine/visualizations/benchmark_cost.html";
    cost_plot.write_html(cost_path);
    println!("   ✅ Cost chart saved to {}", cost_path);

    // Create savings percentage chart
    println!("   Creating savings percentage chart...");

    let savings_trace = Bar::new(policies.clone(), savings_pct.clone())
        .name("Savings (%)")
        .marker(Marker::new().color_array(colors.clone()));

    let mut savings_plot = Plot::new();
    savings_plot.add_trace(savings_trace);

    let savings_layout = Layout::new()
        .title("Cost Savings vs On-Demand Baseline")
        .x_axis(Axis::new().title("Policy (Migration Strategy)"))
        .y_axis(Axis::new()
            .title("Savings (%)")
            .grid_color(NamedColor::LightGray)
            .range(vec![0.0, 90.0])
        )
        .hover_mode(plotly::layout::HoverMode::X)
        .show_legend(false);

    savings_plot.set_layout(savings_layout);

    let savings_path = "applications/synkti-simulation-engine/visualizations/benchmark_savings.html";
    savings_plot.write_html(savings_path);
    println!("   ✅ Savings chart saved to {}", savings_path);

    // Create combined overview chart (side-by-side bars)
    println!("   Creating combined overview chart...");

    // Normalize costs to percentage of baseline for comparison
    let baseline_cost = 2069.0;
    let cost_pct: Vec<f64> = costs.iter().map(|c| (c / baseline_cost) * 100.0).collect();

    let cost_pct_trace = Bar::new(policies.clone(), cost_pct.clone())
        .name("Cost (% of baseline)")
        .marker(Marker::new()
            .color("rgba(55, 128, 191, 0.7)")
            .line(plotly::common::Line::new()
                .color("rgba(55, 128, 191, 1.0)")
                .width(1.5)
            )
        );

    let preemptions_f64: Vec<f64> = preemptions.iter().map(|&p| p as f64).collect();
    let preemption_trace = Bar::new(policies.clone(), preemptions_f64)
        .name("Preemptions")
        .marker(Marker::new()
            .color("rgba(219, 64, 82, 0.7)")
            .line(plotly::common::Line::new()
                .color("rgba(219, 64, 82, 1.0)")
                .width(1.5)
            )
        )
        .y_axis("y2");

    let mut combined_plot = Plot::new();
    combined_plot.add_trace(cost_pct_trace);
    combined_plot.add_trace(preemption_trace);

    let combined_layout = Layout::new()
        .title("Benchmark Overview: Cost vs Preemptions (200 tasks, 72h)")
        .bar_mode(BarMode::Group)
        .x_axis(Axis::new().title("Policy (Migration Strategy)"))
        .y_axis(Axis::new()
            .title("Cost (% of On-Demand Baseline)")
            .grid_color(NamedColor::LightGray)
            .range(vec![0.0, 110.0])
        )
        .y_axis2(Axis::new()
            .title("Preemption Count")
            .overlaying("y")
            .side(plotly::common::AxisSide::Right)
            .range(vec![0.0, 25.0])
        )
        .hover_mode(plotly::layout::HoverMode::X);

    combined_plot.set_layout(combined_layout);

    let combined_path = "applications/synkti-simulation-engine/visualizations/benchmark_comparison.html";
    combined_plot.write_html(combined_path);
    println!("   ✅ Combined overview saved to {}", combined_path);

    // Print summary statistics
    println!("\n📊 Benchmark Summary (200 tasks, 72 hours):");
    println!("   ┌─────────────────────────────────────────────────────────┐");
    println!("   │ Policy                     Cost      Savings  Preemptions│");
    println!("   ├─────────────────────────────────────────────────────────┤");
    for i in 0..policies.len() {
        println!("   │ {:22} ${:7.2}   {:5.1}%      {:2}       │",
            policies[i].replace('\n', " "),
            costs[i],
            savings_pct[i],
            preemptions[i]
        );
    }
    println!("   └─────────────────────────────────────────────────────────┘");

    println!("\n🔑 Key Findings:");
    println!("   • Optimal KM migration achieves up to 80% cost reduction");
    println!("   • Optimal provides 7-46% cost reduction vs naive (policy-dependent)");
    println!("   • Greedy-Optimal: 45% fewer preemptions than Greedy-Naive");

    println!("\n🌐 Open visualizations in browser:");
    println!("   firefox {} &", combined_path);
}
