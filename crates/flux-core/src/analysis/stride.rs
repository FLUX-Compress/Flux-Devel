//! Stride detection foundation and autocorrelation analysis
//!
//! Detects repeating byte stride patterns (e.g., interleaved multi-channel audio,
//! raw float tables, structured structs) using Pearson autocorrelation coefficients.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::buffer::window::SlidingWindow;
use crate::threads::signals::{StrideSignal, CompressionSignal};

/// Confidence threshold required to confirm a stride candidate in a single window.
///
/// # Rationale for 0.75
/// A value of 0.75 balances noise resilience with sensitivity. Random binary data or normal text
/// rarely exceeds 0.3 correlation. Structured multi-channel waveforms (such as stereo 16-bit PCM WAV
/// with stride 4) or 32-bit floats (stride 4 or 8) consistently exhibit autocorrelation values
/// above 0.85. The 0.75 threshold ensures we filter out false positives from noise while reliably
/// catching real multidimensional strides.
pub const STRIDE_CONFIRMATION_THRESHOLD: f32 = 0.75;

/// Number of consecutive windows that must exceed the threshold before confirming a stride.
///
/// # Rationale for 3 Windows
/// Requiring confirmation across 3 consecutive windows prevents transient/accidental repeating
/// byte patterns from triggering a stride pipeline reassignment. It guarantees that the stride is
/// a stable characteristic of the file block, justifying the cost of the transpose operation.
pub const STRIDE_CONFIRMATION_WINDOWS: u8 = 3;

/// Number of bytes to process before re-examining the stride characteristics.
///
/// Sets the interval (64KB) at which the stride analyzer wakes up to re-evaluate the data stream.
pub const STRIDE_REEXAMINE_INTERVAL: usize = 65536;

/// Operating mode for the stride detector, governing evaluation frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisMode {
    /// Full analysis every `STRIDE_REEXAMINE_INTERVAL` bytes.
    /// Used during initial startup or when a drift is detected.
    Hot,
    /// Analysis every `4 * STRIDE_REEXAMINE_INTERVAL` bytes.
    /// Triggered when stability is first confirmed.
    Warm,
    /// Analysis every `16 * STRIDE_REEXAMINE_INTERVAL` bytes.
    /// Pattern confirmed stable; runs minimal background loops to conserve CPU.
    Cold,
    /// Stop all analysis loops and terminate execution thread.
    Shutdown,
}

/// Represents a candidate stride size (from 1 to 16 bytes) under evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrideCandidate {
    /// The stride length (in bytes) being tested.
    pub stride: u8,
    /// The Pearson autocorrelation coefficient computed for this stride.
    pub correlation: f32,
    /// Number of consecutive sliding windows that have confirmed this candidate.
    pub confirmation_count: u8,
    /// Indicates whether this candidate is confirmed as the stable stride.
    pub is_confirmed: bool,
}

/// Computes stride characteristics over the lookahead buffer windows.
///
/// # Thread Safety
/// This struct is intended to be owned and driven by the dedicated Adaptive Stride Analyzer
/// thread. Its candidates are updated thread-locally, and the confirmed stride results
/// are published to the shared `SlidingWindow` via atomic stores, ensuring complete thread safety.
#[derive(Debug)]
pub struct StrideDetector {
    /// The array of candidate strides from 1 to 16.
    pub candidates: [StrideCandidate; 16],
    /// The currently confirmed stable stride (if any).
    pub current_confirmed: Option<u8>,
    /// Bytes processed since the last complete autocorrelation sweep.
    pub bytes_since_last_check: usize,
    /// Shared reference to the sliding window of data.
    pub window: Arc<SlidingWindow>,
    /// Current operating mode of the analyzer.
    pub mode: AnalysisMode,
    /// Channel for sending stride updates to the backwards analysis thread and pipeline.
    pub mode_tx: crossbeam::channel::Sender<StrideSignal>,
    /// Channel for receiving stability feedback updates.
    pub stability_rx: crossbeam::channel::Receiver<StrideSignal>,
    /// Channel for receiving system-wide compression commands (Shutdown).
    pub shutdown_rx: crossbeam::channel::Receiver<CompressionSignal>,
    /// Total bytes analyzed throughout the lifetime of this detector.
    pub bytes_analyzed_total: u64,
    /// Number of mode transitions occurred (for diagnostic reporting).
    pub mode_transitions: u32,
}

