//! Tessera Simulation Engine CLI
//!
//! Command-line interface for running spot instance orchestration simulations

use clap::Parser;
use serde_json;
use std::fs;

use tessera_simulation_engine::{
    policies::{GreedyPolicy, OnDemandFallbackPolicy, OnDemandOnlyPolicy},
    simulator::Simulator,
    spot_data::SpotPriceGenerator,
    types::Task,
};

#[derive(Parser, Debug)]
#[command(name = "tessera-sim")]
#[command(about = "Simulate spot instance orchestration policies", long_about = None)]
struct Args {
    /// Simulation duration in hours
    #[arg(short, long, default_value_t = 48.0)]
    duration: f64,

    /// Number of tasks to simulate
    #[arg(short, long, default_value_t = 100)]
    tasks: usize,

    /// Policies to compare (comma-separated: greedy,fallback,ondemand)
    #[arg(short, long, default_value = "greedy,fallback,ondemand")]
    policies: String,

    /// On-demand instance price ($/hr)
    #[arg(long, default_value_t = 1.00)]
    on_demand_price: f64,

    /// Mean spot price ($/hr)
    #[arg(long, default_value_t = 0.30)]
    spot_price: f64,

    /// Base preemption rate (per hour)
    #[arg(long, default_value_t = 0.05)]
    preemption_rate: f64,

    /// Output JSON file path (optional)
    #[arg(short, long)]
    output: Option<String>,
}

fn main() {
    let args = Args::parse();

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║  Tessera Simulation Engine                               ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    println!("Configuration:");
    println!("  Duration: {} hours", args.duration);
    println!("  Tasks: {}", args.tasks);
    println!("  On-demand price: ${:.2}/hr", args.on_demand_price);
    println!("  Spot price: ${:.2}/hr", args.spot_price);
    println!("  Preemption rate: {:.1}%/hr\n", args.preemption_rate * 100.0);

    // Generate spot price data
    println!("Generating spot price data...");
    let mut price_generator = SpotPriceGenerator::new(
        args.spot_price,
        args.on_demand_price,
        args.preemption_rate,
    );
    let spot_prices = price_generator.generate(args.duration, 0.1); // 6-minute intervals
    println!("  Generated {} price data points\n", spot_prices.len());

    // Generate tasks
    println!("Generating {} tasks...", args.tasks);
    let tasks: Vec<Task> = (0..args.tasks)
        .map(|i| {
            // Random arrival time (uniform distribution over simulation duration)
            let arrival_time = rand::random::<f64>() * args.duration * 0.8; // Arrive in first 80%

            // Random duration (1-20 hours)
            let duration = 1.0 + rand::random::<f64>() * 19.0;

            Task::new(i as u64, arrival_time, duration)
        })
        .collect();
    println!("  Tasks created\n");

    // Parse policy list
    let policy_names: Vec<&str> = args.policies.split(',').map(|s| s.trim()).collect();

    let mut results = Vec::new();

    // Run simulation for each policy
    for policy_name in &policy_names {
        print!("Running simulation with {} policy... ", policy_name);

        // Parse policy name and migration strategy
        // Supports: "greedy", "greedy-naive", "greedy-optimal", etc.
        let (base_policy, use_optimal) = if policy_name.ends_with("-naive") {
            (policy_name.trim_end_matches("-naive"), false)
        } else if policy_name.ends_with("-optimal") {
            (policy_name.trim_end_matches("-optimal"), true)
        } else {
            // Default: use optimal for backwards compatibility
            (*policy_name, true)
        };

        let policy_box: Box<dyn tessera_simulation_engine::policies::SchedulingPolicy> = match base_policy {
            "greedy" => Box::new(GreedyPolicy::new()),
            "fallback" => Box::new(OnDemandFallbackPolicy::new(2)), // Fallback after 2 preemptions
            "ondemand" => Box::new(OnDemandOnlyPolicy::new()),
            _ => {
                eprintln!("Unknown policy: {}", policy_name);
                continue;
            }
        };

        let mut simulator = Simulator::new(
            policy_box,
            spot_prices.clone(),
            args.on_demand_price,
            use_optimal,
        );

        // Add all tasks
        for task in tasks.clone() {
            simulator.add_task(task);
        }

        // Run simulation
        let result = simulator.run(args.duration);
        println!("Done");

        results.push(result);
    }

    // Display results
    println!("\n╔══════════════════════════════════════════════════════════╗");
    println!("║  Simulation Results                                      ║");
    println!("╚══════════════════════════════════════════════════════════╝\n");

    println!("{:<20} {:>12} {:>12} {:>12} {:>12} {:>12} {:>15}",
        "Policy", "Cost ($)", "Completed", "Preemptions", "Avg Time", "P99 Time", "Checkpoints");
    println!("{}", "-".repeat(107));

    for result in &results {
        let checkpoint_info = if result.checkpoints_attempted > 0 {
            format!("{}/{} ({:.1}h saved)",
                result.checkpoints_successful,
                result.checkpoints_attempted,
                result.total_time_saved_hours)
        } else {
            "N/A".to_string()
        };

        println!("{:<20} {:>12.2} {:>10}/{:<2} {:>12} {:>12.2} {:>12.2} {:>15}",
            result.policy_name,
            result.total_cost,
            result.completed_tasks,
            result.total_tasks,
            result.total_preemptions,
            result.average_completion_time,
            result.p99_completion_time,
            checkpoint_info,
        );
    }

    // Calculate savings (use OnDemand-only as baseline, or most expensive)
    if results.len() > 1 {
        let baseline = results.iter()
            .find(|r| r.policy_name == "OnDemandOnly")
            .or_else(|| results.iter().max_by(|a, b| a.total_cost.partial_cmp(&b.total_cost).unwrap()))
            .unwrap();

        println!("\n{}", "-".repeat(107));
        println!("Cost Savings vs {} baseline:", baseline.policy_name);

        for result in &results {
            if result.policy_name != baseline.policy_name {
                let savings = baseline.total_cost - result.total_cost;
                let savings_pct = (savings / baseline.total_cost) * 100.0;
                println!("  {:<18} ${:>8.2} ({:>5.1}%)",
                    result.policy_name,
                    savings,
                    savings_pct
                );
            }
        }
    }

    // Output to JSON if requested
    if let Some(output_path) = args.output {
        println!("\nWriting results to {}...", output_path);
        let json = serde_json::to_string_pretty(&results).unwrap();
        fs::write(&output_path, json).expect("Failed to write JSON output");
        println!("  Results saved");
    }

    println!("\n✅ Simulation complete!\n");
}
