//! Backwards Analysis Thread
//!
//! Validates whether the detected stride pattern is still holding on compressed data.
//! By monitoring history, it dynamically throttles computation in the forward analyzer
//! or triggers immediate recalibration when drift occurs.

use crate::archive::format::BlockBoundaryHint;
use crate::buffer::window::SlidingWindow;
use crate::threads::signals::{CompressionSignal, ContentType, StrideSignal};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

/// Number of consecutive stable checks before signaling Cold mode.
///
/// # Rationale for 8
/// A stability streak of 8 consecutive checks (representing 8KB of compressed data) ensures
/// that the repeating pattern is stationary and not a transient artifact of the stream.
/// Transitioning the forward analyzer to Cold mode cuts CPU usage by 16x, so we must be highly
/// confident that the pattern is stable to avoid missing a subsequent drift.
pub const STABILITY_STREAK_FOR_COLD: u32 = 8;

/// Correlation drop threshold that triggers a Drifted signal.
///
/// # Rationale for 0.15
/// Small fluctuations in Pearson correlation are normal due to local noise or slight variations
/// in data structure. Setting the drift threshold at 0.15 relative to the confirmation threshold
/// of 0.75 (meaning correlation drops below 0.60) prevents false-alarm wakeups on slightly noisy
/// but stable data, while ensuring that actual changes in layout trigger a rapid wake-up signal.
pub const DRIFT_THRESHOLD: f32 = 0.15;

/// Backwards analyzer tracking stride stability.
#[derive(Debug)]
pub struct BackwardsAnalyzer {
    /// Sliding window context over the circular buffer.
    pub window: Arc<SlidingWindow>,
    /// Channel for sending stride signals to the stride analyzer and coordination pipeline.
    pub signal_tx: crossbeam::channel::Sender<StrideSignal>,
    /// Channel for sending block boundary hints at EOF.
    pub hints_tx: crossbeam::channel::Sender<Vec<BlockBoundaryHint>>,
    /// Channel for receiving system-wide compression commands (Shutdown).
    pub shutdown_rx: crossbeam::channel::Receiver<CompressionSignal>,
    /// Current confirmed stride value being validated.
    pub last_confirmed_stride: AtomicU8,
    /// Number of consecutive stable checks observed.
    pub stability_streak: AtomicU32,
    /// Tunable threshold for correlation drops.
    pub drift_threshold: f32,
    /// Total bytes validated throughout the lifetime of this analyzer.
    pub bytes_validated_total: AtomicU64,
    /// Flag indicating whether EOF global analysis has run.
    pub eof_behavior_complete: AtomicBool,
    /// History of confirmed strides and their starting byte positions.
    pub stride_history: Mutex<Vec<(u64, u8)>>,
}

impl BackwardsAnalyzer {
    /// Creates a new `BackwardsAnalyzer`.
    pub fn new(
        window: Arc<SlidingWindow>,
        signal_tx: crossbeam::channel::Sender<StrideSignal>,
        hints_tx: crossbeam::channel::Sender<Vec<BlockBoundaryHint>>,
        shutdown_rx: crossbeam::channel::Receiver<CompressionSignal>,
    ) -> Self {
        Self {
            window,
            signal_tx,
            hints_tx,
            shutdown_rx,
            last_confirmed_stride: AtomicU8::new(0),
            stability_streak: AtomicU32::new(0),
            drift_threshold: DRIFT_THRESHOLD,
            bytes_validated_total: AtomicU64::new(0),
            eof_behavior_complete: AtomicBool::new(false),
            stride_history: Mutex::new(Vec::new()),
        }
    }

    /// Computes Pearson autocorrelation on historical data for a given stride.
    ///
    /// This intentionally reuses the exact same algorithm as the forward stride detector
    /// to ensure that both components agree on what "stable" and "drifted" mean.
    pub fn validate_stride(&self, data: &[u8], stride: u8) -> f32 {
        crate::analysis::stride::compute_pearson_autocorrelation(data, stride)
    }

