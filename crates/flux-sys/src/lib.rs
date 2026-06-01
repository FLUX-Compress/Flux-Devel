//! # Raw low-level FFI bindings to the FLUX core engine
//!
//! `flux-sys` provides raw, unsafe C-compatible FFI bindings for interfacing with the
//! compiled FLUX core dynamic library (DLL).
//!
//! ## Conventions
//! Following standard Rust convention, this crate is suffixed with `-sys` to designate
//! it as a low-level, unsafe binding layer. Most developers should use the safe,
//! idiomatic interface provided by the [`flux`](../flux/index.html) crate instead of calling these
//! raw bindings directly.
//!
//! ## Native Code Mapping
//! The types and functions defined here map directly to the FFI structure declared in
//! `crates/flux-core/src/ffi.rs`.
//!
//! ## Auto-Generation
//! While this binding layer is hand-written here for clarity and documentation quality,
//! in a production release environment it would be auto-generated from the core C header
//! files using `bindgen`.

use std::os::raw::{c_char, c_void};

// Re-export C-compatible types from the core FFI module.
pub use flux_core_v1::ffi::{
    FluxCompressionLevel, FluxOptions, FluxProgressCallback, FluxProgressEvent, FluxResult,
};

extern "C" {
    /// Compresses an in-memory input buffer into a destination buffer.
    ///
    /// # Parameters
    /// * `input` - Pointer to the source data buffer.
    /// * `input_len` - Size of the source data buffer in bytes.
    /// * `output` - Pointer to the destination buffer where compressed data will be stored.
    /// * `output_len` - Pointer to the size of the output buffer. Writes actual compressed size on success.
    /// * `options` - Configuration options structure.
    /// * `callback` - Optional callback function for tracking progress (nullable).
    /// * `user_data` - Arbitrary pointer passed directly to the progress callback (nullable).
    ///
    /// # Safety
    /// The caller must guarantee that:
    /// * `input` points to a valid memory block of at least `input_len` bytes.
    /// * `output` points to a valid mutable memory block of at least the dereferenced size of `output_len` bytes.
    /// * `output_len` points to a valid, initialized `usize` variable.
    /// * Memory accessed is not mutated concurrently by other threads.
    ///
    /// # Safe Wrapper
    /// Developers should use the safe builder API [`flux::Archive::compress`](../flux/struct.Archive.html#method.compress) instead.
    pub fn flux_v1_compress(
        input: *const u8,
        input_len: usize,
        output: *mut u8,
        output_len: *mut usize,
        options: FluxOptions,
        callback: Option<FluxProgressCallback>,
        user_data: *mut c_void,
    ) -> FluxResult;

    /// Decompresses an in-memory input buffer into a destination buffer.
    ///
    /// # Parameters
    /// * `input` - Pointer to the compressed data buffer.
    /// * `input_len` - Size of the compressed data buffer in bytes.
    /// * `output` - Pointer to the destination buffer where extracted data will be stored.
    /// * `output_len` - Pointer to the size of the output buffer. Writes actual decompressed size on success.
    /// * `options` - Configuration options structure (e.g. passwords).
    ///
    /// # Safety
    /// The caller must guarantee that:
    /// * `input` points to a valid memory block of at least `input_len` bytes.
    /// * `output` points to a valid mutable memory block of at least the dereferenced size of `output_len` bytes.
    /// * `output_len` points to a valid, initialized `usize` variable.
    /// * Memory accessed is not mutated concurrently by other threads.
    ///
    /// # Safe Wrapper
    /// Developers should use the safe builder API [`flux::Archive::extract`](../flux/struct.Archive.html#method.extract) instead.
    pub fn flux_v1_decompress(
        input: *const u8,
        input_len: usize,
        output: *mut u8,
        output_len: *mut usize,
        options: FluxOptions,
    ) -> FluxResult;

    /// Compresses a directory tree into a single solid archive.
    ///
    /// # Parameters
    /// * `src_dir` - Null-terminated UTF-8 string containing the source directory path.
    /// * `dest_archive` - Null-terminated UTF-8 string containing the target archive file path.
    /// * `options` - Configuration options structure.
    /// * `callback` - Optional progress tracking callback function (nullable).
    /// * `user_data` - Arbitrary pointer passed directly to the progress callback (nullable).
    ///
    /// # Safety
    /// The caller must guarantee that:
    /// * `src_dir` and `dest_archive` point to valid null-terminated C strings.
    /// * The file paths are accessible on the underlying file system.
    /// * No concurrent writers modify the target paths.
    ///
    /// # Safe Wrapper
    /// Developers should use the safe builder API [`flux::Archive::compress`](../flux/struct.Archive.html#method.compress) instead.
    pub fn flux_v1_compress_directory(
        src_dir: *const c_char,
        dest_archive: *const c_char,
        options: FluxOptions,
        callback: Option<FluxProgressCallback>,
        user_data: *mut c_void,
    ) -> FluxResult;

    /// Returns the static version string of the FLUX core engine.
    ///
    /// # Safe Wrapper
    /// Developers can retrieve the engine version info cleanly without unsafe blocks.
    pub fn flux_v1_get_version() -> *const c_char;

    /// Deallocates memory associated with any allocated result or error state if needed.
    ///
    /// # Parameters
    /// * `result` - The result code representing memory or status block to free.
    ///
    /// # Safety
    /// This function is safe to call, but must only be used on valid `FluxResult` values.
    ///
    /// # Safe Wrapper
    /// Disposed automatically by the safe wrapper's RAII drop handlers.
    pub fn flux_v1_free_result(result: FluxResult);
}
