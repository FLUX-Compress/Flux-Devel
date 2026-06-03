//! C-Compatible public API for FLUX
//!
//! Exposes functions, enums, and structures for use as a dynamic link library
//! (DLL/shared object) from non-Rust environments (C, C++, etc.).
//!
//! # Architecture
//! The in-memory functions (`flux_v1_compress`, `flux_v1_decompress`) bridge
//! the C buffer API to the file-based Rust engine by writing the payload to a
//! temporary file, calling the real engine, and reading the result back.
//! `flux_v1_compress_directory` maps directly to the engine's directory path.
//!
//! # Safety
//! All exported `unsafe extern "C"` functions:
//! - NULL-check every pointer before dereferencing.
//! - Wrap the real work in `std::panic::catch_unwind` so a Rust panic never
//!   unwinds across the C ABI (which is undefined behaviour).
//! - Never write past the caller's output buffer.

use std::os::raw::{c_char, c_void};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// =========================================================================
// Public #[repr(C)] types
// =========================================================================

/// Compression speed vs. efficiency level selector.
///
/// Controls both the algorithm path and the dictionary/window size:
///   Tiny     (0) —  256 KB  Embedded / constrained targets.
///   Fast     (1) —    4 MB  LZ77 + rANS directly, fastest speed.
///   Balanced (2) —   32 MB  PPM + rANS, good speed and ratio. **DEFAULT**
///   Maximum  (3) —  128 MB  Adaptive per-block path selection.
///   Extreme  (4) —  256 MB  Maximum compression, slowest speed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum FluxCompressionLevel {
    Tiny     = 0,
    Fast     = 1,
    Balanced = 2,
    Maximum  = 3,
    Extreme  = 4,
}

/// Returns the window/block size in bytes for a given compression level.
pub fn window_size_for_level(level: FluxCompressionLevel) -> usize {
    match level {
        FluxCompressionLevel::Tiny     =>  256 * 1024,
        FluxCompressionLevel::Fast     =>    4 * 1024 * 1024,
        FluxCompressionLevel::Balanced =>   32 * 1024 * 1024,
        FluxCompressionLevel::Maximum  =>  128 * 1024 * 1024,
        FluxCompressionLevel::Extreme  =>  256 * 1024 * 1024,
    }
}

/// Status code returned by every FLUX FFI function.
///
///   Ok                (0) — success.
///   ErrGeneric        (1) — unclassified error (including caught panics).
///   ErrInvalidParam   (2) — a parameter value was out of range or malformed.
///   ErrNullPointer    (3) — a required pointer was NULL.
///   ErrIo             (4) — filesystem / read / write error.
///   ErrCorruptData    (5) — archive magic, checksum, or structure invalid.
///   ErrWrongPassword  (6) — password was wrong or Argon2id validation failed.
///   ErrBufferTooSmall (7) — output buffer too small; *output_len set to required size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub enum FluxResult {
    Ok               = 0,
    ErrGeneric       = 1,
    ErrInvalidParam  = 2,
    ErrNullPointer   = 3,
    ErrIo            = 4,
    ErrCorruptData   = 5,
    ErrWrongPassword = 6,
    ErrBufferTooSmall = 7,
}

/// Progress snapshot delivered to a `FluxProgressCallback` during an operation.
///
/// All pointer fields within this struct are valid **only for the duration
/// of the callback invocation**. Do not store them past the callback's return.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FluxProgressEvent {
    /// Raw input bytes processed so far.
    pub bytes_processed: u64,
    /// Total raw input bytes for the operation.
    pub bytes_total: u64,
    /// Null-terminated UTF-8 path of the file currently being processed.
    /// NULL when not applicable.
    pub current_file: *const c_char,
    /// Compression ratio so far (compressed / original, e.g. 0.35 = 35 %).
    pub compression_ratio: f32,
    /// Byte stride (1–16) detected by the autocorrelation analyser.
    pub detected_stride: u32,
    /// Fraction of the internal circular buffer that is full (0.0–1.0).
    pub buffer_fill_level: f32,
}

/// Callback type for progress notifications.
///
/// Invoked on the same thread that called the compress/decompress function.
/// Must return quickly — do not block or perform heavy I/O inside it.
/// Pass NULL to any function accepting a callback to disable progress reporting.
pub type FluxProgressCallback = extern "C" fn(event: FluxProgressEvent, user_data: *mut c_void);

