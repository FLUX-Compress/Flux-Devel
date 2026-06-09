//! Safe, idiomatic Rust wrapper for the FLUX compression and archiving library.
//!
//! Provides a high-level builder pattern API for compressing and extracting archives.
//! This is the public face of FLUX for Rust developers, allowing them to perform
//! solid directory and file archiving with ease.
//!
//! ## Example
//!
//! ```rust,no_run
//! use flux::{Archive, Compression};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Compress a directory structure into a secure solid archive
//! let stats = Archive::compress("./my_project")
//!     .output("backup.flx")
//!     .level(Compression::Maximum)
//!     .password("my_secret_password")
//!     .on_progress(|p| {
//!         println!("{:.1}% — {}", p.percent(), p.current_file());
//!     })
//!     .run()?;
//!
//! println!("Compressed {} files! Ratio: {:.2}x",
//!     stats.files_processed(),
//!     stats.compression_ratio()
//! );
//!
//! // Extract files back securely
//! Archive::extract("backup.flx")
//!     .output("./restored")
//!     .password("my_secret_password")
//!     .verify(true)
//!     .run()?;
//! # Ok(())
//! # }
//! ```
//!
//! ## Licensing
//! FLUX is dual-licensed under:
//! - **GNU General Public License v3**
//! - **FLUX Commercial License**
//!
//! ## Resources
//! - [GitHub Repository](https://github.com/acydd/flux)
//! - [Documentation on docs.rs](https://docs.rs/flux)

use std::fmt;
use std::path::{Path, PathBuf};

// Re-export C-compatible types from ffi via flux-sys just in case, but safe API uses custom enums.
pub use flux_sys::{FluxCompressionLevel, FluxOptions, FluxResult};

/// Compression levels that trade off speed for compression density.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Tiny mode: Small 256 KB window/block for embedded/constrained decompression.
    Tiny,
    /// Fast mode: Uses LZ77 followed directly by rANS (fastest speed, lower ratio, 4 MB window).
    Fast,
    /// Balanced mode (Default): Uses PPM followed by rANS with BWT completely disabled (good speed and ratio, 32 MB window).
    Balanced,
    /// Maximum mode: Evaluates both paths adaptively per-block, keeping the smaller one (highest ratio, slower speed, 128 MB window).
    Maximum,
    /// Extreme mode: Crunches large blocks for maximum compression (256 MB window).
    Extreme,
}

/// Progress status for callbacks during archiving and extraction.
#[derive(Debug, Clone, Default)]
pub struct Progress {
    percent: f32,
    current_file: String,
    bytes_processed: u64,
    bytes_total: u64,
    estimated_seconds_remaining: Option<f32>,
}

impl Progress {
    /// Creates a new `Progress` with specific initial values.
    pub fn new(
        percent: f32,
        current_file: String,
        bytes_processed: u64,
        bytes_total: u64,
        estimated_seconds_remaining: Option<f32>,
    ) -> Self {
        Self {
            percent,
            current_file,
            bytes_processed,
            bytes_total,
            estimated_seconds_remaining,
        }
    }

    /// Returns the progress percentage (0.0 to 100.0).
    pub fn percent(&self) -> f32 {
        if self.bytes_total > 0 {
            (self.bytes_processed as f32 / self.bytes_total as f32) * 100.0
        } else {
            self.percent
        }
    }

    /// Returns the name/path of the file currently being processed.
    pub fn current_file(&self) -> &str {
        &self.current_file
    }

    /// Returns the number of bytes processed so far.
    pub fn bytes_processed(&self) -> u64 {
        self.bytes_processed
    }

    /// Returns the total number of bytes to be processed.
    pub fn bytes_total(&self) -> u64 {
        self.bytes_total
    }

    /// Returns the estimated number of seconds remaining, if available.
    pub fn estimated_seconds_remaining(&self) -> Option<f32> {
        self.estimated_seconds_remaining
    }
}

