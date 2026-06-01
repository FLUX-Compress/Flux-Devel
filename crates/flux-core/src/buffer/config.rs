//! Configuration utilities for FLUX memory allocation.

use sysinfo::System;

/// Minimum buffer capacity (256 MB).
pub const MIN_BUFFER_CAPACITY: usize = 256 * 1024 * 1024;

/// Maximum buffer capacity (8 GB).
pub const MAX_BUFFER_CAPACITY: usize = 8 * 1024 * 1024 * 1024;

/// Detects the recommended buffer capacity based on system resources.
///
/// Queries the total system memory using `sysinfo` and selects 10% of it, bounded
/// by a 256 MB floor and an 8 GB ceiling.
///
/// # Rationale
/// * **10% RAM**: Ensures FLUX has enough lookahead context to analyze large-scale stride patterns
///   without starving the host system's operating memory or causing swap-thrashing.
/// * **256 MB Floor**: Guarantees that even on resource-constrained devices, there is sufficient buffer
///   size to capture repeating structures and delta strides over common multimedia files.
/// * **8 GB Ceiling**: Prevents excessive memory footprint on high-end server hardware, capping memory
///   usage at a point where returns on lookahead context size begin to diminish.
pub fn detect_buffer_capacity() -> usize {
    let mut sys = System::new();
    sys.refresh_memory();
    
    let total_memory = sys.total_memory() as usize;
    let target = total_memory / 10;
    
    target.clamp(MIN_BUFFER_CAPACITY, MAX_BUFFER_CAPACITY)
}
