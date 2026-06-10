//! # Analysis Group
//!
//! ## Purpose
//! The analysis group provides the intelligence layer of the FLUX compression pipeline.
//! Before compression begins, it analyzes block streams to estimate entropy, detect magic
//! byte file signatures, and identify repeating byte stride correlation patterns.
//! This classification and analysis occurs ahead of the compression threads, enabling FLUX
//! to configure its compression pipeline dynamically for each block.
//!
//! ## Pipeline-Ahead Pattern
//! To prevent compression workers from starving or wasting CPU cycles on redundant analysis,
//! the analysis steps run on independent, dedicated threads that process the incoming circular buffer
//! data ahead of the compression head.
//!
//! ## Key Components and Their Relationships
//! 1. **`EntropyEstimator`**: Calculates Shannon entropy (in bits/byte) and byte frequency histograms.
//!    This acts as the primary tool for detecting randomness, redundancy, and zero-padded runs.
//! 2. **`ContentClassifier`**: Uses magic bytes and entropy statistics to assign a `ContentType`
//!    and `CompressionPipeline` to each block (e.g. Text, Binary, Multimedia, or Incompressible).
//!    In V1, it utilizes fast heuristics; in V2, it is designed to be replaced with trained ML models.
//!    It also extracts a preliminary stride hint.
//! 3. **`StrideDetector`**: Computes Pearson autocorrelation across 16 potential byte strides (1–16).
//!    It refines the classifier's hints and confirms stride patterns over multiple sliding windows.
//! 4. **`BackwardsAnalyzer`** (managed by a separate validator thread): Monitors compressed output block
//!    statistics to confirm whether the detected stride remains stable or has drifted, providing
//!    feedback to throttle or wake the `StrideDetector`.
//!
//! ## Thread Ownership
//! * **Classifier Threads (2-3 threads)**: Concurrently read upcoming lookahead windows from the
//!   `SlidingWindow` and send block metadata bundles (`ClassifierSignal`) down a channel.
//!   These threads own instances of `ContentClassifier`.
//! * **Adaptive Stride Analyzer Thread (1 thread)**: Periodically reads lookahead slices to compute
//!   autocorrelation and confirm strides. It owns the `StrideDetector` and updates the shared window hints.
//! * **Backwards Analysis Thread (1 thread)**: Inspects the trailing edges of processed/compressed blocks
//!   to detect stride drift, sending `StrideSignal` back to control the Stride Analyzer.
//!
//! ## Data Flow Diagram
//!
//! ```text
//!                      Raw Input / Circular Buffer
//!                                   │
//!         ┌─────────────────────────┼────────────────────────┐
//!         ▼ (Lookahead: 2-3 blocks) ▼ (Lookahead: 1 block)   │
//!   ┌───────────┐             ┌───────────┐                  │
//!   │Classifier │             │  Stride   │                  │ (SlidingWindow)
//!   │  Threads  │             │ Detector  │                  │
//!   └─────┬─────┘             └─────┬─────┘                  │
//!         │                         │                        │
//!         │ (ClassifierSignal)      │ (Stride Hint)          │
//!         └───────────┬─────────────┘                        │
//!                     ▼                                      │
//!        [Parameter / Signal Bundle]                         │
//!                     │                                      │
//!                     ▼                                      │
//!         ┌───────────────────────┐                          │
//!         │  Compression Workers  │ ◄────────────────────────┘
//!         │(PPMd, LZ77, SSE, rANS)│
//!         └───────────┬───────────┘
//!                     │
//!                     ▼ (Compressed Data)
//!         ┌───────────────────────┐
//!         │  Backwards Validator  │ ──(StrideSignal::Stable/Drifted)──► [Stride Detector]
//!         └───────────────────────┘
//! ```
//!
//! ## Stride Control Feedback Loop
//!
//! ```text
//!   [Buffer] ──► [Stride Analyzer (Hot/Warm/Cold)]
//!                      ▲
//!                      │ Stable/Drifted feedback signals
//!                      │
//!                [Backwards Analyzer] ◄── validates history
//!                      │
//!                      ▼ EOF analysis
//!                [Pipeline] ◄── block boundary recommendations
//! ```
//!
//! ## The Parameter Bundle Concept
//! Rather than forcing compression worker threads to make complex runtime decisions, the analysis group
//! generates a complete "decision bundle" (packaged as `ClassifierSignal` and updated in `SlidingWindow` hints).
//! Compression threads simply consume these parameters (e.g. which pipeline to run, which stride to use,
//! and what match-finding window sizes to recommend) and execute without thinking, maximizing throughput.
//!
//! ## Thread Safety Guarantees
//! All shared structs, such as the `SlidingWindow` and indices, synchronize via atomic operations
//! (`AtomicUsize`, `AtomicU8`) using strict Acquire/Release memory ordering. Communication across thread
//! boundaries utilizes lock-free `crossbeam` channels. Structs inside this group are `Send` + `Sync`.

pub mod backwards;
pub mod classifier;
pub mod entropy;
pub mod stride;
pub mod executable_detect;

pub use backwards::BackwardsAnalyzer;
pub use classifier::{ClassificationResult, ContentClassifier, FileSignature};
pub use entropy::EntropyEstimator;
pub use stride::{AnalysisMode, StrideCandidate, StrideDetector};

