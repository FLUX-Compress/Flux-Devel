//! Entropy estimation utilities
//!
//! Provides the `EntropyEstimator` to compute the Shannon entropy of raw data blocks,
//! enabling fast heuristics to detect incompressibility or locate low-entropy/structured runs.

/// An estimator that tracks byte frequency distributions to calculate Shannon entropy.
///
/// # Thread Safety
/// This struct is not thread-safe by itself (`!Sync`), but since it is owned and used
/// entirely within a single classifier thread or analysis worker, it does not require
/// internal locking. The classification pipeline orchestrates ownership across threads safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntropyEstimator {
    /// Stores the occurrence counts for each byte value (0–255).
    pub byte_histogram: [u64; 256],
    /// The total number of bytes processed by the estimator.
    pub total_bytes: u64,
}

impl Default for EntropyEstimator {
    fn default() -> Self {
        Self::new()
    }
}

impl EntropyEstimator {
    /// Creates a new, zeroed `EntropyEstimator`.
    pub fn new() -> Self {
        Self {
            byte_histogram: [0; 256],
            total_bytes: 0,
        }
    }

    /// Feeds a slice of bytes into the estimator, updating the histogram.
    ///
    /// # Parameters
    /// * `data`: Slices of raw bytes to inspect.
    pub fn update(&mut self, data: &[u8]) {
        for &byte in data {
            self.byte_histogram[byte as usize] += 1;
        }
        self.total_bytes += data.len() as u64;
    }

    /// Resets the histogram and total byte counter, preparing it for reuse on a new block.
    pub fn reset(&mut self) {
        self.byte_histogram = [0; 256];
        self.total_bytes = 0;
    }

    /// Computes and returns the Shannon entropy of the accumulated data in bits per byte.
    ///
    /// The return value is in the range `[0.0, 8.0]`:
    /// * `0.0` represents perfectly compressible data (where all processed bytes are identical).
    /// * `8.0` represents a perfectly uniform distribution (ideal random noise or incompressible data).
    ///
    /// # The Shannon Entropy Formula
    /// Shannon entropy $H(X)$ measures the average amount of information produced by a stochastic data source.
    ///
    /// $$H(X) = -\sum_{i=0}^{255} p(x_i) \log_2 p(x_i)$$
    ///
    /// Where:
    /// * $x_i$ is each possible byte value (0 to 255).
    /// * $p(x_i)$ is the probability (frequency count / total bytes) of byte $x_i$ appearing in the input.
    /// * $\log_2 p(x_i)$ is the base-2 logarithm, representing information content in bits.
    ///
    /// # How Entropy Drives Pipeline Selection
    /// * **Low Entropy (< 4.5 bits/byte)**: The block contains high redundancy (e.g. structured text, logs, code,
    ///   or highly repetitive patterns). We route this to the **Text/Structured Pipeline** (BWT + PPMd + rANS)
    ///   where predictive model contexts can achieve extremely high compression ratios.
    /// * **Medium Entropy (4.5 to 6.5 bits/byte)**: The block contains typical compiled executables, structured data,
    ///   or mixed objects. We route this to the **Binary Pipeline** (LZ77 + PPMd + rANS) to balance dictionary matching
    ///   with adaptive predictive models.
    /// * **High Entropy (6.5 to 7.95 bits/byte)**: The block contains raw multimedia waveforms or floating-point arrays.
    ///   We check for stride correlations and route it to the **Multimedia Pipeline** (Adaptive Stride Transposition + LZ77 + SSE + rANS)
    ///   to decorrelate strides before symbol modeling.
    /// * **Extreme Entropy (> 7.95 bits/byte)**: The block is already compressed or encrypted. We skip the pipeline and **Store Raw**.
    pub fn shannon_entropy(&self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }

        let total = self.total_bytes as f64;
        let mut entropy = 0.0;

        for &count in &self.byte_histogram {
            if count > 0 {
                let p = count as f64 / total;
                entropy -= p * p.log2();
            }
        }