/// Statistics returned upon successful archiving compression.
#[derive(Debug, Clone)]
pub struct CompressionStats {
    inner: flux_core_v1::CompressionStats,
}

impl CompressionStats {
    /// Returns the number of files processed.
    pub fn files_processed(&self) -> u32 {
        self.inner.files_processed
    }

    /// Returns the original uncompressed size of the archived files in bytes.
    pub fn original_size(&self) -> u64 {
        self.inner.original_size
    }

    /// Returns the final compressed size of the archive in bytes.
    pub fn compressed_size(&self) -> u64 {
        self.inner.compressed_size
    }

    /// Returns the compression ratio (compressed size divided by original size).
    pub fn compression_ratio(&self) -> f32 {
        if self.inner.original_size > 0 {
            self.inner.compressed_size as f32 / self.inner.original_size as f32
        } else {
            1.0
        }
    }

    /// Returns the elapsed time of the compression process in milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        self.inner.elapsed_ms
    }
}

/// Statistics returned upon successful archive extraction.
#[derive(Debug, Clone)]
pub struct DecompressionStats {
    inner: flux_core_v1::DecompressionStats,
}

impl DecompressionStats {
    /// Returns the number of files successfully extracted.
    pub fn files_extracted(&self) -> u32 {
        self.inner.files_extracted
    }

    /// Returns the total bytes written to disk.
    pub fn bytes_written(&self) -> u64 {
        self.inner.bytes_written
    }

    /// Returns the elapsed time of the extraction process in milliseconds.
    pub fn elapsed_ms(&self) -> u64 {
        self.inner.elapsed_ms
    }

    /// Returns whether the archive's multi-level integrity checks passed.
    pub fn integrity_verified(&self) -> bool {
        self.inner.integrity_verified
    }
}

/// High-level errors returned by FLUX wrapper operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FluxError {
    /// General or filesystem I/O error.
    IoError(String),
    /// The archive structure or index checksum validation failed.
    CorruptArchive(String),
    /// The password was incorrect or decryption failed.
    WrongPassword,
    /// A specific file failed integrity checks on extraction.
    IntegrityCheckFailed {
        /// The path of the corrupted file.
        file: String,
    },
    /// The compression algorithm failed to complete.
    CompressionFailed(String),
    /// An input parameter (e.g. source/destination path) was invalid.
    InvalidInput(String),
    /// The archive version or format is not supported.
    UnsupportedFormat(String),
}

impl fmt::Display for FluxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FluxError::IoError(s) => write!(f, "I/O error: {}", s),
            FluxError::CorruptArchive(s) => write!(f, "Corrupt archive: {}", s),
            FluxError::WrongPassword => write!(f, "Incorrect password or decryption failed"),
            FluxError::IntegrityCheckFailed { file } => {
                write!(f, "Integrity check failed for file: {}", file)
            }
            FluxError::CompressionFailed(s) => write!(f, "Compression failed: {}", s),
            FluxError::InvalidInput(s) => write!(f, "Invalid input parameter: {}", s),
            FluxError::UnsupportedFormat(s) => write!(f, "Unsupported archive format: {}", s),
        }
    }
}

impl std::error::Error for FluxError {}

