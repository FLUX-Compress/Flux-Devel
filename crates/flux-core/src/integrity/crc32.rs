//! CRC32 integrity verification
//!
//! Provides SIMD-accelerated CRC32 checksums and parallel background computation.

use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use crossbeam::channel::Receiver;
use crate::buffer::circular::CircularBuffer;
use crate::threads::signals::BufferSignal;

/// High-speed CRC32 checksum accumulator using the `crc32fast` engine.
#[derive(Debug, Clone)]
pub struct Crc32Hasher {
    hasher: crc32fast::Hasher,
    bytes_processed: u64,
}

impl Crc32Hasher {
    /// Creates a new `Crc32Hasher`.
    pub fn new() -> Self {
        Self {
            hasher: crc32fast::Hasher::new(),
            bytes_processed: 0,
        }
    }

    /// Feeds bytes into the running CRC32 computation.
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
        self.bytes_processed += data.len() as u64;
    }

    /// Returns the final CRC32 value without consuming self.
    pub fn finalize(&self) -> u32 {
        self.hasher.clone().finalize()
    }

    /// Resets the hasher state for reuse on a different stream block.
    pub fn reset(&mut self) {
        self.hasher = crc32fast::Hasher::new();
        self.bytes_processed = 0;
    }

    /// Returns the total bytes processed by this hasher.
    pub fn bytes_processed(&self) -> u64 {
        self.bytes_processed
    }
}

impl Default for Crc32Hasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Computes CRC32 in a dedicated background thread reading from the same
/// circular buffer as compression, avoiding extra disk I/O overhead.
pub struct ParallelCrc32 {
    buffer: Arc<CircularBuffer>,
    /// List of (file_index, file_size) pairs.
    files: Mutex<Vec<(u64, u64)>>,
}

impl ParallelCrc32 {
    /// Creates a new `ParallelCrc32` tied to a circular buffer.
    pub fn new(buffer: Arc<CircularBuffer>) -> Self {
        Self {
            buffer,
            files: Mutex::new(Vec::new()),
        }
    }

    /// Configures the sequence of files to be processed by this parallel hasher.
    ///
    /// Must be called before `run`.
    pub fn set_files(&self, files: Vec<(u64, u64)>) {
        *self.files.lock().unwrap() = files;
    }

    /// Spawns a background thread that calculates CRC32s for all configured files.
    pub fn run(&self, signal_rx: Receiver<BufferSignal>) -> JoinHandle<HashMap<u64, u32>> {
        let buffer = self.buffer.clone();
        
        // Clone files list from the mutex.
        let files = self.files.lock().unwrap().clone();

        std::thread::spawn(move || {
            let mut results = HashMap::new();
            let mut processed_abs = 0;
            
            let mut current_file_idx = 0;
            let mut current_file_bytes_processed = 0u64;
            let mut current_hasher = crc32fast::Hasher::new();

            // Pre-initialize empty files before the loop or during transitions.
            while current_file_idx < files.len() && files[current_file_idx].1 == 0 {
                let (file_index, _) = files[current_file_idx];
                results.insert(file_index, crc32fast::Hasher::new().finalize());
                current_file_idx += 1;
            }

            loop {
                // Wait for the next buffer signal.
                let sig = signal_rx.recv().ok();

                // Process all data currently available in the buffer.
                loop {
                    let tail = buffer.tail.load(Ordering::Acquire);
                    let head = buffer.head.load(Ordering::Acquire);
                    
                    if processed_abs >= tail {
                        break;
                    }
                    
                    let total_to_read = tail - processed_abs;
                    let chunk_size = std::cmp::min(total_to_read, 65536);
                    let offset = processed_abs - head;
                    
                    let mut temp_buf = vec![0u8; chunk_size];
                    let bytes_read = buffer.read(&mut temp_buf, offset, chunk_size);
                    if bytes_read == 0 {
                        break;
                    }

                    // Feed the read bytes into the current active file hasher.
                    let mut data_left = &temp_buf[..bytes_read];
                    while !data_left.is_empty() && current_file_idx < files.len() {
                        let (file_index, file_size) = files[current_file_idx];
                        let remaining_for_file = file_size - current_file_bytes_processed;
                        let consume_len = std::cmp::min(data_left.len() as u64, remaining_for_file) as usize;

                        current_hasher.update(&data_left[..consume_len]);
                        current_file_bytes_processed += consume_len as u64;
                        data_left = &data_left[consume_len..];

                        if current_file_bytes_processed == file_size {
                            // Finalize this file
                            let crc = current_hasher.clone().finalize();
                            results.insert(file_index, crc);

                            // Move to next file
                            current_file_idx += 1;
                            current_file_bytes_processed = 0;
                            current_hasher = crc32fast::Hasher::new();

                            // Handle contiguous sequence of empty files
                            while current_file_idx < files.len() && files[current_file_idx].1 == 0 {
                                let (next_file_index, _) = files[current_file_idx];
                                results.insert(next_file_index, crc32fast::Hasher::new().finalize());
                                current_file_idx += 1;
                            }
                        }
                    }

                    processed_abs += bytes_read;
                }

                if sig == Some(BufferSignal::EndOfFile) {
                    break;
                }
            }

            // If we hit EOF but still have any files unfinished (e.g. padding/truncation), finalize them
            if current_file_idx < files.len() && current_file_bytes_processed > 0 {
                let (file_index, _) = files[current_file_idx];
                results.insert(file_index, current_hasher.finalize());
            }

            results
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_crc32_known_value() {
        let mut hasher = Crc32Hasher::new();
        hasher.update(b"hello world");
        assert_eq!(hasher.finalize(), 0x0d4a1185);
    }

    #[test]
    fn test_crc32_incremental_equals_single_pass() {
        let data = b"the quick brown fox jumps over the lazy dog";
        
        let mut hasher1 = Crc32Hasher::new();
        hasher1.update(data);
        let val1 = hasher1.finalize();

        let mut hasher2 = Crc32Hasher::new();
        hasher2.update(&data[..10]);
        hasher2.update(&data[10..20]);
        hasher2.update(&data[20..]);
        let val2 = hasher2.finalize();

        assert_eq!(val1, val2);
        assert_eq!(hasher2.bytes_processed(), data.len() as u64);
    }

    #[test]
    fn test_crc32_reset_and_reuse() {
        let mut hasher = Crc32Hasher::new();
        hasher.update(b"test data 1");
        let val1 = hasher.finalize();

        hasher.reset();
        assert_eq!(hasher.bytes_processed(), 0);
        
        hasher.update(b"test data 1");
        let val2 = hasher.finalize();
        assert_eq!(val1, val2);
    }
}
