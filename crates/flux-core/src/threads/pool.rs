//! Thread pool allocation
//!
//! Allocates and coordinates background workers using rayon/std.

/// Worker pool for compression and integrity checking.
pub struct ThreadPool;

impl ThreadPool {
    /// Creates a new ThreadPool.
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for ThreadPool {
    fn default() -> Self {
        Self::new()
    }
}