impl StrideDetector {
    /// Creates a new `StrideDetector` bound to the given sliding window context and channels.
    pub fn new(
        window: Arc<SlidingWindow>,
        mode_tx: crossbeam::channel::Sender<StrideSignal>,
        stability_rx: crossbeam::channel::Receiver<StrideSignal>,
        shutdown_rx: crossbeam::channel::Receiver<CompressionSignal>,
    ) -> Self {
        let mut candidates = [StrideCandidate {
            stride: 0,
            correlation: 0.0,
            confirmation_count: 0,
            is_confirmed: false,
        }; 16];

        for (i, candidate) in candidates.iter_mut().enumerate() {
            candidate.stride = (i + 1) as u8;
        }

        Self {
            candidates,
            current_confirmed: None,
            bytes_since_last_check: 0,
            window,
            mode: AnalysisMode::Hot,
            mode_tx,
            stability_rx,
            shutdown_rx,
            bytes_analyzed_total: 0,
            mode_transitions: 0,
        }
    }

    /// Computes the Pearson autocorrelation coefficient for a specific stride.
    ///
    /// Delegates to the shared `compute_pearson_autocorrelation` function.
    pub fn compute_autocorrelation(&self, data: &[u8], stride: u8) -> f32 {
        compute_pearson_autocorrelation(data, stride)
    }

    /// Evaluates all 16 stride candidates against the sample block.
    ///
    /// Computes autocorrelation coefficients, increments/resets confirmation counts,
    /// and updates the active confirmed stride.
    pub fn test_all_candidates(&mut self, sample: &[u8]) {
        for i in 0..16 {
            let stride = self.candidates[i].stride;
            let corr = self.compute_autocorrelation(sample, stride);
            self.candidates[i].correlation = corr;
            if corr >= STRIDE_CONFIRMATION_THRESHOLD {
                self.candidates[i].confirmation_count = self.candidates[i].confirmation_count.saturating_add(1);
            } else {
                self.candidates[i].confirmation_count = 0;
            }
            self.candidates[i].is_confirmed = self.candidates[i].confirmation_count >= STRIDE_CONFIRMATION_WINDOWS;
        }

        // Keep track of the confirmed candidate with highest correlation
        self.current_confirmed = self.best_candidate().map(|c| c.stride);

        // Update the SlidingWindow's hint for the compression threads to consume
        if let Some(stride) = self.current_confirmed {
            self.window.set_stride_hint(stride);
        } else {
            self.window.set_stride_hint(0);
        }

        self.bytes_since_last_check = 0;
    }

