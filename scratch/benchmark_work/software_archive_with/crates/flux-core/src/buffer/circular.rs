//! Lock-free circular ring buffer with mirrored memory representation
//!
//! Provides thread-safe, lock-free, single-writer multi-reader byte queue operations
//! utilizing atomic variables.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A thread-safe, lock-free circular byte buffer.
///
/// # Thread Safety
/// This structure is `Send` and `Sync`. It relies on a single-writer, multi-reader pattern:
/// * Only one thread (typically the `BufferManager`) should write data to the tail.
/// * Multiple consumer threads may read from the buffer or peek into it.
/// * Synchronization is achieved using atomic indices with Acquire/Release semantics.
///
/// # Mirrored Layout
/// The internal data vector is allocated with size `capacity * 2`. Any writes to index
/// `i % capacity` are mirrored to `(i % capacity) + capacity`. This guarantees that
/// any contiguous read/peek of size up to `capacity` starting within `0..capacity`
/// is contiguous in physical memory, allowing the returning of zero-copy `&[u8]` slices.
pub struct CircularBuffer {
    /// Mirrored internal byte storage. Length is `capacity * 2`.
    pub data: Vec<u8>,
    /// Logical capacity of the circular buffer.
    pub capacity: usize,
    /// Read cursor (monotonic counter).
    pub head: AtomicUsize,
    /// Write cursor (monotonic counter).
    pub tail: AtomicUsize,
    /// Total bytes currently available for reading.
    pub bytes_available: AtomicUsize,
    /// End-of-file signal flag for threads peeking backwards/forwards.
    pub eof: std::sync::atomic::AtomicBool,
}

unsafe impl Send for CircularBuffer {}
unsafe impl Sync for CircularBuffer {}

impl std::fmt::Debug for CircularBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CircularBuffer")
            .field("capacity", &self.capacity)
            .field("head", &self.head)
            .field("tail", &self.tail)
            .field("bytes_available", &self.bytes_available)
            .field("eof", &self.eof)
            .finish()
    }
}

