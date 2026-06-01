//! # Context Mixer
//!
//! Blends probabilities from the PPM (Prediction by Partial Matching) model and the
//! Secondary Symbol Estimator (SSE) using adaptive weights.
//!
//! ## Adaptive Weight Mixing and PAQ's Approach
//!
//! In high-performance compression codecs (like the PAQ series), mixing the predictions
//! of multiple independent models is the key to achieving state-of-the-art compression ratios.
//! Instead of using a static combination rule, FLUX uses adaptive weight mixing:
//! - We assign a weight $w_i$ to each model.
//! - For each encoded symbol, the blended probability for symbol $s$ is the weighted sum:
//!   $$P_{blended}(s) = w_{ppm} \cdot P_{ppm}(s) + w_{sec} \cdot P_{sec}(s)$$
//! - After observing the actual symbol $S$, we evaluate the accuracy of each model. If a model
//!   assigned a high probability to $S$, its prediction error is small, and we increase its weight.
//!   If it assigned a low probability, its prediction error is large, and we decrease its weight.
//! - The weights are updated using a gradient descent rule on the squared prediction error:
//!   $$error_i = 1.0 - P_i(S) \quad \text{if correct (predicted highest prob)}$$
//!   $$error_i = P_i(S) \quad \text{if incorrect}$$
//! - Weights are clamped to the interval `[0.05, 0.95]`. This ensures that neither model is
//!   ever completely discarded, allowing the mixer to remain resilient and adapt if data
//!   characteristics change midway through a block.
//!
//! ## Weight Reset
//!
//! Weights are reset to `0.5 / 0.5` at the start of each block rather than each file. This allows
//! the mixer to adapt to the unique local statistics of each individual block independently,
//! which is crucial for heterogeneous datasets.

use std::collections::VecDeque;
use crate::compress::ppm::PpmEvent;
use crate::compress::rans::ProbabilityTable;

/// Represents the weight and error tracking for an individual model.
#[derive(Clone, Debug, PartialEq)]
pub struct MixerWeight {
    /// The current model weight in `[0.05, 0.95]`.
    pub weight: f32,
    /// Rate at which the weight adapts.
    pub learning_rate: f32,
    /// Exponential moving average of prediction error.
    pub recent_error: f32,
}

impl MixerWeight {
    /// Creates a new MixerWeight.
    pub fn new(initial_weight: f32, learning_rate: f32) -> Self {
        Self {
            weight: initial_weight.clamp(0.05, 0.95),
            learning_rate,
            recent_error: 0.5,
        }
    }

    /// Adjusts the weight based on prediction quality.
    ///
    /// The update formula rewards high-probability correct predictions and penalizes
    /// wrong predictions using the learning rate.
    pub fn update(&mut self, predicted_prob: f32, actual_symbol_was_correct: bool) {
        let error = if actual_symbol_was_correct {
            1.0 - predicted_prob
        } else {
            1.0
        };

        // Update exponential moving average of the error
        self.recent_error = (1.0 - self.learning_rate) * self.recent_error + self.learning_rate * error;

        // Perform gradient update
        if actual_symbol_was_correct {
            // Reward high confidence correct prediction
            self.weight += self.learning_rate * (1.0 - self.weight) * predicted_prob;
        } else {
            // Penalize incorrect predictions or low-confidence
            self.weight -= self.learning_rate * self.weight * (1.0 - predicted_prob);
        }

        // Clamp weight to guarantee neither model is fully excluded
        self.weight = self.weight.clamp(0.05, 0.95);
    }
}

/// The context mixer combining PPM and Secondary Symbol Estimator probabilities.
pub struct ContextMixer {
    /// Blend weight for the PPM model.
    pub ppm_weight: MixerWeight,
    /// Blend weight for the Secondary Symbol Estimator.
    pub secondary_weight: MixerWeight,
    /// Blend history storing recent (ppm_prob, secondary_prob) pairs.
    pub blend_history: VecDeque<(f32, f32)>,
    /// Counter of total mixed symbols.
    pub symbols_mixed: u64,
}

impl ContextMixer {
    /// Creates a new ContextMixer with equal model weights.
    pub fn new() -> Self {
        Self {
            ppm_weight: MixerWeight::new(0.5, 0.05),
            secondary_weight: MixerWeight::new(0.5, 0.05),
            blend_history: VecDeque::with_capacity(100),
            symbols_mixed: 0,
        }
    }