    /// Returns a reference to the confirmed candidate with the highest correlation score.
    ///
    /// If multiple candidates have the same highest correlation score, the tie is broken
    /// by selecting the candidate with the smallest stride length (the fundamental period).
    ///
    /// Returns `None` if no candidate has met the required confirmation criteria.
    pub fn best_candidate(&self) -> Option<&StrideCandidate> {
        self.candidates
            .iter()
            .filter(|c| c.is_confirmed)
            .max_by(|a, b| {
                a.correlation
                    .partial_cmp(&b.correlation)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.stride.cmp(&a.stride))
            })
    }

    /// Indicates whether enough bytes have passed to trigger a stride re-evaluation.
    pub fn should_reexamine(&self) -> bool {
        let threshold = match self.mode {
            AnalysisMode::Hot => STRIDE_REEXAMINE_INTERVAL,
            AnalysisMode::Warm => 4 * STRIDE_REEXAMINE_INTERVAL,
            AnalysisMode::Cold => 16 * STRIDE_REEXAMINE_INTERVAL,
            AnalysisMode::Shutdown => 0,
        };
        self.bytes_since_last_check >= threshold
    }

    /// Main loop for the stride analyzer thread.
    ///
    /// Performs periodic scans of lookahead data, dynamically adjusting frequency and sleep intervals
    /// based on stability feedback signals.
    pub fn run(&mut self, window: Arc<SlidingWindow>) {
        let mut last_start = window.window_start.load(Ordering::Acquire);
        loop {
            // 10. Check for CompressionSignal::Shutdown
            if self.mode == AnalysisMode::Shutdown {
                break;
            }
            match self.shutdown_rx.try_recv() {
                Ok(CompressionSignal::Shutdown) => {
                    self.transition_mode(AnalysisMode::Shutdown);
                    break;
                }
                Err(crossbeam::channel::TryRecvError::Disconnected) => {
                    self.transition_mode(AnalysisMode::Shutdown);
                    break;
                }
                _ => {}
            }

            // 1. Check stability_rx for signals from backwards analysis thread
            while let Ok(sig) = self.stability_rx.try_recv() {
                match sig {
                    // 2. If StrideSignal::Stable(_) received → transition toward Cold mode
                    StrideSignal::Stable(_) => {
                        let next_mode = match self.mode {
                            AnalysisMode::Hot => AnalysisMode::Warm,
                            AnalysisMode::Warm => AnalysisMode::Cold,
                            AnalysisMode::Cold => AnalysisMode::Cold,
                            AnalysisMode::Shutdown => AnalysisMode::Shutdown,
                        };
                        self.transition_mode(next_mode);
                    }
                    // 3. If StrideSignal::Drifted received → transition to Hot mode immediately
                    StrideSignal::Drifted => {
                        self.transition_mode(AnalysisMode::Hot);
                    }
                    _ => {}
                }
            }

            // Track how many bytes the compression head has advanced
            let current_start = window.window_start.load(Ordering::Acquire);
            let advanced = current_start.saturating_sub(last_start);
            self.bytes_since_last_check += advanced;
            last_start = current_start;

            if self.should_reexamine() {
                // 4. Determine sample size based on current AnalysisMode
                let sample_size = match self.mode {
                    AnalysisMode::Hot => 4096,
                    AnalysisMode::Warm => 2048,
                    AnalysisMode::Cold => 1024,
                    AnalysisMode::Shutdown => 0,
                };

                let bytes_ahead = window.bytes_ahead();
                if bytes_ahead >= sample_size {
                    let old_stride = self.current_confirmed;

                    // 5. Take sample from window using peek()
                    let sample = window.peek(0, sample_size);

                    // 6. Call test_all_candidates() on sample
                    self.test_all_candidates(sample);
                    self.bytes_analyzed_total += sample_size as u64;

                    let new_stride = self.current_confirmed;

                    // 7. If new stride confirmed → send StrideSignal::Stable(stride) on mode_tx
                    // 8. If stride lost → send StrideSignal::Drifted on mode_tx
                    match (old_stride, new_stride) {
                        (None, Some(s)) => {
                            let _ = self.mode_tx.send(StrideSignal::Stable(s));
                        }
                        (Some(old_s), Some(new_s)) if old_s != new_s => {
                            let _ = self.mode_tx.send(StrideSignal::Stable(new_s));
                        }
                        (Some(_), None) => {
                            let _ = self.mode_tx.send(StrideSignal::Drifted);
                        }
                        _ => {}
                    }
                } else if bytes_ahead == 0 && window.bytes_behind() > 0 {
                    // Reached the end of available streaming data
                    break;
                }
            }

            // 9. Sleep according to mode (Hot=no sleep/yield if no work, Warm=brief, Cold=longer)
            match self.mode {
                AnalysisMode::Hot => {
                    if advanced == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                AnalysisMode::Warm => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                AnalysisMode::Cold => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                AnalysisMode::Shutdown => break,
            }
        }
    }

    /// Handles mode transitions and diagnostic logging.
    ///
    /// # CPU Usage Implications
    /// - `Hot` -> `Warm`: Reduces calculation density by 4x, conserving CPU slots.
    /// - `Warm` -> `Cold`: Reduces calculation density by 16x relative to Hot. CPU footprint drops to negligible.
    /// - Any -> `Hot`: Immediately brings analyzer to maximum vigilance (evaluation every 64KB) to quickly locks onto shifts.
    /// - Any -> `Shutdown`: Exits analyzer loop, releasing all resources.
    pub fn transition_mode(&mut self, new_mode: AnalysisMode) {
        if self.mode != new_mode {
            self.mode = new_mode;
            self.mode_transitions += 1;
            #[cfg(debug_assertions)]
            println!(
                "[StrideDetector] Operating mode transitioned to {:?} (Total Transitions: {})",
                self.mode, self.mode_transitions
            );
        }
    }

    /// Transposes the byte sequence into independent stride planes.
    ///
    /// For a detected stride length $S$, each byte at index $N$ in the input data is mapped
    /// to plane $(N \pmod S)$, at position $\lfloor N / S \rfloor$ within that plane.
    ///
    /// # Worked Example
    /// With stride $S = 4$ and a $12$-byte block:
    /// - Input: `[A0, A1, A2, A3, B0, B1, B2, B3, C0, C1, C2, C3]`
    ///   Here, $A$, $B$, and $C$ are consecutive $4$-byte structures (such as stereo 16-bit audio samples or 32-bit float points).
    /// - Plane 0: `[A0, B0, C0]`
    /// - Plane 1: `[A1, B1, C1]`
    /// - Plane 2: `[A2, B2, C2]`
    /// - Plane 3: `[A3, B3, C3]`
    /// - Output (Planes concatenated sequentially):
    ///   `[A0, B0, C0, A1, B1, C1, A2, B2, C2, A3, B3, C3]`
    ///
    /// # Rationale for Compression Improvement
    /// In structured data, bytes corresponding to the same offset within a struct (plane) serve the
    /// same structural "role". For instance:
    /// - In a 24-bit RGB image ($S = 3$), Plane 0 holds Red values, Plane 1 holds Green, and Plane 2 holds Blue. Red values correlate heavily with neighboring Red values, but far less with Green or Blue values.
    /// - In a 32-bit float array ($S = 4$), Plane 3 holds the exponent bytes. Exponents in a float array are often identical or change very slowly, whereas the mantissa bytes (Planes 0-2) vary rapidly.
    ///
    /// By grouping bytes of the same plane together, we create highly uniform sequences with low local entropy, allowing BWT, LZ77, and PPM models to find much longer matches and predict symbols with higher confidence.
    pub fn apply_transposition(&self, data: &mut Vec<u8>, stride: u8) {
        crate::transform::transpose::ByteTransposer::new(stride).transpose_inplace(data);
    }

    /// Performs delta-coding within each transposed plane of the data vector.
    ///
    /// For each plane, every byte (except the first byte, which serves as the seed)
    /// is replaced by the difference between itself and the preceding byte in that plane:
    ///
    /// $$\Delta_i = X_i - X_{i-1} \pmod{256}$$
    ///
    /// # Entropy Reduction Explanation
    /// Under transposition, consecutive bytes in a plane represent values of the same role
    /// across adjacent samples (e.g. consecutive left-channel audio samples or consecutive float exponents).
    /// These values typically change smoothly or remain constant.
    ///
    /// For example, in a 16-bit PCM mono audio stream, adjacent samples are highly correlated.
    /// By taking the difference between consecutive values in the same plane:
    /// - Constant runs (like silence or flat regions) become long runs of `0`s.
    /// - Slowly changing signals become very small integers (e.g., `-1`, `0`, `1`), which cluster tightly
    ///   around zero.
    ///
    /// This transforms a wide probability distribution (high entropy) into a highly peaked distribution
    /// centered on zero (very low entropy), which is extremely easy for PPM and rANS encoders to compress.
    pub fn apply_delta_coding(&self, data: &mut [u8]) {
        let stride = match self.current_confirmed {
            Some(s) if s > 1 => s as usize,
            _ => return,
        };
        let len = data.len();
        if len <= stride {
            return;
        }

        let num_blocks = len / stride;
        let remainder = len % stride;

        let mut plane_offsets = vec![0; stride];
        let mut current_offset = 0;
        for (p, offset) in plane_offsets.iter_mut().enumerate() {
            *offset = current_offset;
            let plane_len = num_blocks + if p < remainder { 1 } else { 0 };
            current_offset += plane_len;
        }

        for (p, &start) in plane_offsets.iter().enumerate() {
            let plane_len = num_blocks + if p < remainder { 1 } else { 0 };
            if plane_len <= 1 {
                continue;
            }
            
            for idx in (1..plane_len).rev() {
                let curr_idx = start + idx;
                let prev_idx = start + idx - 1;
                data[curr_idx] = data[curr_idx].wrapping_sub(data[prev_idx]);
            }
        }
    }
}

