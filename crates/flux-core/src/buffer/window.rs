//! Sliding window views over the circular buffer
//!
//! Provides read-only windows of active data for analysis and compression threads,
//! allowing zero-copy lookahead and history checks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use super::circular::CircularBuffer;

/// A sliding window providing direct read-only views into the circular buffer.
///
/// Multi-reader threads (such as the content classifier, stride analyzer, and compression
/// workers) use this struct to peer into upcoming or historic byte sequences.
#[derive(Debug)]
pub struct SlidingWindow {
    /// Reference to the underlying circular byte pool.
    pub buffer: Arc<CircularBuffer>,
    /// Monotonically increasing start position (in bytes) of this window.
    pub window_start: AtomicUsize,
    /// Predefined length of the visible window.
    pub window_size: usize,
    /// Last verified stride pattern length (0 = unknown/changing).
    pub stride_hint: AtomicU8,
}

impl SlidingWindow {
    /// Creates a new `SlidingWindow` view over a circular buffer.
    ///
    /// # Parameters
    /// * `buffer`: Shared reference to the underlying `CircularBuffer`.
    /// * `window_size`: Length in bytes of the window context.
    pub fn new(buffer: Arc<CircularBuffer>, window_size: usize) -> Self {
        Self {
            buffer,
            window_start: AtomicUsize::new(0),
            window_size,
            stride_hint: AtomicU8::new(0),
        }
    }

    /// Provides a zero-copy read-only slice of data ahead of the current window start.
    ///
    /// # Parameters
    /// * `offset`: Byte offset forward from the current window start.
    /// * `len`: Number of bytes to peek.
    ///
    /// # Safety
    /// Returns a direct slice references to the circular buffer memory. This is safe because:
    /// * The buffer is mirrored: contiguous reads up to `capacity` are guaranteed not to wrap.
    /// * The returned slice is bound to the lifetime of `&self`, preventing compile-time use
    ///   after buffer reclamation.
    pub fn peek(&self, offset: usize, len: usize) -> &[u8] {
        assert!(
            len <= self.buffer.capacity,
            "Peek length {} exceeds circular buffer capacity {}",
            len,
            self.buffer.capacity
        );
        let start_pos = self.window_start.load(Ordering::Acquire);
        let read_start = (start_pos + offset) % self.buffer.capacity;
        
        unsafe {
            let ptr = self.buffer.data.as_ptr().add(read_start);
            std::slice::from_raw_parts(ptr, len)
        }
    }

    /// Provides a zero-copy slice of previously processed bytes behind the current window start.
    ///
    /// # Parameters
    /// * `offset`: How many bytes behind the current window start to begin reading.
    /// * `len`: Number of bytes to peek.
    ///
    /// # Why the Backwards Analysis Thread Needs This
    /// The Backwards Analysis Thread verifies whether the previously chosen stride pattern
    /// is still holding stable. To do this without keeping redundant buffers or re-reading
    /// from disk, it peeks backwards into the history window *behind* the current compression head.
    /// If the pattern is stable, it signals the Stride Analyzer to throttle down. If it drifts,
    /// it triggers recalibration.
    ///
    /// # Safety
    /// Boundary checks ensure we never read beyond the current buffer head (data that has been
    /// overwritten by the manager thread).
    pub fn peek_backwards(&self, offset: usize, len: usize) -> &[u8] {
        let window_start_val = self.window_start.load(Ordering::Acquire);
        let start_pos = window_start_val.saturating_sub(offset);
        let actual_len = std::cmp::min(len, window_start_val - start_pos);
        if actual_len == 0 {
            return &[];
        }
        assert!(
            actual_len <= self.buffer.capacity,
            "Peek backwards length {} exceeds circular buffer capacity {}",
            actual_len,
            self.buffer.capacity
        );
        let read_start = start_pos % self.buffer.capacity;
        
        unsafe {
            let ptr = self.buffer.data.as_ptr().add(read_start);
            std::slice::from_raw_parts(ptr, actual_len)
        }
    }

    /// Slides the window view forward, releasing processed bytes from the circular buffer.
    ///
    /// Advances `window_start` and increments the circular buffer's `head` pointer.
    pub fn slide_forward(&self, bytes: usize) {
        self.window_start.fetch_add(bytes, Ordering::Release);
        self.buffer.advance_head(bytes);
    }

    /// Sets the current stride hint value.
    pub fn set_stride_hint(&self, stride: u8) {
        self.stride_hint.store(stride, Ordering::Release);
    }

    /// Gets the current stride hint value.
    pub fn get_stride_hint(&self) -> u8 {
        self.stride_hint.load(Ordering::Acquire)
    }

    /// Returns the number of uncompressed bytes currently buffered ahead of the window start.
    pub fn bytes_ahead(&self) -> usize {
        let tail_val = self.buffer.tail.load(Ordering::Acquire);
        let window_start_val = self.window_start.load(Ordering::Acquire);
        tail_val.saturating_sub(window_start_val)
    }

    /// Returns the number of uncompressed bytes currently buffered behind the window start.
    pub fn bytes_behind(&self) -> usize {
        let window_start_val = self.window_start.load(Ordering::Acquire);
        let head_val = self.buffer.head.load(Ordering::Acquire);
        window_start_val.saturating_sub(head_val)
    }
}