    /// Blends PPM probabilities with the secondary estimator prediction for a target symbol.
    ///
    /// PPM outputs a distribution for all 256 symbols. The secondary estimator outputs a single
    /// prediction probability for the target symbol. We construct a 256-symbol distribution
    /// for the secondary model by distributing the remaining probability mass uniformly across
    /// the remaining 255 symbols.
    ///
    /// After blending, weights are updated based on prediction success.
    pub fn blend(
        &mut self,
        ppm_probs: &[(u8, f32)],
        secondary_prob: f32,
        symbol: u8,
    ) -> Vec<(u8, f32)> {
        let mut ppm_map = [0.0f32; 256];
        for &(sym, p) in ppm_probs {
            ppm_map[sym as usize] = p;
        }

        let target_idx = symbol as usize;
        let w_ppm = self.ppm_weight.weight;
        let w_sec = self.secondary_weight.weight;

        // 1. Blend predictions
        let mut raw_blended = [0.0f32; 256];
        for (s, item) in raw_blended.iter_mut().enumerate() {
            let p_ppm = ppm_map[s];
            let p_sec = if s == target_idx {
                secondary_prob
            } else {
                (1.0 - secondary_prob) / 255.0
            };
            *item = w_ppm * p_ppm + w_sec * p_sec;
        }

        // 2. Normalize distribution
        let total_sum: f32 = raw_blended.iter().sum();
        let normalizer = if total_sum > 0.0 { 1.0 / total_sum } else { 1.0 };
        let mut blended = Vec::with_capacity(256);
        for (s, &val) in raw_blended.iter().enumerate() {
            blended.push((s as u8, val * normalizer));
        }

        // 3. Determine if models predicted correctly (predicted highest probability)
        let ppm_max_sym = ppm_probs.iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|&(sym, _)| sym);
        let ppm_was_correct = ppm_max_sym == Some(symbol);

        let sec_was_correct = secondary_prob > (1.0 - secondary_prob) / 255.0;

        // 4. Update model weights
        self.ppm_weight.update(ppm_map[target_idx], ppm_was_correct);
        self.secondary_weight.update(secondary_prob, sec_was_correct);

        // 5. Update history
        self.blend_history.push_back((ppm_map[target_idx], secondary_prob));
        if self.blend_history.len() > 100 {
            self.blend_history.pop_front();
        }
        self.symbols_mixed += 1;

        blended
    }

    /// Convenience method to blend probabilities and build a rANS `ProbabilityTable`.
    pub fn blend_to_table(
        &mut self,
        ppm_events: &[PpmEvent],
        secondary_prob: f32,
        symbol: u8,
    ) -> ProbabilityTable {
        // Retrieve symbol probability from PPM events
        let ppm_prob = ppm_events.iter()
            .find_map(|event| match event {
                PpmEvent::Symbol { symbol: sym, probability, .. } if *sym == symbol => Some(*probability),
                _ => None,
            })
            .unwrap_or(1.0 / 256.0);

        // Build mock PPM distribution from the known symbol event
        let mut ppm_probs = Vec::with_capacity(256);
        for s in 0..256 {
            let p = if s == symbol as usize {
                ppm_prob
            } else {
                (1.0 - ppm_prob) / 255.0
            };
            ppm_probs.push((s as u8, p));
        }

        let blended = self.blend(&ppm_probs, secondary_prob, symbol);
        ProbabilityTable::from_probabilities(&blended)
    }

    /// Resets the blend weights to `0.5 / 0.5` at the start of a block.
    pub fn reset(&mut self) {
        self.ppm_weight = MixerWeight::new(0.5, 0.05);
        self.secondary_weight = MixerWeight::new(0.5, 0.05);
        self.blend_history.clear();
        self.symbols_mixed = 0;
    }
}

impl Default for ContextMixer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mixer_equal_weights_initially() {
        let mixer = ContextMixer::new();
        assert_eq!(mixer.ppm_weight.weight, 0.5);
        assert_eq!(mixer.secondary_weight.weight, 0.5);
    }

    #[test]
    fn test_mixer_weight_increases_for_good_prediction() {
        let mut weight = MixerWeight::new(0.5, 0.1);
        
        // Good prediction: correct and high probability
        weight.update(0.8, true);
        assert!(weight.weight > 0.5);
    }

    #[test]
    fn test_mixer_weight_decreases_for_bad_prediction() {
        let mut weight = MixerWeight::new(0.5, 0.1);
        
        // Bad prediction: incorrect with low probability
        weight.update(0.1, false);
        assert!(weight.weight < 0.5);
    }

    #[test]
    fn test_mixer_weights_clamped() {
        let mut weight = MixerWeight::new(0.5, 0.5);

        // Force increase to maximum
        for _ in 0..20 {
            weight.update(0.99, true);
        }
        assert!(weight.weight <= 0.95);

        // Force decrease to minimum
        for _ in 0..20 {
            weight.update(0.01, false);
        }
        assert!(weight.weight >= 0.05);
    }

    #[test]
    fn test_mixer_blended_distribution_sums_to_one() {
        let mut mixer = ContextMixer::new();
        let mut ppm_probs = Vec::new();
        for i in 0..256 {
            ppm_probs.push((i as u8, 1.0 / 256.0));
        }

        let blended = mixer.blend(&ppm_probs, 0.5, 42);
        let sum: f32 = blended.iter().map(|&(_, p)| p).sum();
        assert!((sum - 1.0).abs() < 1e-4);
    }

    #[test]
    fn test_mixer_adapts_over_sequence() {
        let mut mixer = ContextMixer::new();
        
        // Simulating a sequence where PPM performs extremely well (high probability, correct)
        // and secondary performs poorly (low probability, incorrect)
        let mut ppm_probs = vec![(0u8, 0.9)];
        for i in 1..256 {
            ppm_probs.push((i as u8, 0.1 / 255.0));
        }

        for _ in 0..10 {
            mixer.blend(&ppm_probs, 0.001, 0);
        }

        // PPM weight should have shifted significantly higher than secondary weight
        assert!(mixer.ppm_weight.weight > 0.6);
        assert!(mixer.secondary_weight.weight < 0.4);
    }
}
