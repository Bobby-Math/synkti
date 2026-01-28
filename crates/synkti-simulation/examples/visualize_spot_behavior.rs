//! Visualize spot instance market behavior
//!
//! Generates interactive HTML chart showing:
//! - Spot price volatility over time
//! - Preemption probability correlation
//! - Price-risk relationship
//!
//! Usage:
//!   cargo run --example visualize_spot_behavior
//!   Open visualizations/spot_behavior.html in browser

use plotly::{
    color::NamedColor,
    common::{Line, Marker, Mode},
    layout::{Axis, Layout},
    Plot, Scatter,
};
use synkti_simulation::spot_data::SpotPriceGenerator;

fn main() {
    println!("🎨 Generating spot instance behavior visualization...");

    // Generate 48 hours of spot price data (realistic market simulation)
    let mut generator = SpotPriceGenerator::new(
        0.30, // Mean spot price: $0.30/hr (30% of on-demand)
        1.00, // On-demand price: $1.00/hr
        0.05, // Base preemption rate: 5% per hour
    );

    let prices = generator.generate(48.0, 0.1); // 48 hours, 6-minute intervals

    println!("   Generated {} data points", prices.len());

    // Extract time series data
    let times: Vec<f64> = prices.iter().map(|p| p.time).collect();
    let price_values: Vec<f64> = prices.iter().map(|p| p.price).collect();
    let preemption_probs: Vec<f64> = prices.iter().map(|p| p.preemption_probability * 100.0).collect(); // Convert to percentage

    // Create price trace (blue line)
    let price_trace = Scatter::new(times.clone(), price_values.clone())
        .name("Spot Price ($/hr)")
        .mode(Mode::Lines)
        .line(
            Line::new()
                .color(NamedColor::Blue)
                .width(2.0)
        );

    // Add on-demand price reference line (dashed red)
    let on_demand_line = Scatter::new(vec![0.0, 48.0], vec![1.00, 1.00])
        .name("On-Demand Price (baseline)")
        .mode(Mode::Lines)
        .line(
            Line::new()
                .color(NamedColor::Red)
                .width(1.5)
                .dash(plotly::common::DashType::Dash)
        );

    // Create preemption probability trace (orange line, secondary y-axis)
    let preemption_trace = Scatter::new(times.clone(), preemption_probs.clone())
        .name("Preemption Risk (%)")
        .mode(Mode::Lines)
        .y_axis("y2")
        .line(
            Line::new()
                .color(NamedColor::OrangeRed)
                .width(2.0)
        );

    // Find preemption events (probability spikes above threshold)
    let threshold = 0.8; // 0.8% per 6-minute interval (~8% per hour)
    let preemption_events: Vec<(f64, f64)> = prices
        .iter()
        .filter(|p| p.preemption_probability * 100.0 > threshold)
        .map(|p| (p.time, p.price))
        .collect();

    let (event_times, event_prices): (Vec<f64>, Vec<f64>) = preemption_events.into_iter().unzip();

    // Mark preemption events with red dots
    let preemption_markers = Scatter::new(event_times, event_prices)
        .name("High Preemption Risk")
        .mode(Mode::Markers)
        .marker(
            Marker::new()
                .color(NamedColor::Red)
                .size(8)
                .symbol(plotly::common::MarkerSymbol::X)
        );

    // Create plot
    let mut plot = Plot::new();
    plot.add_trace(price_trace);
    plot.add_trace(on_demand_line);
    plot.add_trace(preemption_trace);
    plot.add_trace(preemption_markers);

    // Layout configuration
    let layout = Layout::new()
        .title("Spot Instance Market Behavior (48 hours)")
        .x_axis(
            Axis::new()
                .title("Time (hours)")
                .grid_color(NamedColor::LightGray)
        )
        .y_axis(
            Axis::new()
                .title("Spot Price ($/hr)")
                .grid_color(NamedColor::LightGray)
                .range(vec![0.0, 1.2])
        )
        .y_axis2(
            Axis::new()
                .title("Preemption Risk (% per 6min)")
                .overlaying("y")
                .side(plotly::common::AxisSide::Right)
                .range(vec![0.0, 2.0])
        )
        .hover_mode(plotly::layout::HoverMode::X);

    plot.set_layout(layout);

    // Save to HTML
    let output_path = "applications/synkti-simulation-engine/visualizations/spot_behavior.html";
    plot.write_html(output_path);

    println!("✅ Visualization saved to {}", output_path);
    println!("\n📊 Key Observations:");
    println!("   - Price volatility: ${:.2} - ${:.2}/hr",
        price_values.iter().cloned().fold(f64::INFINITY, f64::min),
        price_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    );
    println!("   - Average price: ${:.2}/hr ({}% of on-demand)",
        price_values.iter().sum::<f64>() / price_values.len() as f64,
        (price_values.iter().sum::<f64>() / price_values.len() as f64) * 100.0
    );
    println!("   - Max preemption risk: {:.2}% per interval",
        preemption_probs.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
    );
    println!("   - Price-risk correlation: Lower price → Higher risk (visible in chart)");
    println!("\n🌐 Open in browser:");
    println!("   firefox {} &", output_path);
    println!("   # or");
    println!("   google-chrome {} &", output_path);
}