impl From<flux_core_v1::archive::index::ArchiveError> for FluxError {
    fn from(err: flux_core_v1::archive::index::ArchiveError) -> Self {
        match err {
            flux_core_v1::archive::index::ArchiveError::InvalidMagic => {
                FluxError::UnsupportedFormat("Invalid FLUX archive magic bytes".to_string())
            }
            flux_core_v1::archive::index::ArchiveError::UnsupportedVersion => {
                FluxError::UnsupportedFormat("Unsupported FLUX archive version".to_string())
            }
            flux_core_v1::archive::index::ArchiveError::CorruptIndex => {
                FluxError::CorruptArchive("Corrupt archive file index".to_string())
            }
            flux_core_v1::archive::index::ArchiveError::HeaderCorrupt => FluxError::CorruptArchive(
                "Archive header is corrupted, cannot read this file".to_string(),
            ),
            flux_core_v1::archive::index::ArchiveError::CorruptBlock(id) => {
                FluxError::CorruptArchive(format!("Corrupt solid block (id: {})", id))
            }
            flux_core_v1::archive::index::ArchiveError::CorruptFile(path) => {
                FluxError::IntegrityCheckFailed { file: path }
            }
            flux_core_v1::archive::index::ArchiveError::DecryptionFailed => {
                FluxError::WrongPassword
            }
            flux_core_v1::archive::index::ArchiveError::WrongPassword => FluxError::WrongPassword,
            flux_core_v1::archive::index::ArchiveError::Io(s) => FluxError::IoError(s),
        }
    }
}

/// Chaining builder for managing FLUX compression.
pub struct CompressBuilder {
    input: PathBuf,
    output: Option<PathBuf>,
    level: Compression,
    password: Option<String>,
    progress_callback: Option<Box<dyn Fn(Progress) + Send>>,
    verify_after: bool,
    volume_size: u64,
}

impl CompressBuilder {
    /// Specifies the target path of the output archive file.
    pub fn output(mut self, path: impl AsRef<Path>) -> Self {
        self.output = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets the target compression level (Fast, Balanced, Maximum).
    pub fn level(mut self, level: Compression) -> Self {
        self.level = level;
        self
    }

    /// Sets a password for authenticated AES-256-GCM encryption.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Attaches a progress callback handler.
    pub fn on_progress(mut self, callback: impl Fn(Progress) + Send + 'static) -> Self {
        self.progress_callback = Some(Box::new(callback));
        self
    }

    /// Enables or disables post-compression integrity verification.
    pub fn verify(mut self, verify: bool) -> Self {
        self.verify_after = verify;
        self
    }

    /// Sets the target volume size in bytes (0 = single-volume / no splitting).
    pub fn volume_size(mut self, size: u64) -> Self {
        self.volume_size = size;
        self
    }

    /// Runs the builder compression.
    pub fn run(self) -> Result<CompressionStats, FluxError> {
        let out = self.output.ok_or_else(|| {
            FluxError::InvalidInput("No output archive path specified".to_string())
        })?;

        let CompressBuilder {
            input,
            level,
            password,
            progress_callback,
            verify_after,
            ..
        } = self;

        // Calculate bytes_total
        let bytes_total = if input.is_file() {
            std::fs::metadata(&input).map(|m| m.len()).unwrap_or(0)
        } else if input.is_dir() {
            fn get_dir_size(p: &Path) -> u64 {
                let mut size = 0;
                if let Ok(entries) = std::fs::read_dir(p) {
                    for entry in entries.flatten() {
                        if let Ok(meta) = entry.metadata() {
                            if meta.is_file() {
                                size += meta.len();
                            } else if meta.is_dir() {
                                size += get_dir_size(&entry.path());
                            }
                        }
                    }
                }
                size
            }
            get_dir_size(&input)
        } else {
            0
        };

        // Wrap the progress callback so it is thread-safe and shareable
        let progress_cb =
            progress_callback.map(|cb| std::sync::Arc::new(std::sync::Mutex::new(cb)));

        if let Some(ref cb) = progress_cb {
            if let Ok(guard) = cb.lock() {
                guard(Progress {
                    percent: 0.0,
                    current_file: input.to_string_lossy().into_owned(),
                    bytes_processed: 0,
                    bytes_total,
                    estimated_seconds_remaining: None,
                });
            }
        }

        // Keep CString password alive until the end of the method execution.
        let password_cstr = password
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()).unwrap());
        let password_ptr = password_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let core_level = match level {
            Compression::Tiny => FluxCompressionLevel::Tiny,
            Compression::Fast => FluxCompressionLevel::Fast,
            Compression::Balanced => FluxCompressionLevel::Balanced,
            Compression::Maximum => FluxCompressionLevel::Maximum,
            Compression::Extreme => FluxCompressionLevel::Extreme,
        };