/// Compression and decompression options.
///
/// Fields are ordered to eliminate interior padding on 64-bit (LP64) platforms.
/// Always zero-initialise before setting individual fields:
///
/// ```c
/// FluxOptions opts;
/// memset(&opts, 0, sizeof(opts));
/// opts.level = FLUX_LEVEL_BALANCED;
/// ```
///
/// Memory layout on LP64:
///   offset  0 : password     (pointer, 8 bytes)
///   offset  8 : thread_count (uint32_t, 4 bytes)
///   offset 12 : block_size   (uint32_t, 4 bytes)
///   offset 16 : level        (FluxCompressionLevel / C int, 4 bytes)
///   offset 20 : [4-byte end-alignment pad — not a field]
///   sizeof    : 24 bytes
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct FluxOptions {
    /// UTF-8 password for Argon2id key derivation.  NULL disables encryption.
    /// Must remain valid for the entire duration of the API call.
    pub password: *const c_char,
    /// Worker threads.  0 = auto-detect from available CPU cores.
    pub thread_count: u32,
    /// Block size override in bytes (e.g. 1 048 576 = 1 MB).  0 = automatic.
    pub block_size: u32,
    /// Compression level / algorithm selector.
    pub level: FluxCompressionLevel,
}

// =========================================================================
// Internal helpers
// =========================================================================

fn map_archive_error(e: &crate::archive::ArchiveError) -> FluxResult {
    use crate::archive::ArchiveError;
    match e {
        ArchiveError::WrongPassword | ArchiveError::DecryptionFailed => FluxResult::ErrWrongPassword,
        ArchiveError::InvalidMagic
        | ArchiveError::UnsupportedVersion
        | ArchiveError::CorruptIndex
        | ArchiveError::HeaderCorrupt
        | ArchiveError::CorruptBlock(_)
        | ArchiveError::CorruptFile(_) => FluxResult::ErrCorruptData,
        ArchiveError::Io(_) => FluxResult::ErrIo,
    }
}

/// RAII guard — deletes its temp directory on drop (best-effort).
struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> std::io::Result<Self> {
        static CTR: AtomicU64 = AtomicU64::new(0);
        let n = CTR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join(format!("flux_ffi_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Newtype that lets a `*mut c_void` cross `Send + Sync` bounds.
///
/// SAFETY: The C caller is responsible for not aliasing or mutating `user_data`
/// from other threads without external synchronisation, which is the normal
/// contract for C callback `void *` parameters.
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}
impl SendPtr {
    // Use a method so Rust 2021 closure precision captures the whole struct,
    // not just the inner `*mut c_void` field (which is !Send + !Sync).
    fn get(&self) -> *mut c_void { self.0 }
}

/// Builds the engine-internal `ProgressCallback` from a C callback + cookie.
fn make_progress_callback(
    c_cb: FluxProgressCallback,
    user_data: *mut c_void,
) -> crate::ProgressCallback {
    let ud = SendPtr(user_data);
    Some(Arc::new(move |bytes_processed: u64, bytes_total: u64, current_file: String| {
        let c_file = std::ffi::CString::new(current_file.as_bytes()).unwrap_or_default();
        let ratio = if bytes_total > 0 {
            bytes_processed as f32 / bytes_total as f32
        } else {
            0.0
        };
        let event = FluxProgressEvent {
            bytes_processed,
            bytes_total,
            current_file: c_file.as_ptr(),
            compression_ratio: ratio,
            detected_stride: 0,    // not reported on the per-file callback path
            buffer_fill_level: 0.0,
        };
        // c_file is alive here; it drops after c_cb returns (synchronous call).
        c_cb(event, ud.get());
    }))
}

// =========================================================================
// Exported C symbols
// =========================================================================

/// Returns the FLUX core engine version string (e.g. `"0.1.0"`).
///
/// The returned pointer is permanently valid (static storage).  Never free it.
/// Thread-safe: may be called from any thread at any time.
#[no_mangle]
pub extern "C" fn flux_v1_get_version() -> *const c_char {
    // Build a NUL-terminated byte literal at compile time from the Cargo version.
    static V: &[u8] = concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes();
    V.as_ptr() as *const c_char
}

/// Compresses an in-memory buffer and writes the compressed archive bytes into
/// a caller-allocated output buffer.
///
/// Internally, the input is stored as a single blob file inside a temporary
/// FLUX archive. Use `flux_v1_decompress` to recover the original bytes; the
/// two functions are inverses of each other.
///
/// **Buffer sizing**: allocate `input_len + input_len/8 + 4096` bytes for the
/// output buffer as a safe upper bound, then call `flux_v1_get_decompressed_size`
/// on the result to store the exact size alongside the data.
///
/// If the output buffer is too small, `*output_len` is set to the minimum
/// required size and `FluxResult::ErrBufferTooSmall` is returned without
/// touching the output buffer.  On success, `*output_len` is set to the
/// number of bytes written.
///
/// # Safety
/// - `input`  must point to a valid readable region of at least `input_len` bytes (or be NULL only when `input_len == 0`).
/// - `output` must point to a valid writable region of at least `*output_len` bytes.
/// - `output_len` must point to a valid `size_t` variable.
/// - `options.password` (if non-NULL) must remain valid for the call duration.
/// - The callback (if non-NULL) must be thread-safe if the engine uses parallelism.
#[no_mangle]
pub unsafe extern "C" fn flux_v1_compress(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: *mut usize,
    options: FluxOptions,
    callback: Option<FluxProgressCallback>,
    user_data: *mut c_void,
) -> FluxResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compress_buf_impl(input, input_len, output, output_len, options, callback, user_data)
    }))
    .unwrap_or(FluxResult::ErrGeneric)
}

