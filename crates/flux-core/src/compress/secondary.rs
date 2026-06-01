//! Secondary Symbol Estimator (SSE)
//!
//! Models cross-plane correlations and stride-periodic patterns in preprocessed/transposed data.
//!
//! ## Physical Meaning of Cross-Plane Correlation
//! In transposed multimedia data (like floating-point arrays, RGB image channels, or multi-channel audio),
//! different planes (or channels) exhibit strong structural correlations.
//! For example, in 32-bit float arrays:
//! - If the exponent byte (plane A) is high, the mantissa bytes (plane B) tend to cluster in specific ranges.
//! - For RGB pixels, a high red value often correlates with a high green value.
//!
//! Physically, cross-plane correlation means that knowing the value of a byte in an adjacent plane
//! provides substantial information about the expected value of the byte in the current plane.
//!
//! ## Warmup Threshold
//! The Secondary Estimator starts with no knowledge of the data stream. Before it has seen enough
//! samples, its statistical estimates are highly volatile and inaccurate. We enforce a warmup threshold
//! of 1024 total observations. Below this threshold, `predict` returns a neutral `0.5` probability,
//! which does not bias the mixer's probability blending.
//!
//! ## Blending with PPM
//! In Step 8, the mixer blends the probability distribution from the PPM model with the SSE prediction.
//! This allows FLUX to dynamically shift coding weights towards plane-correlated values when they are
//! detected.

use std::collections::HashMap;

/// Secondary Symbol Estimator.
pub struct SecondaryEstimator {
    /// Maps (plane_index, byte_value) to frequency count.
    /// Captures the marginal frequency distribution within each transposed plane.
    pub stride_model: HashMap<(u8, u8), u32>,
    /// Maps (plane_a_value, plane_b_value, plane_index) to joint frequency.
    /// Captures cross-plane correlations between adjacent planes.
    pub cross_plane_model: HashMap<(u8, u8, u8), u32>,
    /// Stride length currently active in the stream.
    pub active_stride: u8,
    /// Total number of observations recorded.
    pub total_observations: u64,
}

impl SecondaryEstimator {
    /// Creates a new Secondary Symbol Estimator with the given stride.
    pub fn new(stride: u8) -> Self {
        Self {
            stride_model: HashMap::new(),
            cross_plane_model: HashMap::new(),
            active_stride: stride,
            total_observations: 0,
        }
    }

    /// Records an observation to update the frequency models.
    pub fn observe(&mut self, plane: u8, value: u8, _prev_value: u8, adjacent_plane_value: u8) {
        // Record marginal frequency for the byte in this plane
        *self.stride_model.entry((plane, value)).or_insert(0) += 1;

        // Record joint frequency between adjacent plane value and current plane value
        *self.cross_plane_model.entry((adjacent_plane_value, value, plane)).or_insert(0) += 1;

        self.total_observations += 1;
    }

    /// Returns a probability estimate for the next value in this plane.
    ///
    /// Evaluates the likelihood of `prev_value` (stride pattern) and `adjacent_plane_value` (cross-plane pattern)
    /// based on observed marginal and conditional frequencies.
    /// Returns 0.5 if total observations are below 1024 (neutral warmup phase).
    pub fn predict(&self, plane: u8, prev_value: u8, adjacent_plane_value: u8) -> f32 {
        if !self.has_sufficient_data() {
            return 0.5;
        }

        // Stride probability: how likely is the next byte to be equal to prev_value?
        let stride_count = *self.stride_model.get(&(plane, prev_value)).unwrap_or(&0) as f64;
        let stride_total = self.stride_model.iter()
            .filter(|&(&(p, _), _)| p == plane)
            .map(|(_, &c)| c as f64)
            .sum::<f64>();
        let p_stride = if stride_total > 0.0 { stride_count / stride_total } else { 0.5 };

        // Cross-plane probability: how likely is the next byte to be equal to adjacent_plane_value?
        let cross_count = *self.cross_plane_model.get(&(adjacent_plane_value, adjacent_plane_value, plane)).unwrap_or(&0) as f64;
        let cross_total = self.cross_plane_model.iter()
            .filter(|&(&(pa, _, pi), _)| pa == adjacent_plane_value && pi == plane)
            .map(|(_, &c)| c as f64)
            .sum::<f64>();
        let p_cross = if cross_total > 0.0 { cross_count / cross_total } else { 0.5 };

        // Take the maximum correlation strength to highlight any learned pattern
        let p = p_stride.max(p_cross);
        p as f32
    }

    /// Returns true if the model has warmed up with more than 1024 observations.
    pub fn has_sufficient_data(&self) -> bool {
        self.total_observations > 1024
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_secondary_neutral_before_warmup() {
        let mut estimator = SecondaryEstimator::new(4);
        
        // Feed 500 observations (below the 1024 warmup threshold)
        for _ in 0..500 {
            estimator.observe(0, 42, 42, 100);
        }

        assert!(!estimator.has_sufficient_data());
        assert_eq!(estimator.predict(0, 42, 100), 0.5);
    }

    #[test]
    fn test_secondary_learns_pattern() {
        let mut estimator = SecondaryEstimator::new(4);
        
        // Feed 1100 observations of the same repeating byte (42) in plane 0
        for _ in 0..1100 {
            estimator.observe(0, 42, 42, 100);
        }

        assert!(estimator.has_sufficient_data());
        // Stride probability for 42 should be very high, making overall prediction close to 1.0 (> 0.5)
        let pred = estimator.predict(0, 42, 100);
        assert!(pred > 0.8, "Prediction was {}", pred);
    }

    #[test]
    fn test_secondary_cross_plane_correlation() {
        let mut estimator = SecondaryEstimator::new(4);
        
        // Feed 1100 observations where value in plane 0 (100) matches adjacent plane (100)
        for _ in 0..1100 {
            estimator.observe(0, 100, 42, 100);
        }

        assert!(estimator.has_sufficient_data());
        // Cross-plane correlation should predict 100 with high probability
        let pred = estimator.predict(0, 42, 100);
        assert!(pred > 0.8, "Prediction was {}", pred);
    }
}