        let block_size = flux_core_v1::ffi::window_size_for_level(core_level) as u32;
        if self.volume_size > 0 && self.volume_size < block_size as u64 {
            let level_name = match level {
                Compression::Tiny => "Tiny",
                Compression::Fast => "Fast",
                Compression::Balanced => "Balanced",
                Compression::Maximum => "Maximum",
                Compression::Extreme => "Extreme",
            };
            let block_size_mb = block_size as f64 / 1_048_576.0;
            return Err(FluxError::InvalidInput(format!(
                "Level {} uses {:.2} MB blocks; volume_size must be at least {:.2} MB. Use a lower compression level or increase volume_size.",
                level_name, block_size_mb, block_size_mb
            )));
        }

        let options = FluxOptions {
            level: core_level,
            password: password_ptr,
            thread_count: 0,
            block_size: 0,
            volume_size: self.volume_size,
        };

        let mut compressor = flux_core_v1::FluxCompressor::new(options);

        if let Some(ref cb) = progress_cb {
            let cb_clone = std::sync::Arc::clone(cb);
            compressor.progress_callback = Some(std::sync::Arc::new(
                move |bytes_processed, bytes_total, current_file| {
                    let percent = if bytes_total > 0 {
                        (bytes_processed as f32 / bytes_total as f32) * 100.0
                    } else {
                        0.0
                    };
                    if let Ok(guard) = cb_clone.lock() {
                        guard(Progress {
                            percent,
                            current_file,
                            bytes_processed,
                            bytes_total,
                            estimated_seconds_remaining: None,
                        });
                    }
                },
            ));
        }

        let stats = if input.is_dir() {
            compressor.compress_directory(&input, &out)?
        } else {
            compressor.compress_file(&input, &out)?
        };

        if verify_after {
            let mut decompressor = flux_core_v1::FluxDecompressor::new(options);
            // Verify file structure reads and unpacks cleanly. We can discard stats or just test it.
            let temp_dest = tempfile::tempdir().map_err(|e| FluxError::IoError(e.to_string()))?;
            let d_stats = decompressor.decompress(&out, temp_dest.path())?;
            if !d_stats.integrity_verified {
                return Err(FluxError::CorruptArchive(
                    "Post-verify check failed".to_string(),
                ));
            }
        }

        if let Some(ref cb) = progress_cb {
            if let Ok(guard) = cb.lock() {
                guard(Progress {
                    percent: 100.0,
                    current_file: "".to_string(),
                    bytes_processed: stats.original_size,
                    bytes_total: stats.original_size,
                    estimated_seconds_remaining: Some(0.0),
                });
            }
        }

        Ok(CompressionStats { inner: stats })
    }
}

/// Chaining builder for managing FLUX extraction.
pub struct ExtractBuilder {
    input: PathBuf,
    output: Option<PathBuf>,
    password: Option<String>,
    progress_callback: Option<Box<dyn Fn(Progress) + Send>>,
    verify: bool,
}

impl ExtractBuilder {
    /// Specifies the target directory path where archived files will be restored.
    pub fn output(mut self, path: impl AsRef<Path>) -> Self {
        self.output = Some(path.as_ref().to_path_buf());
        self
    }

    /// Sets the password used for dynamic archive decryption.
    pub fn password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Attaches a progress callback handler.
    pub fn on_progress(mut self, callback: impl Fn(Progress) + Send + 'static) -> Self {
        self.progress_callback = Some(Box::new(callback));
        self
    }

    /// Enforces rigorous multi-level integrity checksum verification.
    pub fn verify(mut self, verify: bool) -> Self {
        self.verify = verify;
        self
    }