    /// Global post-analysis run when EOF is reached.
    ///
    /// By looking at the complete stride history across the entire file, FLUX can make
    /// globally optimal decisions for block boundary re-optimization that per-block
    /// local analysis cannot make.
    pub fn handle_eof_analysis(&mut self) {
        let history = self.stride_history.lock().unwrap();
        let mut boundary_hints = Vec::new();

        if history.is_empty() {
            // No strides observed throughout the file, so it's uniform with stride 0
            let _ = self.signal_tx.send(StrideSignal::Uniform(0));
        } else {
            let first_stride = history[0].1;
            let is_uniform = history.iter().all(|&(_, s)| s == first_stride);
            if is_uniform {
                // Entire file was uniform, signal that no boundary re-optimization is needed.
                let _ = self.signal_tx.send(StrideSignal::Uniform(first_stride));
            } else {
                // Construct optimal block boundaries based on stride transitions
                let mut ranges = Vec::new();
                let mut current_start = history[0].0;
                let mut current_stride = history[0].1;

                for i in 1..history.len() {
                    let (pos, stride) = history[i];
                    if stride != current_stride {
                        ranges.push((current_start, pos, current_stride));

                        // Record a boundary hint
                        let content_type_before = if current_stride > 0 {
                            ContentType::Multimedia
                        } else {
                            ContentType::Binary
                        };
                        let content_type_after = if stride > 0 {
                            ContentType::Multimedia
                        } else {
                            ContentType::Binary
                        };
                        let stride_before = if current_stride > 0 {
                            Some(current_stride)
                        } else {
                            None
                        };
                        let stride_after = if stride > 0 { Some(stride) } else { None };

                        boundary_hints.push(BlockBoundaryHint {
                            byte_offset: pos,
                            content_type_before,
                            content_type_after,
                            stride_before,
                            stride_after,
                        });

                        current_start = pos;
                        current_stride = stride;
                    }
                }
                // Push the final range up to the total validated bytes
                let total_bytes = self.bytes_validated_total.load(Ordering::Acquire);
                ranges.push((current_start, total_bytes, current_stride));

                let _ = self.signal_tx.send(StrideSignal::VariedPatterns(ranges));
            }
        }

        // Send discovered boundary hints on hints_tx
        let _ = self.hints_tx.send(boundary_hints);
        self.eof_behavior_complete.store(true, Ordering::Release);
    }