unsafe fn compress_buf_impl(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: *mut usize,
    options: FluxOptions,
    callback: Option<FluxProgressCallback>,
    user_data: *mut c_void,
) -> FluxResult {
    // --- pointer validation ---
    if output.is_null() || output_len.is_null() {
        return FluxResult::ErrNullPointer;
    }
    // Allow input == NULL only when input_len == 0 (empty buffer).
    if input.is_null() && input_len != 0 {
        return FluxResult::ErrNullPointer;
    }

    let src: &[u8] = if input_len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(input, input_len)
    };
    let cap = *output_len;

    // --- temp workspace ---
    let tmp = match TempDir::new() {
        Ok(d) => d,
        Err(_) => return FluxResult::ErrIo,
    };
    let in_path  = tmp.path().join("input.bin");
    let out_path = tmp.path().join("output.flx");

    if std::fs::write(&in_path, src).is_err() {
        return FluxResult::ErrIo;
    }

    // --- run the real engine ---
    let mut comp = crate::FluxCompressor::new(options);
    if let Some(cb) = callback {
        comp.progress_callback = make_progress_callback(cb, user_data);
    }

    if let Err(e) = comp.compress_file(&in_path, &out_path) {
        return map_archive_error(&e);
    }

    let compressed = match std::fs::read(&out_path) {
        Ok(d) => d,
        Err(_) => return FluxResult::ErrIo,
    };

    // --- buffer-size check (do NOT overflow caller buffer) ---
    if compressed.len() > cap {
        *output_len = compressed.len();
        return FluxResult::ErrBufferTooSmall;
    }

    std::ptr::copy_nonoverlapping(compressed.as_ptr(), output, compressed.len());
    *output_len = compressed.len();
    FluxResult::Ok
}

/// Decompresses an in-memory FLUX archive (produced by `flux_v1_compress`) back
/// to a caller-allocated buffer.
///
/// The archive must contain a single blob file named `"input.bin"` as created
/// by `flux_v1_compress`.  To decompress multi-file archives produced by
/// `flux_v1_compress_directory`, use the CLI or the Rust crate API instead.
///
/// Call `flux_v1_get_decompressed_size` to determine the required buffer size
/// before calling this function.
///
/// If the output buffer is too small, `*output_len` is set to the required
/// size and `FluxResult::ErrBufferTooSmall` is returned.  On success,
/// `*output_len` is set to the actual decompressed byte count.
///
/// # Safety
/// - `input` / `output` / `output_len` must all be non-NULL and valid.
/// - `options.password` (if non-NULL) must remain valid for the call duration.
#[no_mangle]
pub unsafe extern "C" fn flux_v1_decompress(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: *mut usize,
    options: FluxOptions,
) -> FluxResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decompress_buf_impl(input, input_len, output, output_len, options)
    }))
    .unwrap_or(FluxResult::ErrGeneric)
}

unsafe fn decompress_buf_impl(
    input: *const u8,
    input_len: usize,
    output: *mut u8,
    output_len: *mut usize,
    options: FluxOptions,
) -> FluxResult {
    if input.is_null() || output.is_null() || output_len.is_null() {
        return FluxResult::ErrNullPointer;
    }

    let src = std::slice::from_raw_parts(input, input_len);
    let cap = *output_len;

    let tmp = match TempDir::new() {
        Ok(d) => d,
        Err(_) => return FluxResult::ErrIo,
    };
    let arc_path = tmp.path().join("archive.flx");
    let out_dir  = tmp.path().join("out");

    if std::fs::write(&arc_path, src).is_err() {
        return FluxResult::ErrIo;
    }
    if std::fs::create_dir_all(&out_dir).is_err() {
        return FluxResult::ErrIo;
    }

    // --- run the real engine ---
    let mut decomp = crate::FluxDecompressor::new(options);
    if let Err(e) = decomp.decompress(&arc_path, &out_dir) {
        return map_archive_error(&e);
    }

    // The archive always contains exactly one file named "input.bin"
    // (the path written by flux_v1_compress).
    let extracted = out_dir.join("input.bin");
    let data = match std::fs::read(&extracted) {
        Ok(d) => d,
        Err(_) => return FluxResult::ErrIo,
    };

    if data.len() > cap {
        *output_len = data.len();
        return FluxResult::ErrBufferTooSmall;
    }

    std::ptr::copy_nonoverlapping(data.as_ptr(), output, data.len());
    *output_len = data.len();
    FluxResult::Ok
}

