//! # Buffer Management Group
//!
//! ## Purpose
//! The Buffer Management Group coordinates memory allocation and data streaming. It allocates
//! a large, thread-safe circular buffer in memory, refilling the tail dynamically as data is
//! consumed from the head.
//!
//! ## Rationale: The Always-Full Lookahead Buffer
//! Keeping the circular buffer perpetually filled is critical for FLUX's stride detection.
//! The adaptive stride analyzer relies on autocorrelation over massive lookahead arrays. If the
//! compression threads starved or the buffer emptied between files, correlation patterns would
//! break, causing fallback to slower/suboptimal compression pathways.
//!
//! ## The Babysitter Thread Pattern
//! FLUX implements a single-writer dedicated thread called the `BufferManager` (the "Babysitter").
//! The Babysitter runs in an independent loop, reading raw files from the source stream and
//! stuffing them into the circular buffer's tail. It wakes up when consumers advance the head
//! and sleeps briefly when the buffer is saturated.
//!
//! ## Thread Interactions
//! ```text
//!    [ Disk I/O ]
//!         │
//!         ▼ (Reads file data)
//!  +---------------+
//!  | BufferManager |  ---(Writes tail)---> [ CircularBuffer (Mirrored) ]
//!  +---------------+                                  │
//!          │                                          ├──► [Classifier Thread]
//!          ▼ (Emits BufferSignals)                    ├──► [Stride Analyzer]
//!     (Channels) ─────────────────────────────────────┼──► [Compression Workers]
//!                                                     └──► [Backwards Validator]
//!                                                              │
//!                                                   (Advances head on complete)
//! ```
//!
//! *   **I/O / BufferManager**: Single writer that appends incoming data to the circular buffer.
//! *   **Classifier, Stride Analyzer, Compression Workers**: Read-only threads that inspect the
//!     uncompressed window buffer concurrently.
//! *   **Backwards Validator**: Inspects the history behind the compression head.
//!
//! ## Thread Safety Guarantees
//! All buffer access is lock-free and thread-safe. Single-writer synchronization is maintained
//! using `AtomicUsize` head/tail indices with Acquire/Release memory ordering guarantees.
//! Read access to the circular buffer is mirrored in a virtual double-buffer to guarantee
//! contiguous memory access across ring-boundaries without mutex overhead.

pub mod circular;
pub mod config;
pub mod manager;
pub mod window;

pub use config::detect_buffer_capacity;