    /// Main loop for the backwards analysis thread.
    ///
    /// Validates processed data behind the compression pointer, sending stability feedback
    /// signals to adjust the Stride Analyzer's operating vigilance.
    pub fn run(&mut self) {
        let mut last_checked_pos = 0u64;

        loop {
            // Check for shutdown signal
            match self.shutdown_rx.try_recv() {
                Ok(CompressionSignal::Shutdown) => break,
                Err(crossbeam::channel::TryRecvError::Disconnected) => break,
                _ => {}
            }

            // Detect EOF: no bytes ahead and EOF flag is raised in circular buffer
            let is_eof =
                self.window.buffer.eof.load(Ordering::Acquire) && self.window.bytes_ahead() == 0;
            if is_eof {
                if !self.eof_behavior_complete.load(Ordering::Acquire) {
                    self.handle_eof_analysis();
                }
                break;
            }

            let current_start = self.window.window_start.load(Ordering::Acquire) as u64;
            let physical_bytes_behind =
                std::cmp::min(current_start, self.window.buffer.capacity as u64);

            // Validate in chunks of 1024 bytes once they have been advanced by compression
            if current_start.saturating_sub(last_checked_pos) >= 1024 {
                let mut work_done = false;

                while current_start.saturating_sub(last_checked_pos) >= 1024 {
                    let offset = (current_start - last_checked_pos) as usize;
                    if offset > physical_bytes_behind as usize {
                        // Catch up if history has been reclaimed/advanced past the buffer capacity
                        last_checked_pos = current_start - physical_bytes_behind;
                        continue;
                    }

                    // Keep last_confirmed_stride in sync with window hints
                    let active_stride = self.window.get_stride_hint();
                    if active_stride > 0 {
                        self.last_confirmed_stride
                            .store(active_stride, Ordering::Release);
                    }

                    let stride = self.last_confirmed_stride.load(Ordering::Acquire);
                    let pos = last_checked_pos;
                    if stride > 0 {
                        // Peek backwards into recently processed history
                        let data = self.window.peek_backwards(offset, 1024);
                        if data.len() >= 1024 {
                            let corr = self.validate_stride(data, stride);
                            self.bytes_validated_total
                                .fetch_add(1024, Ordering::Release);

                            let threshold = crate::analysis::stride::STRIDE_CONFIRMATION_THRESHOLD;
                            if corr >= (threshold - self.drift_threshold) {
                                self.stride_history.lock().unwrap().push((pos, stride));
                                let streak =
                                    self.stability_streak.fetch_add(1, Ordering::SeqCst) + 1;
                                if streak >= STABILITY_STREAK_FOR_COLD {
                                    let _ = self.signal_tx.send(StrideSignal::Stable(stride));
                                }
                            } else {
                                // Drift detected: reset stability streak, push stride 0, and send wake-up signal
                                self.stride_history.lock().unwrap().push((pos, 0));
                                self.stability_streak.store(0, Ordering::SeqCst);
                                self.last_confirmed_stride.store(0, Ordering::Release);
                                let _ = self.signal_tx.send(StrideSignal::Drifted);
                            }
                        }
                    } else {
                        // Stride is 0/unknown, log as 0 to track the transition
                        self.bytes_validated_total
                            .fetch_add(1024, Ordering::Release);
                        self.stride_history.lock().unwrap().push((pos, 0));
                    }
                    last_checked_pos += 1024;
                    work_done = true;
                }

                if !work_done {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            } else {
                // Yield/sleep to avoid busy spin-locking
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::stride::{AnalysisMode, StrideDetector};
    use crate::buffer::circular::CircularBuffer;
    use crate::buffer::window::SlidingWindow;

    #[test]
    fn test_backwards_analyzer_live() {
        let buffer = Arc::new(CircularBuffer::new(16384));
        let window = Arc::new(SlidingWindow::new(buffer.clone(), 4096));

        let (signal_tx, signal_rx) = crossbeam::channel::unbounded();
        let (hints_tx, hints_rx) = crossbeam::channel::unbounded();
        let (_shutdown_tx, shutdown_rx) = crossbeam::channel::unbounded();

        let mut validator =
            BackwardsAnalyzer::new(window.clone(), signal_tx, hints_tx, shutdown_rx);

        // Set stride hint and confirmed stride
        window.set_stride_hint(4);
        validator.last_confirmed_stride.store(4, Ordering::Release);

        // Start live validator thread
        let handle = std::thread::spawn(move || {
            validator.run();
            validator
        });

        // Write repeating 4-byte pattern to buffer
        let mut pattern = Vec::new();
        for i in 0..1024 {
            pattern.push((i % 4) as u8);
        }
        buffer.write(&pattern);

        // Slide forward so bytes are behind (to trigger validation)
        window.slide_forward(1024);

        // Write 8 blocks of 1024 bytes to trigger Cold transition (STABILITY_STREAK_FOR_COLD = 8)
        for _ in 0..7 {
            buffer.write(&pattern);
            window.slide_forward(1024);
        }

        // Now it has 8 checks, so it should send Stable(4)
        let mut found_stable = false;
        for _ in 0..100 {
            if let Ok(StrideSignal::Stable(s)) = signal_rx.try_recv() {
                assert_eq!(s, 4);
                found_stable = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(found_stable);

        // Now, let's trigger drift by writing random/noisy data
        let mut noise = Vec::new();
        for i in 0..1024 {
            noise.push(((i * 17 + 5) % 256) as u8);
        }
        buffer.write(&noise);
        window.slide_forward(1024);

        // Validator should detect drift and send Drifted signal
        let mut found_drift = false;
        for _ in 0..100 {
            if let Ok(StrideSignal::Drifted) = signal_rx.try_recv() {
                found_drift = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(found_drift);

        // Trigger EOF to run eof analysis and close Validator thread
        buffer.eof.store(true, Ordering::Release);

        // Wait for validator thread to exit
        let validator = handle.join().unwrap();

        assert_eq!(
            validator.bytes_validated_total.load(Ordering::Acquire),
            9216
        ); // 9 blocks of 1024

        // Verify we got the EOF boundary hints
        let hints = hints_rx.try_recv().unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].byte_offset, 8192);
        assert_eq!(hints[0].stride_before, Some(4));
        assert_eq!(hints[0].stride_after, None);
        assert_eq!(hints[0].content_type_before, ContentType::Multimedia);
        assert_eq!(hints[0].content_type_after, ContentType::Binary);
    }

    #[test]
    fn test_mode_transitions_live() {
        let buffer = Arc::new(CircularBuffer::new(4096));
        let window = Arc::new(SlidingWindow::new(buffer.clone(), 2048));

        let (mode_tx, _mode_rx) = crossbeam::channel::unbounded();
        let (stability_tx, stability_rx) = crossbeam::channel::unbounded();
        let (shutdown_tx, shutdown_rx) = crossbeam::channel::unbounded();

        let mut detector = StrideDetector::new(window.clone(), mode_tx, stability_rx, shutdown_rx);

        assert_eq!(detector.mode, AnalysisMode::Hot);

        let window_clone = window.clone();
        let handle = std::thread::spawn(move || {
            detector.run(window_clone);
            detector
        });

        // Transition Hot -> Warm
        stability_tx.send(StrideSignal::Stable(4)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Transition Warm -> Cold
        stability_tx.send(StrideSignal::Stable(4)).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Transition Cold -> Hot
        stability_tx.send(StrideSignal::Drifted).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));

        // Shut down the loop
        shutdown_tx.send(CompressionSignal::Shutdown).unwrap();

        let detector = handle.join().unwrap();

        // During the run, it transitioned:
        // Hot -> Warm -> Cold -> Hot -> Shutdown (final state)
        assert_eq!(detector.mode, AnalysisMode::Shutdown);
        assert!(detector.mode_transitions >= 3);
    }
}