/// Compresses a directory tree or single file into a FLUX archive on disk.
///
/// If `src_dir` is a directory, the entire tree is archived recursively.
/// If `src_dir` is a regular file, that file alone is archived.
///
/// `dest_archive` is created (or overwritten) at the given path.
///
/// # Safety
/// - `src_dir` and `dest_archive` must be non-NULL, valid null-terminated UTF-8.
/// - `options.password` (if non-NULL) must remain valid for the call duration.
/// - Ensure no concurrent writer is modifying `dest_archive`.
#[no_mangle]
pub unsafe extern "C" fn flux_v1_compress_directory(
    src_dir: *const c_char,
    dest_archive: *const c_char,
    options: FluxOptions,
    callback: Option<FluxProgressCallback>,
    user_data: *mut c_void,
) -> FluxResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compress_dir_impl(src_dir, dest_archive, options, callback, user_data)
    }))
    .unwrap_or(FluxResult::ErrGeneric)
}

unsafe fn compress_dir_impl(
    src_dir: *const c_char,
    dest_archive: *const c_char,
    options: FluxOptions,
    callback: Option<FluxProgressCallback>,
    user_data: *mut c_void,
) -> FluxResult {
    if src_dir.is_null() || dest_archive.is_null() {
        return FluxResult::ErrNullPointer;
    }

    let src_s = match std::ffi::CStr::from_ptr(src_dir).to_str() {
        Ok(s) => s,
        Err(_) => return FluxResult::ErrInvalidParam,
    };
    let dst_s = match std::ffi::CStr::from_ptr(dest_archive).to_str() {
        Ok(s) => s,
        Err(_) => return FluxResult::ErrInvalidParam,
    };

    let src_path = Path::new(src_s);
    let dst_path = Path::new(dst_s);

    let mut comp = crate::FluxCompressor::new(options);
    if let Some(cb) = callback {
        comp.progress_callback = make_progress_callback(cb, user_data);
    }

    let res = if src_path.is_dir() {
        comp.compress_directory(src_path, dst_path)
    } else {
        comp.compress_file(src_path, dst_path)
    };

    match res {
        Ok(_) => FluxResult::Ok,
        Err(e) => map_archive_error(&e),
    }
}

/// Returns the total original (uncompressed) size recorded in the FLUX archive header.
///
/// Use this to pre-allocate the exact output buffer before calling
/// `flux_v1_decompress`, avoiding a trial-and-retry loop.
///
/// For **encrypted** archives, `options.password` must be set — the header
/// itself is encrypted and cannot be read without the correct key.
/// For **unencrypted** archives, pass a zero-initialised `FluxOptions`.
///
/// On success, `*out_size` is set to the total uncompressed byte count and
/// `FluxResult::Ok` is returned.
///
/// # Safety
/// - `input` and `out_size` must be non-NULL and valid.
/// - `options.password` (if non-NULL) must remain valid for the call duration.
#[no_mangle]
pub unsafe extern "C" fn flux_v1_get_decompressed_size(
    input: *const u8,
    input_len: usize,
    options: FluxOptions,
    out_size: *mut usize,
) -> FluxResult {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        get_decomp_size_impl(input, input_len, options, out_size)
    }))
    .unwrap_or(FluxResult::ErrGeneric)
}

unsafe fn get_decomp_size_impl(
    input: *const u8,
    input_len: usize,
    options: FluxOptions,
    out_size: *mut usize,
) -> FluxResult {
    if input.is_null() || out_size.is_null() {
        return FluxResult::ErrNullPointer;
    }

    let src = std::slice::from_raw_parts(input, input_len);

    let tmp = match TempDir::new() {
        Ok(d) => d,
        Err(_) => return FluxResult::ErrIo,
    };
    let arc_path = tmp.path().join("archive.flx");

    if std::fs::write(&arc_path, src).is_err() {
        return FluxResult::ErrIo;
    }

    let mut decomp = crate::FluxDecompressor::new(options);
    match decomp.read_archive_info(&arc_path) {
        Ok((original_size, _, _, _)) => {
            *out_size = original_size as usize;
            FluxResult::Ok
        }
        Err(e) => map_archive_error(&e),
    }
}

/// Releases any resources associated with a `FluxResult` value.
///
/// Currently a documented no-op: all result codes are plain enum variants
/// with no heap ownership.  Call this for forward-compatibility; a future
/// version may attach error strings or context objects to results.
///
/// # Safety
/// Safe to call with any valid `FluxResult`.  Do not call more than once
/// on the same value (future-proofing for versions that allocate state).
#[no_mangle]
pub unsafe extern "C" fn flux_v1_free_result(_result: FluxResult) {}