    /// Reads and returns basic information about the archive without full decompression.
    /// Returns: (original_size, compressed_size, file_count, is_encrypted)
    pub fn read_metadata(&self) -> Result<(u64, u64, u32, bool), FluxError> {
        let password_cstr = self
            .password
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()).unwrap());
        let password_ptr = password_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: password_ptr,
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };

        let mut decompressor = flux_core_v1::FluxDecompressor::new(options);
        let info = decompressor.read_archive_info(&self.input)?;
        Ok(info)
    }

    /// Lists all file paths preserved inside the archive.
    pub fn list_files(&self) -> Result<Vec<String>, FluxError> {
        let password_cstr = self
            .password
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()).unwrap());
        let password_ptr = password_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: password_ptr,
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };

        let mut decompressor = flux_core_v1::FluxDecompressor::new(options);
        let files = decompressor.list_files(&self.input)?;
        Ok(files)
    }

    /// Runs the builder extraction.
    pub fn run(self) -> Result<DecompressionStats, FluxError> {
        let out = self.output.ok_or_else(|| {
            FluxError::InvalidInput("No output extraction path specified".to_string())
        })?;

        let ExtractBuilder {
            input,
            password,
            progress_callback,
            verify,
            ..
        } = self;

        // Keep CString password alive until the end of the method execution.
        let password_cstr = password
            .as_ref()
            .map(|p| std::ffi::CString::new(p.as_str()).unwrap());
        let password_ptr = password_cstr
            .as_ref()
            .map(|c| c.as_ptr())
            .unwrap_or(std::ptr::null());

        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: password_ptr,
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };

        // Determine total size from metadata
        let mut decompressor = flux_core_v1::FluxDecompressor::new(options);
        let bytes_total = decompressor
            .read_archive_info(&input)
            .map(|info| info.0)
            .unwrap_or(0);

        // Wrap the progress callback so it is thread-safe and shareable
        let progress_cb =
            progress_callback.map(|cb| std::sync::Arc::new(std::sync::Mutex::new(cb)));

        if let Some(ref cb) = progress_cb {
            if let Ok(guard) = cb.lock() {
                guard(Progress {
                    percent: 0.0,
                    current_file: input.to_string_lossy().into_owned(),
                    bytes_processed: 0,
                    bytes_total,
                    estimated_seconds_remaining: None,
                });
            }
        }

        if let Some(ref cb) = progress_cb {
            let cb_clone = std::sync::Arc::clone(cb);
            decompressor.progress_callback = Some(std::sync::Arc::new(
                move |bytes_processed, bytes_total, current_file| {
                    let percent = if bytes_total > 0 {
                        (bytes_processed as f32 / bytes_total as f32) * 100.0
                    } else {
                        0.0
                    };
                    if let Ok(guard) = cb_clone.lock() {
                        guard(Progress {
                            percent,
                            current_file,
                            bytes_processed,
                            bytes_total,
                            estimated_seconds_remaining: None,
                        });
                    }
                },
            ));
        }

        let stats = decompressor.decompress(&input, &out)?;

        if verify && !stats.integrity_verified {
            return Err(FluxError::CorruptArchive(
                "Integrity verification failed during extraction".to_string(),
            ));
        }

        if let Some(ref cb) = progress_cb {
            if let Ok(guard) = cb.lock() {
                guard(Progress {
                    percent: 100.0,
                    current_file: "".to_string(),
                    bytes_processed: stats.bytes_written,
                    bytes_total: stats.bytes_written,
                    estimated_seconds_remaining: Some(0.0),
                });
            }
        }

        Ok(DecompressionStats { inner: stats })
    }
}

/// High-level entry point for FLUX archiving and extraction operations.
pub struct Archive;

