//! Stream Pipeline coordination
//!
//! Handles sequential execution pathways between block classifiers and compression engines.

use std::sync::Arc;
use std::thread::JoinHandle;
use crossbeam::channel::{unbounded, Sender, Receiver};
use crate::buffer::window::SlidingWindow;
use crate::threads::signals::{StrideSignal, CompressionSignal};
use crate::analysis::{StrideDetector, BackwardsAnalyzer};
use crate::archive::format::BlockBoundaryHint;

/// Multi-threaded compression execution pipeline.
pub struct CompressionPipeline;

/// Tuple containing the handles and channels returned by `spawn_analysis_pipeline`.
pub type AnalysisPipelineChannels = (
    JoinHandle<()>,
    JoinHandle<()>,
    Receiver<StrideSignal>,
    Receiver<Vec<BlockBoundaryHint>>,
    Sender<CompressionSignal>,
);

impl CompressionPipeline {
    /// Spawns and wires the Adaptive Stride Analyzer and Backwards Analysis threads.
    ///
    /// Shows how the threads are spawned using `std::thread::spawn` and wired together
    /// using `crossbeam` channels to form the confidence-based resource allocation loop.
    ///
    /// # Parameters
    /// * `window`: Shared reference to the `SlidingWindow` context.
    ///
    /// # Returns
    /// A tuple containing:
    /// 1. Stride Analyzer Thread `JoinHandle`
    /// 2. Backwards Validator Thread `JoinHandle`
    /// 3. Receiver for routing stride and EOF/stability signals to the pipeline coordinator
    /// 4. Receiver for EOF block boundary hints
    /// 5. Sender to trigger pipeline shutdown/pause
    pub fn spawn_analysis_pipeline(
        window: Arc<SlidingWindow>,
    ) -> AnalysisPipelineChannels {
        // 1. Setup channels
        // Forward stride detector output to the pipeline/coordinator
        let (stride_tx, stride_rx) = unbounded::<StrideSignal>();
        
        // Stability feedback loop channel to stride analyzer
        let (backwards_tx, backwards_rx) = unbounded::<StrideSignal>();
        
        // Dedicated channel for validator outputs to be broadcasted
        let (validator_tx, validator_rx) = unbounded::<StrideSignal>();
        
        // Channel for EOF block boundary hints
        let (hints_tx, hints_rx) = unbounded::<Vec<BlockBoundaryHint>>();
        
        // Control shutdown channel for coordinating thread exits
        let (shutdown_tx, shutdown_rx_stride) = unbounded::<CompressionSignal>();
        let shutdown_rx_backwards = shutdown_rx_stride.clone();

        // 2. Broadcast/Routing Loop
        // Reads stability signals from the validator and broadcasts them to both the
        // stride detector (via backwards_tx) and the pipeline coordinator (via stride_tx).
        let stride_tx_for_router = stride_tx.clone();
        std::thread::spawn(move || {
            while let Ok(sig) = validator_rx.recv() {
                let _ = stride_tx_for_router.send(sig.clone());
                let _ = backwards_tx.send(sig);
            }
        });

        // 3. Spawn the Adaptive Stride Analyzer Thread
        let window_stride = window.clone();
        let stride_tx_for_stride = stride_tx.clone();
        let stride_handle = std::thread::spawn(move || {
            let mut detector = StrideDetector::new(
                window_stride.clone(),
                stride_tx_for_stride,
                backwards_rx,
                shutdown_rx_stride,
            );
            detector.run(window_stride);
        });

        // 4. Spawn the Backwards Validator Thread
        let window_backwards = window;
        let backwards_handle = std::thread::spawn(move || {
            let mut validator = BackwardsAnalyzer::new(
                window_backwards,
                validator_tx,
                hints_tx,
                shutdown_rx_backwards,
            );
            validator.run();
        });

        (stride_handle, backwards_handle, stride_rx, hints_rx, shutdown_tx)
    }
}
