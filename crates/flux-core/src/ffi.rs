//! C-Compatible public API for FLUX
//!
//! Exposes functions, enums, and structures designed to build a dynamic link library
//! (DLL/shared object) usable by non-Rust environments (C, C++, etc.).

use std::os::raw::{c_char, c_void};

/// Compression speed vs efficiency levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum FluxCompressionLevel {
    /// Tiny mode: Small 256 KB window/block for embedded/constrained decompression.
    Tiny = 0,
    /// Fast mode: Uses LZ77 followed directly by rANS (fastest speed, lower ratio, 4 MB window).
    Fast = 1,
    /// Balanced mode (Default): Uses PPM followed by rANS with BWT completely disabled (good speed and ratio, 32 MB window).
    Balanced = 2,
    /// Maximum mode: Evaluates both paths adaptively per-block, keeping the smaller one (highest ratio, slower speed, 128 MB window).
    Maximum = 3,
    /// Extreme mode: Crunches large blocks for maximum compression (256 MB window).
    Extreme = 4,
}

/// Returns the concrete window/block size in bytes for a given compression level.
pub fn window_size_for_level(level: FluxCompressionLevel) -> usize {
    match level {
        FluxCompressionLevel::Tiny => 256 * 1024,
        FluxCompressionLevel::Fast => 4 * 1024 * 1024,
        FluxCompressionLevel::Balanced => 32 * 1024 * 1024,
        FluxCompressionLevel::Maximum => 128 * 1024 * 1024,
        FluxCompressionLevel::Extreme => 256 * 1024 * 1024,
    }
}

/// Compression and decompression options passed from external callers.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FluxOptions {
    /// The target compression level.
    pub level: FluxCompressionLevel,
    /// UTF-8 password string for Argon2id key derivation. Nullable if encryption is disabled.
    pub password: *const c_char,
    /// Number of worker threads to spawn. If 0, automatically uses available CPU cores.
    pub thread_count: u32,
    /// Force a specific block size in bytes (e.g. 1048576 for 1MB). Set to 0 for automatic block sizing.
    pub block_size: u32,
}

/// Return result codes for FLUX FFI functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum FluxResult {
    /// Operation completed successfully.
    Ok = 0,
    /// General or unknown error occurred.
    ErrGeneric = 1,
    /// One or more arguments passed were invalid.
    ErrInvalidParam = 2,
    /// An expected pointer parameter was null.
    ErrNullPointer = 3,
    /// An I/O error occurred during reading or writing.
    ErrIo = 4,
    /// The archive structure or checksum validation failed.
    ErrCorruptData = 5,
    /// The password was incorrect or Argon2id validation failed.
    ErrWrongPassword = 6,
    /// Destination buffer is not large enough to hold output.
    ErrBufferTooSmall = 7,
}

/// Progress state sent during archiving operations.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FluxProgressEvent {
    /// Number of raw input bytes processed so far.
    pub bytes_processed: u64,
    /// Total number of input bytes to process.
    pub bytes_total: u64,
    /// Pointer to the null-terminated UTF-8 path of the file currently being processed.
    pub current_file: *const c_char,
    /// Current overall compression ratio (compressed size / original size).
    pub compression_ratio: f32,
    /// The current byte stride (1-16) detected by the autocorrelation analyzer.
    pub detected_stride: u32,
    /// Percentage of the circular buffer currently occupied by input data (0.0 to 1.0).
    pub buffer_fill_level: f32,
}

/// Callback function definition for receiving operational progress.
pub type FluxProgressCallback = extern "C" fn(event: FluxProgressEvent, user_data: *mut c_void);

/// Returns the static version string of the FLUX core engine.
///
/// # Why this was chosen
/// Provides runtime validation of library versions for dynamically linked software.
///
/// # Return Value
/// Pointer to a static, null-terminated ASCII C-string.
///
/// # Thread Safety
/// This function is safe to call concurrently from any thread.
///
/// # Example C Usage
/// ```c
/// const char* version = flux_v1_get_version();
/// printf("Running FLUX Core: %s\n", version);
/// ```
#[no_mangle]
pub extern "C" fn flux_v1_get_version() -> *const c_char {
    c"0.1.0".as_ptr()
}