/// Computes the Pearson autocorrelation coefficient for a specific stride over a byte slice.
///
/// Pearson autocorrelation measures the linear correlation between a signal
/// and a time-lagged copy of itself.
///
/// # Autocorrelation Formula
/// Given a block of data $X$ of length $N$, and a stride/lag $S$, we compare $X\[0..N-S\]$
/// against the shifted series $Y = X\[S..N\]$.
///
/// The Pearson coefficient $r$ is calculated as:
///
/// $$r(S) = \frac{(N - S) \sum x_i y_i - \sum x_i \sum y_i}{\sqrt{ \left[(N - S) \sum x_i^2 - (\sum x_i)^2\right] \left[(N - S) \sum y_i^2 - (\sum y_i)^2\right] }}$$
///
/// Where:
/// * $x_i = X\[i\]$
/// * $y_i = X\[i + S\]$
/// * $i \in \[0,\ N-S-1\]$
pub fn compute_pearson_autocorrelation(data: &[u8], stride: u8) -> f32 {
    let s = stride as usize;
    if data.len() <= s * 2 {
        return 0.0;
    }

    let n = data.len() - s;
    let mut sum_x: f64 = 0.0;
    let mut sum_y: f64 = 0.0;
    let mut sum_xy: f64 = 0.0;
    let mut sum_x2: f64 = 0.0;
    let mut sum_y2: f64 = 0.0;

    for i in 0..n {
        let x = data[i] as f64;
        let y = data[i + s] as f64;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_x2 += x * x;
        sum_y2 += y * y;
    }

    let n_f = n as f64;
    let num = n_f * sum_xy - sum_x * sum_y;
    let den_x = n_f * sum_x2 - sum_x * sum_x;
    let den_y = n_f * sum_y2 - sum_y * sum_y;

    if den_x <= 0.0 || den_y <= 0.0 {
        // Variance is zero. If all bytes are identical, they correlate perfectly (1.0).
        let mut identical = true;
        for i in 0..n {
            if data[i] != data[i + s] {
                identical = false;
                break;
            }
        }
        return if identical { 1.0 } else { 0.0 };
    }

    let den = (den_x * den_y).sqrt();
    let r = num / den;

    r.max(0.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::circular::CircularBuffer;
    use crate::buffer::window::SlidingWindow;

    #[test]
    fn test_autocorrelation_constant() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let detector = StrideDetector::new(window, mode_tx, stability_rx, shutdown_rx);

        let data = [42; 100];
        let corr = detector.compute_autocorrelation(&data, 2);
        assert_eq!(corr, 1.0);
    }

    #[test]
    fn test_autocorrelation_alternating() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let detector = StrideDetector::new(window, mode_tx, stability_rx, shutdown_rx);

        let mut data = Vec::new();
        for i in 0..100 {
            data.push((i % 2) as u8);
        }

        let corr_s2 = detector.compute_autocorrelation(&data, 2);
        let corr_s1 = detector.compute_autocorrelation(&data, 1);

        assert_eq!(corr_s2, 1.0);
        assert_eq!(corr_s1, 0.0);
    }

    #[test]
    fn test_stride_confirmation() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let mut detector = StrideDetector::new(window.clone(), mode_tx, stability_rx, shutdown_rx);

        let mut pattern = Vec::new();
        for i in 0..200 {
            pattern.push((i % 4) as u8);
        }

        detector.test_all_candidates(&pattern);
        assert_eq!(detector.candidates[3].stride, 4);
        assert_eq!(detector.candidates[3].confirmation_count, 1);
        assert!(!detector.candidates[3].is_confirmed);
        assert_eq!(window.get_stride_hint(), 0);

        detector.test_all_candidates(&pattern);
        assert_eq!(detector.candidates[3].confirmation_count, 2);
        assert!(!detector.candidates[3].is_confirmed);

        detector.test_all_candidates(&pattern);
        assert_eq!(detector.candidates[3].confirmation_count, 3);
        assert!(detector.candidates[3].is_confirmed);
        
        assert_eq!(detector.best_candidate().unwrap().stride, 4);
        assert_eq!(detector.current_confirmed, Some(4));
        assert_eq!(window.get_stride_hint(), 4);

        let random_pattern: Vec<u8> = (0..200).map(|i| ((i * 17) % 256) as u8).collect();
        detector.test_all_candidates(&random_pattern);
        assert_eq!(detector.candidates[3].confirmation_count, 0);
        assert!(!detector.candidates[3].is_confirmed);
        assert_eq!(detector.current_confirmed, None);
        assert_eq!(window.get_stride_hint(), 0);
    }

    #[test]
    fn test_should_reexamine() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let mut detector = StrideDetector::new(window, mode_tx, stability_rx, shutdown_rx);

        assert!(!detector.should_reexamine());
        detector.bytes_since_last_check = STRIDE_REEXAMINE_INTERVAL - 1;
        assert!(!detector.should_reexamine());
        detector.bytes_since_last_check = STRIDE_REEXAMINE_INTERVAL;
        assert!(detector.should_reexamine());
    }

    #[test]
    fn test_apply_transposition() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let detector = StrideDetector::new(window, mode_tx, stability_rx, shutdown_rx);

        // Input: [1, 2, 3, 4, 10, 20, 30, 40, 100, 110, 120, 130]
        let mut data = vec![1, 2, 3, 4, 10, 20, 30, 40, 100, 110, 120, 130];
        detector.apply_transposition(&mut data, 4);
        // Expected Output: [1, 10, 100, 2, 20, 110, 3, 30, 120, 4, 40, 130]
        assert_eq!(data, vec![1, 10, 100, 2, 20, 110, 3, 30, 120, 4, 40, 130]);
    }

    #[test]
    fn test_apply_delta_coding() {
        let buffer = Arc::new(CircularBuffer::new(2048));
        let window = Arc::new(SlidingWindow::new(buffer, 1024));
        let (mode_tx, _) = crossbeam::channel::unbounded();
        let (_, stability_rx) = crossbeam::channel::unbounded();
        let (_, shutdown_rx) = crossbeam::channel::unbounded();
        let mut detector = StrideDetector::new(window, mode_tx, stability_rx, shutdown_rx);
        detector.current_confirmed = Some(4);

        let mut data = vec![1, 10, 100, 2, 20, 110, 3, 30, 120, 4, 40, 130];
        detector.apply_delta_coding(&mut data);
        assert_eq!(data, vec![1, 9, 90, 2, 18, 90, 3, 27, 90, 4, 36, 90]);
    }
}
