//! Inter-thread signal types
//!
//! Provides the message types and control signaling used to synchronize work across
//! the I/O, Buffer Manager, Classifier, Stride Analyzer, Backwards Validator,
//! and Compression worker threads.

/// The classified content category of a file block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentType {
    /// Primarily natural text or highly formatted structured text.
    Text,
    /// Arbitrary machine code, archives, or compiled binaries.
    Binary,
    /// Uncompressed images, raw audio, video frames, or dense tables.
    Multimedia,
    /// Pre-compressed data, encrypted files, or random noise.
    Incompressible,
    /// x86 and x86-64 executable binary code (added in v1.4).
    Executable,
}

/// The suggested pipeline pathway for compressing a block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionPipeline {
    /// Uses optional BWT, PPM modeling, context mixing, and rANS.
    TextPipeline,
    /// Uses LZ77, PPM modeling, and rANS.
    BinaryPipeline,
    /// Uses Adaptive Stride Transposition, LZ77, SSE probability modeling, and rANS.
    MultimediaPipeline,
    /// Direct store raw bytes with zero-compression flags.
    StoreRaw,
}

/// Signals emitted by the Backwards Analysis thread to communicate stride state.
///
/// Sent from the Backwards Analysis Thread to the Adaptive Stride Analyzer Thread
/// to throttle computation or trigger active re-examination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrideSignal {
    /// Stride pattern is stable. Holds the confirmed stride value in bytes (1 to 16).
    /// Sent to allow the Stride Analyzer to throttle down resource utilization.
    Stable(u8),
    /// Stride pattern has drifted or correlation has fallen below threshold.
    /// Sent to wake the Stride Analyzer for recalculation.
    Drifted,
    /// Stride Analyzer is actively recalibrating candidates.
    Recalibrating,
    /// The stride state is unknown or pattern could not be determined.
    Unknown,
    /// Entire file was uniform with a single stride pattern throughout.
    /// Signals that no block boundary re-optimization is needed.
    Uniform(u8),
    /// File contained varied stride patterns across different byte ranges.
    /// Reports the optimal byte ranges and stride sizes to place block boundaries.
    /// The tuple contains (start_offset, end_offset, stride_size).
    VariedPatterns(Vec<(u64, u64, u8)>),
}

/// Signals emitted by the Classifier threads.
///
/// Sent from the ML/Heuristic Classifier Threads to the Compression and Stride Analyzer
/// threads to configure block-level parameters ahead of compression.
#[derive(Debug, Clone)]
pub struct ClassifierSignal {
    /// Detected content category of the block.
    pub content_type: ContentType,
    /// Initial stride hint (if any).
    pub stride_hint: Option<u8>,
    /// Suggested window size for match finding.
    pub window_size_recommendation: usize,
    /// Suggested compression pathway to execute.
    pub pipeline_recommendation: CompressionPipeline,
}

/// Signals emitted by the Buffer Manager thread.
///
/// Sent from the Buffer Manager Thread to all active reader threads to update them
/// on memory slot availability or EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferSignal {
    /// New chunk of raw data is populated in the circular buffer. Holds number of bytes.
    DataAvailable(usize),
    /// The end of file has been reached. Sent to initiate pipeline shutdown.
    EndOfFile,
    /// Buffer fill level falls below 20%. Sent to trigger rapid I/O reads.
    LowWater,
    /// Buffer fill level exceeds 90%. Sent to throttle I/O input reading.
    HighWater,
}

/// Signals sent to control compression worker threads.
///
/// Sent from the coordination/Backwards Analysis thread to worker compression threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionSignal {
    /// Stride is stable. workers bypass full analysis loops and run fast pathways.
    FastPath,
    /// Stride has drifted or content changed. workers run the full pipeline check.
    FullPipeline,
    /// Pause compression (e.g. buffer starves or system throttles).
    Pause,
    /// Shutdown compression threads. Sent during archiver cleanup or abort.
    Shutdown,
}
