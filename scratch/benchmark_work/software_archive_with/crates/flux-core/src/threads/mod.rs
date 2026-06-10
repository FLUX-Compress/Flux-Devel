//! # Threading Group
//!
//! ## Purpose
//! Coordinates FLUX's multi-threaded pipeline. Instead of a single compression thread
//! or generic thread pool, FLUX allocates dedicated roles to prevent bottlenecks and ensure
//! latency-free data classification and stride tracking.
//!
//! ## Threading Model Diagram
//!
//! ```text
//!  +------------+
//!  | I/O Thread | --[Raw Bytes]--> ( Circular Buffer )
//!  +------------+                         │
//!                                         ├────────────────────────┐
//!                                         ▼                        ▼
//!                              +--------------------+    +--------------------+
//!                              | ML Classifiers     |    | Stride Analyzer    |
//!                              | (2-3 threads ahead)|    | (Autocorrelation)  |
//!                              +--------------------+    +--------------------+
//!                                        │                         │
//!                              [ClassifierSignals]          [StrideSignals]
//!                                        │                         │
//!                                        ▼                         ▼
//!                             =========================================
//!                                      Work Queue / Channels
//!                             =========================================
//!                                                  │
//!                                                  ▼
//!                                     +--------------------------+
//!                        ┌──────────> | Compression Workers      |
//!                        │            | (Multi-core rANS/PPM)    |
//!                        │            +--------------------------+
//!                [CompressionSignal]               │
//!                        │                         ▼
//!                        │                  (Compressed Block)
//!                        │                         │
//!             +--------------------+               ▼
//!             | Backwards Analysis | <─────────────┘
//!             | Thread             |
//!             +--------------------+
//! ```
//!
//! ## Thread Roles and Synchronization
//!
//! *   **I/O Thread**: Pulls file chunks from disk and fills the Circular Buffer.
//! *   **Buffer Manager**: "The Babysitter" thread managing buffer allocations and boundaries.
//! *   **ML Classifiers (2-3)**: Parse upcoming files to produce metadata/pipeline guidelines.
//! *   **Stride Analyzer**: Calculates byte autocorrelation on current lookahead blocks.
//! *   **Compression Workers**: Perform heavy lifting using precomputed parameter bundles.
//! *   **Backwards Analyzer**: Evaluates output stream quality and triggers stride changes or throttles.
//!
//! Synchronization is maintained via lock-free `crossbeam` channels.

pub mod pipeline;
pub mod pool;
pub mod signals;