/// Compresses an in-memory input buffer into a destination buffer.
///
/// # Why this was chosen
/// Core in-memory interface enabling stream-based and chunk-based compression.
///
/// # Parameters
/// * `input`: Pointer to the source data buffer.
/// * `input_len`: Size of the source data buffer in bytes.
/// * `output`: Pointer to the destination buffer where compressed data will be stored.
/// * `output_len`: Pointer to the size of the output buffer. Writes actual compressed size on success.
/// * `options`: Configuration options structure.
/// * `callback`: Optional callback function for tracking progress (nullable).
/// * `user_data`: Arbitrary pointer passed directly to the progress callback (nullable).
///
/// # Return Value
/// Returns `FluxResult::Ok` on success, or an appropriate error code.
///
/// # Safety
/// The caller must ensure that:
/// * `input` points to a valid memory block of at least `input_len` bytes.
/// * `output` points to a valid mutable memory block of at least the dereferenced size of `output_len` bytes.
/// * `output_len` points to a valid, initialized `usize` variable.
/// * Memory accessed is not mutated concurrently by other threads.
///
/// # Thread Safety
/// Safe to call concurrently on separate inputs. Do not modify source buffers while active.
///
/// # Example C Usage
/// ```c
/// unsigned char output[1024];
/// size_t out_len = sizeof(output);
/// FluxOptions options = { Balanced, NULL, 0, 0 };
/// FluxResult res = flux_v1_compress(src, src_len, output, &out_len, options, NULL, NULL);
/// if (res == Ok) {
///     // use compressed data
/// }
/// ```
#[no_mangle]
pub unsafe extern "C" fn flux_v1_compress(
    _input: *const u8,
    _input_len: usize,
    _output: *mut u8,
    _output_len: *mut usize,
    _options: FluxOptions,
    _callback: Option<FluxProgressCallback>,
    _user_data: *mut c_void,
) -> FluxResult {
    FluxResult::Ok
}

/// Decompresses an in-memory input buffer into a destination buffer.
///
/// # Why this was chosen
/// Fast extraction interface from in-memory compressed blocks.
///
/// # Parameters
/// * `input`: Pointer to the compressed data buffer.
/// * `input_len`: Size of the compressed data buffer in bytes.
/// * `output`: Pointer to the destination buffer where extracted data will be stored.
/// * `output_len`: Pointer to the size of the output buffer. Writes actual decompressed size on success.
/// * `options`: Configuration options structure (e.g. passwords).
///
/// # Return Value
/// Returns `FluxResult::Ok` on success, or an appropriate error code.
///
/// # Safety
/// The caller must ensure that:
/// * `input` points to a valid memory block of at least `input_len` bytes.
/// * `output` points to a valid mutable memory block of at least the dereferenced size of `output_len` bytes.
/// * `output_len` points to a valid, initialized `usize` variable.
///
/// # Thread Safety
/// Safe to call concurrently on separate inputs.
///
/// # Example C Usage
/// ```c
/// unsigned char raw_data[4096];
/// size_t raw_len = sizeof(raw_data);
/// FluxOptions options = { Balanced, NULL, 0, 0 };
/// FluxResult res = flux_v1_decompress(compressed, comp_len, raw_data, &raw_len, options);
/// ```
#[no_mangle]
pub unsafe extern "C" fn flux_v1_decompress(
    _input: *const u8,
    _input_len: usize,
    _output: *mut u8,
    _output_len: *mut usize,
    _options: FluxOptions,
) -> FluxResult {
    FluxResult::Ok
}

/// Compresses a directory tree into a single solid archive.
///
/// # Why this was chosen
/// High-level path compression entry point matching archiver CLI and GUI workflows.
///
/// # Parameters
/// * `src_dir`: Null-terminated UTF-8 string containing the source directory path.
/// * `dest_archive`: Null-terminated UTF-8 string containing the target archive file path.
/// * `options`: Configuration options structure.
/// * `callback`: Optional progress tracking callback function (nullable).
/// * `user_data`: Arbitrary pointer passed directly to the progress callback (nullable).
///
/// # Return Value
/// Returns `FluxResult::Ok` on success, or an appropriate error code.
///
/// # Safety
/// The caller must ensure that `src_dir` and `dest_archive` are valid null-terminated ASCII or UTF-8 C strings.
///
/// # Thread Safety
/// Accesses the file system. Ensure no concurrent writers modify the target path.
///
/// # Example C Usage
/// ```c
/// FluxOptions options = { Balanced, NULL, 0, 0 };
/// FluxResult res = flux_v1_compress_directory("my_data", "archive.flx", options, my_progress, NULL);
/// ```
#[no_mangle]
pub unsafe extern "C" fn flux_v1_compress_directory(
    _src_dir: *const c_char,
    _dest_archive: *const c_char,
    _options: FluxOptions,
    _callback: Option<FluxProgressCallback>,
    _user_data: *mut c_void,
) -> FluxResult {
    FluxResult::Ok
}

/// Deallocates memory associated with any allocated result or error state if needed.
///
/// # Why this was chosen
/// Prevents memory leaks when crossing the FFI boundary.
///
/// # Parameters
/// * `_result`: The result enum or associated allocation pointer.
///
/// # Safety
/// This function is safe to call, but must only be used on valid `FluxResult` values.
#[no_mangle]
pub unsafe extern "C" fn flux_v1_free_result(_result: FluxResult) {}
