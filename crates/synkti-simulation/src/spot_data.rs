//! Synthetic spot price generation using Ornstein-Uhlenbeck process
//!
//! Models spot prices as a mean-reverting stochastic process with:
//! - Daily/weekly periodicity
//! - Realistic preemption rates
//! - Price volatility similar to AWS spot market

use rand_distr::{Distribution, Normal};

use crate::types::SpotPrice;

/// Spot price generator using Ornstein-Uhlenbeck process
pub struct SpotPriceGenerator {
    mean_price: f64,
    volatility: f64,
    mean_reversion_speed: f64,
    current_price: f64,
    on_demand_price: f64,
    base_preemption_rate: f64, // Per hour
}

impl SpotPriceGenerator {
    /// Create a new spot price generator
    ///
    /// # Arguments
    /// * `mean_price` - Mean spot price (e.g., $0.30/hr)
    /// * `on_demand_price` - On-demand price for comparison (e.g., $1.00/hr)
    /// * `base_preemption_rate` - Base preemption probability per hour (e.g., 0.05 = 5%)
    pub fn new(mean_price: f64, on_demand_price: f64, base_preemption_rate: f64) -> Self {
        SpotPriceGenerator {
            mean_price,
            volatility: 0.2,              // 20% volatility
            mean_reversion_speed: 0.5,    // Moderate mean reversion
            current_price: mean_price,
            on_demand_price,
            base_preemption_rate,
        }
    }

    /// Generate spot price data for a time period
    ///
    /// # Arguments
    /// * `duration_hours` - Total simulation time in hours
    /// * `sample_interval` - Time between samples in hours
    ///
    /// # Returns
    /// Vector of spot prices with preemption probabilities
    pub fn generate(&mut self, duration_hours: f64, sample_interval: f64) -> Vec<SpotPrice> {
        let num_samples = (duration_hours / sample_interval).ceil() as usize;
        let mut prices = Vec::with_capacity(num_samples);
        let mut rng = rand::thread_rng();
        let normal = Normal::new(0.0, 1.0).unwrap();

        for i in 0..num_samples {
            let time = i as f64 * sample_interval;

            // Ornstein-Uhlenbeck process: dX = θ(μ - X)dt + σdW
            // θ = mean reversion speed, μ = mean, σ = volatility
            let dt = sample_interval;
            let dw = normal.sample(&mut rng) * dt.sqrt();

            let mean_reversion = self.mean_reversion_speed * (self.mean_price - self.current_price);
            let diffusion = self.volatility * dw;

            self.current_price += mean_reversion * dt + diffusion;

            // Add periodic pattern (daily cycle)
            let daily_factor = 1.0 + 0.1 * (2.0 * std::f64::consts::PI * time / 24.0).sin();
            let price_with_periodicity = self.current_price * daily_factor;

            // Clamp price to reasonable range (10% to 95% of on-demand)
            let final_price = price_with_periodicity
                .max(self.on_demand_price * 0.1)
                .min(self.on_demand_price * 0.95);

            // Preemption probability increases when price is low (high demand)
            // Using inverse relationship: lower price → higher preemption risk
            let price_ratio = final_price / self.on_demand_price;
            let preemption_multiplier = (1.0 - price_ratio).max(0.1);
            let preemption_prob = self.base_preemption_rate * preemption_multiplier * dt;

            prices.push(SpotPrice {
                time,
                price: final_price,
                preemption_probability: preemption_prob,
            });
        }

        prices
    }

    /// Generate a simple price trace (deterministic, for testing)
    pub fn generate_simple(duration_hours: f64, spot_price: f64, preemption_rate: f64) -> Vec<SpotPrice> {
        let sample_interval = 1.0; // 1 hour
        let num_samples = duration_hours as usize;

        (0..num_samples)
            .map(|i| SpotPrice {
                time: i as f64,
                price: spot_price,
                preemption_probability: preemption_rate,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_generation() {
        let prices = SpotPriceGenerator::generate_simple(10.0, 0.30, 0.05);
        assert_eq!(prices.len(), 10);
        assert_eq!(prices[0].price, 0.30);
        assert_eq!(prices[0].preemption_probability, 0.05);
    }

    #[test]
    fn test_ou_generation() {
        let mut generator = SpotPriceGenerator::new(0.30, 1.00, 0.05);
        let prices = generator.generate(24.0, 1.0);

        assert_eq!(prices.len(), 24);

        // All prices should be positive and below on-demand
        for price in &prices {
            assert!(price.price > 0.0);
            assert!(price.price < 1.00);
            assert!(price.preemption_probability >= 0.0);
            assert!(price.preemption_probability < 1.0);
        }
    }
}