impl Archive {
    /// Starts the builder pattern for solid compression.
    pub fn compress(input: impl AsRef<Path>) -> CompressBuilder {
        CompressBuilder {
            input: input.as_ref().to_path_buf(),
            output: None,
            level: Compression::Balanced,
            password: None,
            progress_callback: None,
            verify_after: false,
            volume_size: 0,
        }
    }

    /// Starts the builder pattern for solid archive extraction.
    pub fn extract(input: impl AsRef<Path>) -> ExtractBuilder {
        ExtractBuilder {
            input: input.as_ref().to_path_buf(),
            output: None,
            password: None,
            progress_callback: None,
            verify: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_builder_default_options() {
        let builder = Archive::compress("src_dir");
        assert_eq!(builder.input, PathBuf::from("src_dir"));
        assert!(builder.output.is_none());
        assert_eq!(builder.level, Compression::Balanced);
        assert!(builder.password.is_none());
        assert!(!builder.verify_after);
        assert_eq!(builder.volume_size, 0);
    }

    #[test]
    fn test_compress_builder_chaining() {
        let builder = Archive::compress("src_dir")
            .output("archive.flx")
            .level(Compression::Maximum)
            .password("pass123")
            .on_progress(|_| {})
            .verify(true)
            .volume_size(100_000_000);

        assert_eq!(builder.output.unwrap(), PathBuf::from("archive.flx"));
        assert_eq!(builder.level, Compression::Maximum);
        assert_eq!(builder.password.unwrap(), "pass123");
        assert!(builder.progress_callback.is_some());
        assert!(builder.verify_after);
        assert_eq!(builder.volume_size, 100_000_000);
    }

    #[test]
    fn test_extract_builder_chaining() {
        let builder = Archive::extract("archive.flx")
            .output("dest_dir")
            .password("pass123")
            .on_progress(|_| {})
            .verify(false);

        assert_eq!(builder.output.unwrap(), PathBuf::from("dest_dir"));
        assert_eq!(builder.password.unwrap(), "pass123");
        assert!(builder.progress_callback.is_some());
        assert!(!builder.verify);
    }

    #[test]
    fn test_flux_error_display() {
        let err1 = FluxError::IoError("missing".to_string());
        assert_eq!(format!("{}", err1), "I/O error: missing");

        let err2 = FluxError::CorruptArchive("header".to_string());
        assert_eq!(format!("{}", err2), "Corrupt archive: header");

        let err3 = FluxError::WrongPassword;
        assert_eq!(
            format!("{}", err3),
            "Incorrect password or decryption failed"
        );

        let err4 = FluxError::IntegrityCheckFailed {
            file: "test.txt".to_string(),
        };
        assert_eq!(
            format!("{}", err4),
            "Integrity check failed for file: test.txt"
        );

        let err5 = FluxError::CompressionFailed("oom".to_string());
        assert_eq!(format!("{}", err5), "Compression failed: oom");

        let err6 = FluxError::InvalidInput("empty".to_string());
        assert_eq!(format!("{}", err6), "Invalid input parameter: empty");
    }

    #[test]
    fn test_compression_stats_ratio() {
        let inner = flux_core_v1::CompressionStats {
            original_size: 1000,
            compressed_size: 250,
            compression_ratio: 0.25,
            files_processed: 5,
            elapsed_ms: 120,
            blocks_created: 1,
        };
        let stats = CompressionStats { inner };
        assert_eq!(stats.original_size(), 1000);
        assert_eq!(stats.compressed_size(), 250);
        assert_eq!(stats.compression_ratio(), 0.25);
        assert_eq!(stats.files_processed(), 5);
        assert_eq!(stats.elapsed_ms(), 120);
    }

    #[test]
    fn test_progress_percent() {
        let p = Progress {
            percent: 0.0,
            current_file: "test.txt".to_string(),
            bytes_processed: 50,
            bytes_total: 100,
            estimated_seconds_remaining: None,
        };
        assert_eq!(p.percent(), 50.0);
    }
}
