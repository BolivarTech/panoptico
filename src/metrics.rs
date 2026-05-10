// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Cost tracking — tracks token usage and calculates review costs.
//!
//! Accumulates [`TokenUsage`] from each
//! API call and calculates estimated costs with cache savings.

use crate::backend::TokenUsage;

/// Number of tokens per million, used in cost calculations.
const TOKENS_PER_MILLION: f64 = 1_000_000.0;

/// Discount factor applied to cache-read tokens (charged at 10% of input price).
const CACHE_READ_DISCOUNT: f64 = 0.9;

/// Accumulated metrics for a complete review session.
#[derive(Debug)]
pub struct ReviewMetrics {
    /// Total input tokens across all batches.
    pub total_input_tokens: u64,
    /// Total output tokens across all batches.
    pub total_output_tokens: u64,
    /// Tokens served from prompt cache.
    pub cache_read_tokens: u64,
    /// Tokens written to prompt cache.
    pub cache_write_tokens: u64,
    /// Number of batches processed.
    pub batch_count: u32,
}

impl Default for ReviewMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ReviewMetrics {
    /// Create a new zeroed-out metrics tracker.
    pub fn new() -> Self {
        Self {
            total_input_tokens: 0,
            total_output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            batch_count: 0,
        }
    }

    /// Accumulate token usage from a single API call.
    ///
    /// # Arguments
    ///
    /// * `usage` - Token usage from one backend call.
    pub fn track(&mut self, usage: &TokenUsage) {
        self.total_input_tokens += u64::from(usage.input_tokens);
        self.total_output_tokens += u64::from(usage.output_tokens);
        self.cache_read_tokens += u64::from(usage.cache_read_input_tokens);
        self.cache_write_tokens += u64::from(usage.cache_creation_input_tokens);
        self.batch_count += 1;
    }

    /// Calculate estimated cost based on per-million-token pricing.
    ///
    /// Applies a 90% discount for cache-read tokens (charged at 0.1x input price).
    ///
    /// # Arguments
    ///
    /// * `input_price` - Price per million input tokens.
    /// * `output_price` - Price per million output tokens.
    ///
    /// # Returns
    ///
    /// Estimated cost in dollars.
    pub fn calculate_cost(&self, input_price: f64, output_price: f64) -> f64 {
        let input = (self.total_input_tokens as f64 / TOKENS_PER_MILLION) * input_price;
        let output = (self.total_output_tokens as f64 / TOKENS_PER_MILLION) * output_price;
        let savings = (self.cache_read_tokens as f64 / TOKENS_PER_MILLION)
            * input_price
            * CACHE_READ_DISCOUNT;
        input + output - savings
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::TokenUsage;

    fn make_usage(input: u32, output: u32, cache_read: u32, cache_write: u32) -> TokenUsage {
        TokenUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_write,
        }
    }

    // --- ReviewMetrics::new ---

    #[test]
    fn new_metrics_are_zeroed() {
        let m = ReviewMetrics::new();
        assert_eq!(m.total_input_tokens, 0u64);
        assert_eq!(m.total_output_tokens, 0u64);
        assert_eq!(m.cache_read_tokens, 0u64);
        assert_eq!(m.cache_write_tokens, 0u64);
        assert_eq!(m.batch_count, 0);
    }

    // --- ReviewMetrics::track ---

    #[test]
    fn track_accumulates_single_usage() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(100, 50, 20, 10));
        assert_eq!(m.total_input_tokens, 100);
        assert_eq!(m.total_output_tokens, 50);
        assert_eq!(m.cache_read_tokens, 20);
        assert_eq!(m.cache_write_tokens, 10);
        assert_eq!(m.batch_count, 1);
    }

    #[test]
    fn track_accumulates_multiple_usages() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(100, 50, 20, 10));
        m.track(&make_usage(200, 100, 40, 0));
        m.track(&make_usage(150, 75, 0, 30));
        assert_eq!(m.total_input_tokens, 450);
        assert_eq!(m.total_output_tokens, 225);
        assert_eq!(m.cache_read_tokens, 60);
        assert_eq!(m.cache_write_tokens, 40);
        assert_eq!(m.batch_count, 3);
    }

    #[test]
    fn track_zero_usage_increments_batch_count() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(0, 0, 0, 0));
        assert_eq!(
            m.batch_count, 1,
            "Even zero-token usage should count as a batch"
        );
    }

    // --- ReviewMetrics::calculate_cost ---

    #[test]
    fn cost_zero_tokens_is_zero() {
        let m = ReviewMetrics::new();
        let cost = m.calculate_cost(3.0, 15.0);
        assert!(
            (cost - 0.0).abs() < f64::EPSILON,
            "Zero tokens should cost $0.00"
        );
    }

    #[test]
    fn cost_input_only() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(1_000_000, 0, 0, 0));
        // 1M input tokens * $3/M = $3.00
        let cost = m.calculate_cost(3.0, 15.0);
        assert!(
            (cost - 3.0).abs() < 0.001,
            "1M input tokens at $3/M should cost $3.00, got {}",
            cost
        );
    }

    #[test]
    fn cost_output_only() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(0, 1_000_000, 0, 0));
        // 1M output tokens * $15/M = $15.00
        let cost = m.calculate_cost(3.0, 15.0);
        assert!(
            (cost - 15.0).abs() < 0.001,
            "1M output tokens at $15/M should cost $15.00, got {}",
            cost
        );
    }

    #[test]
    fn cost_with_cache_read_savings() {
        let mut m = ReviewMetrics::new();
        // 1M input, 0 output, 500K from cache
        m.track(&make_usage(1_000_000, 0, 500_000, 0));
        // Input cost: 1M * $3/M = $3.00
        // Cache savings: 500K * $3/M * 0.9 = $1.35
        // Total: $3.00 - $1.35 = $1.65
        let cost = m.calculate_cost(3.0, 15.0);
        assert!(
            (cost - 1.65).abs() < 0.001,
            "Cache read should save 90% of input cost for cached tokens, got {}",
            cost
        );
    }

    #[test]
    fn cost_mixed_tokens() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(500_000, 100_000, 200_000, 50_000));
        // Input: 500K * $3/M = $1.50
        // Output: 100K * $15/M = $1.50
        // Cache savings: 200K * $3/M * 0.9 = $0.54
        // Total: $1.50 + $1.50 - $0.54 = $2.46
        let cost = m.calculate_cost(3.0, 15.0);
        assert!(
            (cost - 2.46).abs() < 0.001,
            "Mixed token cost should be $2.46, got {}",
            cost
        );
    }

    #[test]
    fn cost_scales_with_different_prices() {
        let mut m = ReviewMetrics::new();
        m.track(&make_usage(1_000_000, 1_000_000, 0, 0));
        // Haiku pricing: $1/M input, $5/M output
        let cost = m.calculate_cost(1.0, 5.0);
        assert!(
            (cost - 6.0).abs() < 0.001,
            "1M each at Haiku prices ($1/$5) should cost $6.00, got {}",
            cost
        );
    }

    #[test]
    fn track_large_accumulation_does_not_overflow() {
        let mut m = ReviewMetrics::new();
        let large = make_usage(u32::MAX, u32::MAX, u32::MAX, u32::MAX);
        m.track(&large);
        m.track(&large);
        let expected = u64::from(u32::MAX) * 2;
        assert_eq!(m.total_input_tokens, expected);
        assert_eq!(m.total_output_tokens, expected);
        assert_eq!(m.cache_read_tokens, expected);
        assert_eq!(m.cache_write_tokens, expected);
    }
}
