//! Buffer Manager thread implementation
//!
//! "The Babysitter" thread runs in the background to ensure that the compression workers
//! are never starved of data by maintaining the circular buffer in an always-full state.

use super::circular::CircularBuffer;
use crate::threads::signals::{BufferSignal, CompressionSignal};
use crossbeam::channel::{Receiver, Sender};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Coordinates streaming data from a source reader into the circular buffer.
///
/// Runs as a dedicated background thread to babysit buffer levels, preventing starvation
/// of classifier and compression workers by proactively filling buffer space.
pub struct BufferManager {
    /// Reference to the shared circular buffer.
    pub buffer: Arc<CircularBuffer>,
    /// Thread-safe wrapper around the raw input source reader.
    pub source: Mutex<Box<dyn std::io::Read + Send>>,
    /// Channel to emit data-availability and watermark signals.
    pub signal_tx: Sender<BufferSignal>,
    /// Channel to listen for shutdown signals from coordination threads.
    pub shutdown_rx: Receiver<CompressionSignal>,
    /// Monotonically increasing count of total bytes read from source.
    pub bytes_read_total: AtomicU64,
    /// Flag indicating whether the source stream has reached its end.
    pub eof_reached: AtomicBool,
}

impl BufferManager {
    /// Creates a new `BufferManager` instance.
    ///
    /// # Parameters
    /// * `buffer`: Shared reference to the `CircularBuffer` memory pool.
    /// * `source`: Dyn reader representing input file/stream.
    /// * `signal_tx`: Crossbeam sender for buffer activity events.
    /// * `shutdown_rx`: Crossbeam receiver for shutdown controls.
    pub fn new(
        buffer: Arc<CircularBuffer>,
        source: Box<dyn std::io::Read + Send>,
        signal_tx: Sender<BufferSignal>,
        shutdown_rx: Receiver<CompressionSignal>,
    ) -> Self {
        Self {
            buffer,
            source: Mutex::new(source),
            signal_tx,
            shutdown_rx,
            bytes_read_total: AtomicU64::new(0),
            eof_reached: AtomicBool::new(false),
        }
    }

    /// Executed by the spawned thread. Runs the babysitting loop until EOF or shutdown.
    ///
    /// # Decisions and Logic
    /// * **Shutdown Receiver**: Regularly checks if the compression threads issued a shutdown.
    /// * **Watermarks**: Signals low/high water thresholds to balance disk I/O and CPU memory.
    /// * **Refill Mechanism**: Fills free space dynamically using zero-copy closure writes.
    /// * **Microsecond Sleep**: Avoids busy-spinning the CPU core when the buffer is saturated.
    pub fn run(&self) {
        let mut was_low = false;
        let mut was_high = false;

        loop {
            // Decision 1: Check if the coordination pipeline requested shutdown.
            // Using non-blocking try_recv to avoid stalling the circular buffer fill logic.
            if let Ok(CompressionSignal::Shutdown) = self.shutdown_rx.try_recv() {
                break;
            }

            // Decision 2: Query the buffer fill state to determine watermark notifications.
            let fill = self.buffer.fill_level();

            // Low watermark: Send notification if fill drops below 25% to warn consumers.
            if fill < 0.25 {
                if !was_low {
                    let _ = self.signal_tx.send(BufferSignal::LowWater);
                    was_low = true;
                }
            } else {
                was_low = false;
            }

            // High watermark: Notify when fill goes above 90% (can trigger input throttle).
            if fill > 0.90 {
                if !was_high {
                    let _ = self.signal_tx.send(BufferSignal::HighWater);
                    was_high = true;
                }
            } else {
                was_high = false;
            }

            // Decision 3: Proactively pull data from source if buffer has free slots.
            if self.buffer.free_space() > 0 {
                match self.fill_tail() {
                    Ok(0) => {
                        // Source is completely dry. Initiate final EOF cleanup.
                        self.handle_eof();
                        break;
                    }
                    Ok(n) => {
                        // Data read successfully, notify readers of availability.
                        let _ = self.signal_tx.send(BufferSignal::DataAvailable(n));
                    }
                    Err(e) => {
                        // Log standard I/O errors and trigger clean exit.
                        crate::flux_debug!("FLUX BufferManager read error: {:?}", e);
                        self.handle_eof();
                        break;
                    }
                }
            } else {
                // Decision 4: Sleep briefly when the circular buffer is completely full.
                // Sleeping for 100 microseconds yields the CPU slice to compression threads,
                // preventing CPU starvation while maintaining rapid latency response.
                std::thread::sleep(std::time::Duration::from_micros(100));
            }
        }
    }

    /// Reads data from the input reader directly into the circular buffer tail.
    ///
    /// Utilizes `write_with` on the circular buffer to perform zero-copy reads,
    /// preventing double allocation and double buffering.
    ///
    /// # Return Value
    /// Returns the number of bytes read and committed, or an I/O Error.
    pub fn fill_tail(&self) -> Result<usize, std::io::Error> {
        let mut source_guard = self.source.lock().unwrap();

        // Read up to 64KB blocks to optimize disk sectors throughput.
        self.buffer
            .write_with(65536, |slice| source_guard.read(slice))
    }

    /// Handles EOF transition, marking flags and sending notifications.
    ///
    /// # Rationale
    /// EOF detection is useful intelligence for compression. When the compressor
    /// knows that no further blocks will arrive, it can safely switch from dynamic
    /// sliding window dictionaries to global block statistics (e.g. final sort/Huffman)
    /// to achieve maximum compression density in the final block.
    pub fn handle_eof(&self) {
        self.eof_reached.store(true, Ordering::Release);
        self.buffer.eof.store(true, Ordering::Release);
        let _ = self.signal_tx.send(BufferSignal::EndOfFile);
    }

    /// Returns the total number of bytes read from source so far.
    pub fn current_position(&self) -> u64 {
        self.bytes_read_total.load(Ordering::Acquire)
    }
}