        entropy as f32
    }

    /// Evaluates whether the data block is incompressible.
    ///
    /// Returns `true` if the Shannon entropy is strictly greater than `7.95` bits per byte.
    ///
    /// # Why 7.95 rather than 8.0?
    /// A theoretically pure random byte sequence has an entropy of `8.0`. In practice, compressed archives (e.g. `.zip`,
    /// `.tar.xz`) or encrypted streams contain structured headers, metadata blocks, or cipher alignment blocks that introduce
    /// extremely minor, non-compressible structures. This keeps their empirical entropy slightly below `8.0` (typically
    /// around `7.96` to `7.99`). Setting the threshold at `7.95` catches these files reliably, avoiding useless, CPU-intensive
    /// compression attempts on data that cannot be compressed further.
    pub fn is_incompressible(&self) -> bool {
        self.shannon_entropy() > 7.95
    }

    /// Computes a rough estimate of the achievable compression ratio based on entropy.
    ///
    /// Returns a ratio in the range `[0.0, 1.0]`, representing the predicted output size relative to the input size
    /// (e.g. `0.2` means we predict the output will be 20% of the original size).
    ///
    /// # Rationale
    /// Calculated as:
    ///
    /// $$\text{Estimate} = \frac{H(X)}{8.0}$$
    ///
    /// This assumes a theoretical zero-redundancy coding limit (Shannon's Source Coding Theorem), where the minimum average
    /// code length approaches the source entropy.
    pub fn compression_estimate(&self) -> f32 {
        let entropy = self.shannon_entropy();
        (entropy / 8.0).clamp(0.0, 1.0)
    }

    /// Checks if a single byte value dominates the stream, representing a dense zero or repeating sequence.
    ///
    /// Returns `Some(byte)` if a single byte comprises strictly more than 40% of the analyzed data.
    /// Otherwise, returns `None`.
    pub fn dominant_byte_run(&self) -> Option<u8> {
        if self.total_bytes == 0 {
            return None;
        }

        let threshold = (self.total_bytes as f64 * 0.40) as u64;
        let mut max_count = 0;
        let mut max_byte = None;

        for (byte_val, &count) in self.byte_histogram.iter().enumerate() {
            if count > max_count {
                max_count = count;
                max_byte = Some(byte_val as u8);
            }
        }

        if max_count > threshold {
            max_byte
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entropy_zero_bytes() {
        let estimator = EntropyEstimator::new();
        assert_eq!(estimator.shannon_entropy(), 0.0);
        assert_eq!(estimator.compression_estimate(), 0.0);
        assert!(!estimator.is_incompressible());
        assert_eq!(estimator.dominant_byte_run(), None);
    }

    #[test]
    fn test_entropy_constant_stream() {
        let mut estimator = EntropyEstimator::new();
        estimator.update(&[42; 100]);
        // All bytes are identical; entropy should be 0
        assert_eq!(estimator.shannon_entropy(), 0.0);
        assert_eq!(estimator.compression_estimate(), 0.0);
        assert_eq!(estimator.dominant_byte_run(), Some(42));
    }

    #[test]
    fn test_entropy_random_stream() {
        let mut estimator = EntropyEstimator::new();
        // Generate uniform byte frequency
        let mut data = Vec::with_capacity(256);
        for i in 0..256 {
            data.push(i as u8);
        }
        estimator.update(&data);
        // Shannon entropy of uniform distribution of 256 states is 8.0
        assert_eq!(estimator.shannon_entropy(), 8.0);
        assert_eq!(estimator.compression_estimate(), 1.0);
        assert!(estimator.is_incompressible());
        assert_eq!(estimator.dominant_byte_run(), None);
    }

    #[test]
    fn test_entropy_mixed_stream() {
        let mut estimator = EntropyEstimator::new();
        // 45% of byte 0, 55% of byte 1
        let mut data = vec![0; 45];
        data.extend(vec![1; 55]);
        estimator.update(&data);
        let h = estimator.shannon_entropy();
        // H = -0.45 * log2(0.45) - 0.55 * log2(0.55) approx 0.99
        assert!(h > 0.98 && h < 1.0);
        assert_eq!(estimator.dominant_byte_run(), Some(1)); // 55% is > 40%
    }

    #[test]
    fn test_entropy_reset() {
        let mut estimator = EntropyEstimator::new();
        estimator.update(&[5; 10]);
        assert_eq!(estimator.total_bytes, 10);
        estimator.reset();
        assert_eq!(estimator.total_bytes, 0);
        assert_eq!(estimator.shannon_entropy(), 0.0);
    }
}