impl CircularBuffer {
    /// Creates a new `CircularBuffer` with a specified logical capacity.
    ///
    /// The physical memory allocation will be `capacity * 2` to accommodate the mirror.
    ///
    /// # Parameters
    /// * `capacity`: The logical byte capacity of the buffer.
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0; capacity * 2],
            capacity,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            bytes_available: AtomicUsize::new(0),
            eof: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Writes data from a slice into the circular buffer.
    ///
    /// Writes up to `free_space()` bytes, copying the contents into the mirrored slots.
    /// Returns the actual number of bytes written.
    ///
    /// # Performance
    /// Runs in $O(N)$ where $N$ is the number of bytes written, performing two memory copies.
    ///
    /// # Atomic Ordering Decisions
    /// * `tail` load uses `Acquire` to synchronize with potential concurrent readers.
    /// * `tail` store and `bytes_available` increment use `Release` to ensure that data writes to
    ///   the underlying buffer are fully committed and visible to reader threads before indices update.
    pub fn write(&self, src: &[u8]) -> usize {
        let space = self.free_space();
        if space == 0 || src.is_empty() {
            return 0;
        }
        let to_write = std::cmp::min(src.len(), space);
        let tail_val = self.tail.load(Ordering::Acquire);
        let write_idx = tail_val % self.capacity;

        // Write up to the end of the logical capacity in one contiguous chunk
        let chunk1 = std::cmp::min(to_write, self.capacity - write_idx);
        unsafe {
            let src_ptr = src.as_ptr();
            let dest_first = self.data.as_ptr().add(write_idx) as *mut u8;
            std::ptr::copy_nonoverlapping(src_ptr, dest_first, chunk1);

            let dest_mirror = self.data.as_ptr().add(write_idx + self.capacity) as *mut u8;
            std::ptr::copy_nonoverlapping(src_ptr, dest_mirror, chunk1);

            // If the write wraps around, copy the remainder to the beginning
            if chunk1 < to_write {
                let remainder = to_write - chunk1;
                let src_rem = src_ptr.add(chunk1);
                let dest_rem_first = self.data.as_ptr() as *mut u8;
                std::ptr::copy_nonoverlapping(src_rem, dest_rem_first, remainder);

                let dest_rem_mirror = self.data.as_ptr().add(self.capacity) as *mut u8;
                std::ptr::copy_nonoverlapping(src_rem, dest_rem_mirror, remainder);
            }
        }

        self.tail.fetch_add(to_write, Ordering::Release);
        self.bytes_available.fetch_add(to_write, Ordering::Release);
        to_write
    }

    /// Writes data directly into the buffer's tail space using a closure.
    ///
    /// Prevents double-buffering by providing a mutable slice representing the free space
    /// directly to the reader callback (e.g. file read).
    ///
    /// # Safety
    /// This method is safe because:
    /// * Only one writer thread is allowed to write to the tail.
    /// * Readers are restricted to indexes below the committed `bytes_available`.
    pub fn write_with<F, E>(&self, max_len: usize, f: F) -> Result<usize, E>
    where
        F: FnOnce(&mut [u8]) -> Result<usize, E>,
    {
        let space = self.free_space();
        if space == 0 {
            return Ok(0);
        }
        let to_write = std::cmp::min(max_len, space);
        let tail_val = self.tail.load(Ordering::Acquire);
        let write_idx = tail_val % self.capacity;

        // Contiguous slice available in the logical buffer first half
        let chunk = std::cmp::min(to_write, self.capacity - write_idx);

        let slice = unsafe {
            let ptr = self.data.as_ptr().add(write_idx) as *mut u8;
            std::slice::from_raw_parts_mut(ptr, chunk)
        };

        let bytes_written = f(slice)?;
        if bytes_written > 0 {
            // Mirror written bytes to the second half
            unsafe {
                let src = self.data.as_ptr().add(write_idx);
                let dest = self.data.as_ptr().add(write_idx + self.capacity) as *mut u8;
                std::ptr::copy_nonoverlapping(src, dest, bytes_written);
            }
            self.tail.fetch_add(bytes_written, Ordering::Release);
            self.bytes_available
                .fetch_add(bytes_written, Ordering::Release);
        }
        Ok(bytes_written)
    }

    /// Reads data from the buffer starting at a relative `offset` from the current head.
    ///
    /// Does *not* consume the data (does not advance `head`). Use [`Self::advance_head`] to consume.
    /// Returns the number of bytes successfully read.
    ///
    /// # Atomic Ordering Decisions
    /// * `head` and `bytes_available` are loaded with `Acquire` to ensure we observe the most
    ///   up-to-date values written by the writer thread before reading data.
    pub fn read(&self, buf: &mut [u8], offset: usize, len: usize) -> usize {
        let avail = self.available();
        if offset >= avail || len == 0 {
            return 0;
        }
        let to_read = std::cmp::min(len, avail - offset);
        let head_val = self.head.load(Ordering::Acquire);
        let read_start = (head_val + offset) % self.capacity;

        // Thanks to mirroring, read_start .. read_start + to_read is contiguous
        unsafe {
            let src = self.data.as_ptr().add(read_start);
            let dest = buf.as_mut_ptr();
            std::ptr::copy_nonoverlapping(src, dest, to_read);
        }
        to_read
    }

    /// Returns the number of bytes currently available for reading.
    pub fn available(&self) -> usize {
        self.bytes_available.load(Ordering::Acquire)
    }

    /// Returns the remaining logical byte capacity available for writing.
    pub fn free_space(&self) -> usize {
        self.capacity - self.available()
    }

    /// Returns the percentage (0.0 to 1.0) of the buffer currently occupied by data.
    pub fn fill_level(&self) -> f32 {
        let avail = self.available() as f32;
        let cap = self.capacity as f32;
        if cap > 0.0 {
            avail / cap
        } else {
            0.0
        }
    }

    /// Checks if the buffer contains no readable data.
    pub fn is_empty(&self) -> bool {
        self.available() == 0
    }

    /// Consumes a specific number of bytes from the head of the buffer.
    ///
    /// Wakes up the buffer manager thread if space is made available.
    ///
    /// # Atomic Ordering Decisions
    /// * Modifies `head` and `bytes_available` with `Release` to announce availability of free slots.
    pub fn advance_head(&self, bytes: usize) {
        let avail = self.bytes_available.load(Ordering::Acquire);
        let to_advance = std::cmp::min(bytes, avail);
        if to_advance > 0 {
            self.head.fetch_add(to_advance, Ordering::Release);
            self.bytes_available
                .fetch_sub(to_advance, Ordering::Release);
        }
    }
}
