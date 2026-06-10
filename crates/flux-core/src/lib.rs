//! # FLUX Core Compression Engine
//!
//! `flux-core` is the central engine of the FLUX compression and archiving library.
//! It implements a multi-threaded pipeline combining adaptive stride transposition,
//! prediction by partial matching (PPM), secondary symbol estimation, and range-Asymmetric
//! Numeral Systems (rANS) entropy coding.

pub mod analysis;
pub mod archive;
pub mod buffer;
pub mod compress;
pub mod crypto;
pub mod ffi;
pub mod integrity;
pub mod threads;
pub mod transform;

/// Checks if debug logging is enabled via the `FLUX_DEBUG` environment variable.
#[doc(hidden)]
pub fn debug_enabled() -> bool {
    use std::sync::OnceLock;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FLUX_DEBUG").is_ok())
}

/// Macro for conditional debug prints depending on the `FLUX_DEBUG` environment variable.
#[macro_export]
macro_rules! flux_debug {
    ($($arg:tt)*) => {
        if $crate::debug_enabled() {
            eprintln!($($arg)*);
        }
    };
}

// Re-exports
pub use buffer::detect_buffer_capacity;
pub use ffi::{FluxCompressionLevel, FluxOptions, FluxProgressEvent, FluxResult};
pub use integrity::ChecksumSet;
pub use threads::signals::{
    BufferSignal, ClassifierSignal, CompressionPipeline, CompressionSignal, ContentType,
    StrideSignal,
};

/// Bounded block size for transform and compression stages (1 MB).
pub const COMPRESSION_BLOCK_SIZE: usize = 1_048_576;

use crossbeam::channel::unbounded;
use rand::RngCore;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use archive::{ArchiveError, DirectoryTree, FileIndex, SolidBlockGrouper};
use buffer::circular::CircularBuffer;
use compress::lz77::Lz77Token;
use integrity::{Crc32Hasher, MultiLevelIntegrity, ParallelCrc32, Sha256Hasher};
use std::cell::RefCell;
use transform::{TransformStack, TransformDeserializationError};

thread_local! {
    static TIMING_ENABLED: RefCell<bool> = const { RefCell::new(false) };
    static SCAN_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static CLASSIFY_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static TRANSFORM_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static LZ77_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static PPM_PREDICT_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static PPM_UPDATE_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static PPM_HASH_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static RANS_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static FREQ_TABLE_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static CHECKSUM_TIME: RefCell<f64> = const { RefCell::new(0.0) };
    static WRITE_TIME: RefCell<f64> = const { RefCell::new(0.0) };
}

fn is_timing_enabled() -> bool {
    TIMING_ENABLED.with(|enabled| *enabled.borrow())
}

fn add_scan_time(d: f64) {
    if is_timing_enabled() {
        SCAN_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_classify_time(d: f64) {
    if is_timing_enabled() {
        CLASSIFY_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_transform_time(d: f64) {
    if is_timing_enabled() {
        TRANSFORM_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_lz77_time(d: f64) {
    if is_timing_enabled() {
        LZ77_TIME.with(|t| *t.borrow_mut() += d);
    }
}

#[allow(dead_code)]
fn add_ppm_predict_time(d: f64) {
    if is_timing_enabled() {
        PPM_PREDICT_TIME.with(|t| *t.borrow_mut() += d);
    }
}

#[allow(dead_code)]
fn add_ppm_update_time(d: f64) {
    if is_timing_enabled() {
        PPM_UPDATE_TIME.with(|t| *t.borrow_mut() += d);
    }
}

#[allow(dead_code)]
fn add_ppm_hash_time(d: f64) {
    if is_timing_enabled() {
        PPM_HASH_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_rans_time(d: f64) {
    if is_timing_enabled() {
        RANS_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_freq_table_time(d: f64) {
    if is_timing_enabled() {
        FREQ_TABLE_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_checksum_time(d: f64) {
    if is_timing_enabled() {
        CHECKSUM_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn add_write_time(d: f64) {
    if is_timing_enabled() {
        WRITE_TIME.with(|t| *t.borrow_mut() += d);
    }
}

fn reset_timings() {
    let enabled = std::env::var("FLUX_TIMING").is_ok();
    TIMING_ENABLED.with(|e| *e.borrow_mut() = enabled);
    SCAN_TIME.with(|t| *t.borrow_mut() = 0.0);
    CLASSIFY_TIME.with(|t| *t.borrow_mut() = 0.0);
    TRANSFORM_TIME.with(|t| *t.borrow_mut() = 0.0);
    LZ77_TIME.with(|t| *t.borrow_mut() = 0.0);
    PPM_PREDICT_TIME.with(|t| *t.borrow_mut() = 0.0);
    PPM_UPDATE_TIME.with(|t| *t.borrow_mut() = 0.0);
    PPM_HASH_TIME.with(|t| *t.borrow_mut() = 0.0);
    RANS_TIME.with(|t| *t.borrow_mut() = 0.0);
    FREQ_TABLE_TIME.with(|t| *t.borrow_mut() = 0.0);
    CHECKSUM_TIME.with(|t| *t.borrow_mut() = 0.0);
    WRITE_TIME.with(|t| *t.borrow_mut() = 0.0);
}

fn print_timings_report(total_time: f64) {
    if !is_timing_enabled() {
        return;
    }

    let scan = SCAN_TIME.with(|t| *t.borrow());
    let classify = CLASSIFY_TIME.with(|t| *t.borrow());
    let transform = TRANSFORM_TIME.with(|t| *t.borrow());
    let lz77 = LZ77_TIME.with(|t| *t.borrow());
    let ppm_pred = PPM_PREDICT_TIME.with(|t| *t.borrow());
    let ppm_up = PPM_UPDATE_TIME.with(|t| *t.borrow());
    let ppm_hash = PPM_HASH_TIME.with(|t| *t.borrow());
    let rans = RANS_TIME.with(|t| *t.borrow());
    let freq = FREQ_TABLE_TIME.with(|t| *t.borrow());
    let checksum = CHECKSUM_TIME.with(|t| *t.borrow());
    let write = WRITE_TIME.with(|t| *t.borrow());

    println!("   === FLUX TIMING BREAKDOWN ===");
    println!("   Directory scan:        {:7.2}s", scan);
    println!("   Classification:        {:7.2}s", classify);
    println!("   Transform:             {:7.2}s", transform);
    println!("   LZ77 encode:           {:7.2}s", lz77);
    println!("   PPM predict:           {:7.2}s", ppm_pred);
    println!("   PPM update:            {:7.2}s", ppm_up);
    println!("   PPM context hashing:   {:7.2}s", ppm_hash);
    println!("   rANS encode:           {:7.2}s", rans);
    println!("   Freq table build:      {:7.2}s", freq);
    println!("   Checksums:             {:7.2}s", checksum);
    println!("   Archive write:         {:7.2}s", write);
    println!("   TOTAL:                 {:7.2}s", total_time);
    println!("   =============================");
}

/// Statistics returned on successful compression.
pub type ProgressCallback = Option<Arc<dyn Fn(u64, u64, String) + Send + Sync>>;

#[derive(Debug, Clone)]
pub struct CompressionStats {
    pub original_size: u64,
    pub compressed_size: u64,
    pub compression_ratio: f32,
    pub files_processed: u32,
    pub elapsed_ms: u64,
    pub blocks_created: u32,
}

/// Statistics returned on successful decompression.
#[derive(Debug, Clone)]
pub struct DecompressionStats {
    pub files_extracted: u32,
    pub bytes_written: u64,
    pub elapsed_ms: u64,
    pub integrity_verified: bool,
}

/// High-level API for FLUX compression pipeline.
pub struct FluxCompressor {
    pub options: FluxOptions,
    pub buffer: Arc<CircularBuffer>,
    pub integrity: MultiLevelIntegrity,
    pub progress_callback: ProgressCallback,
}

impl FluxCompressor {
    /// Initializes a new `FluxCompressor`.
    pub fn new(options: FluxOptions) -> Self {
        Self {
            options,
            buffer: Arc::new(CircularBuffer::new(1024 * 1024)), // Minimal 1MB placeholder capacity
            integrity: MultiLevelIntegrity::new(),
            progress_callback: None,
        }
    }

    /// Compresses a single file into a FLUX archive.
    pub fn compress_file(
        &mut self,
        input: &Path,
        output: &Path,
    ) -> Result<CompressionStats, ArchiveError> {
        reset_timings();
        let total_start = std::time::Instant::now();
        flux_debug!("[FLUX] Starting compression of {:?}", input);
        let root_dir = input.parent().unwrap_or(Path::new(""));
        let relative = input.strip_prefix(root_dir).unwrap_or(input);

        // Scan lone file
        let scan_start = std::time::Instant::now();
        let mut tree = DirectoryTree {
            root: root_dir.to_path_buf(),
            entries: Vec::new(),
        };
        let metadata = fs::metadata(input).map_err(|e| ArchiveError::Io(e.to_string()))?;
        flux_debug!("[FLUX] Input size: {} bytes", metadata.len());

        let modified_time = metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let created_time = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(modified_time);

        #[cfg(unix)]
        let permissions = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode()
        };
        #[cfg(not(unix))]
        let permissions = if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };

        tree.entries.push(archive::DirectoryEntry {
            path: relative.to_path_buf(),
            entry_type: archive::EntryType::RegularFile {
                size: metadata.len(),
            },
            metadata: archive::EntryMetadata {
                permissions,
                modified_time,
                created_time,
                owner_uid: 0,
                owner_gid: 0,
            },
        });
        add_scan_time(scan_start.elapsed().as_secs_f64());

        let res = self.compress_tree(tree, output);
        let total_elapsed = total_start.elapsed().as_secs_f64();
        print_timings_report(total_elapsed);
        res
    }

    /// Compresses an entire directory tree recursively into a FLUX archive.
    pub fn compress_directory(
        &mut self,
        input: &Path,
        output: &Path,
    ) -> Result<CompressionStats, ArchiveError> {
        reset_timings();
        let total_start = std::time::Instant::now();
        flux_debug!("[FLUX] Starting compression of directory {:?}", input);
        flux_debug!("[FLUX] Scanning directory...");
        let scan_start = std::time::Instant::now();
        let tree = DirectoryTree::scan(input)?;
        add_scan_time(scan_start.elapsed().as_secs_f64());
        flux_debug!("[FLUX] Found {} files", tree.entries.len());
        let res = self.compress_tree(tree, output);
        let total_elapsed = total_start.elapsed().as_secs_f64();
        print_timings_report(total_elapsed);
        res
    }

    fn detect_chunk_stride(data: &[u8]) -> Option<u8> {
        let sample_len = data.len().min(4096);
        let sample = &data[..sample_len];
        if sample.len() < 64 {
            return None;
        }
        let mut best_stride = None;
        let mut highest_corr = 0.0f32;
        for stride in 2..=16 {
            let corr = crate::analysis::stride::compute_pearson_autocorrelation(sample, stride);
            if corr >= 0.50 && corr > highest_corr {
                highest_corr = corr;
                best_stride = Some(stride);
            }
        }
        best_stride
    }

    fn compress_tree(
        &mut self,
        tree: DirectoryTree,
        output: &Path,
    ) -> Result<CompressionStats, ArchiveError> {
        let mut file_diagnostics = StreamDiagnostics::default();
        let start_time = std::time::Instant::now();

        // 1. Prepare key derivation and password sentinel
        let password_str = if self.options.password.is_null() {
            None
        } else {
            unsafe {
                std::ffi::CStr::from_ptr(self.options.password)
                    .to_str()
                    .ok()
            }
        };
        let is_encrypted = password_str.is_some();
        let (key, salt, sentinel) = if let Some(pass) = password_str {
            let s = crate::crypto::keys::KeyDerivation::generate_salt();
            let k = crate::crypto::keys::KeyDerivation::derive_key(pass, &s)
                .map_err(|_| ArchiveError::WrongPassword)?;
            let sent = crate::crypto::sentinel::create_sentinel(&k)
                .map_err(|_| ArchiveError::DecryptionFailed)?;
            (Some(k), s, sent)
        } else {
            (
                None,
                crate::crypto::Salt([0u8; 16]),
                crate::crypto::stream::EncryptedChunk {
                    iv: crate::crypto::Iv([0u8; 12]),
                    ciphertext: Vec::new(),
                    auth_tag: crate::crypto::AuthTag([0u8; 16]),
                    chunk_index: 0,
                },
            )
        };

        // 2. Classify regular files & prepare file entries with uncompressed checksums
        flux_debug!("[FLUX] Classifying files...");
        let mut files_data = Vec::new();
        let mut classifications = std::collections::HashMap::new();
        let mut file_idx = 0u64;

        let root_resolved = tree.root.clone();

        let mut total_bytes = 0u64;
        for entry in &tree.entries {
            if let archive::EntryType::RegularFile { size } = entry.entry_type {
                total_bytes += size;
            }
        }

        // Cap buffer capacity to the smaller of:
        // - 10% of system RAM (detect_buffer_capacity())
        // - total_bytes + 1MB headroom
        // Capping avoids wasteful allocation on small and medium files.
        let ram_cap = detect_buffer_capacity();
        let target_cap = ram_cap.min((total_bytes as usize).saturating_add(1_048_576));

        // RAM Safety check (warn-and-proceed)
        let window_size = crate::ffi::window_size_for_level(self.options.level);
        let hash_bits = crate::compress::lz77::hash_bits_for_window(window_size);
        let hash_table_size = (1 << hash_bits) * 4;
        let hash_chains_size = window_size * 4;
        let est_peak =
            window_size + target_cap + hash_table_size + hash_chains_size + 16 * 1024 * 1024;

        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let available_ram = sys.available_memory();
        if est_peak as u64 > available_ram {
            let level_name = match self.options.level {
                FluxCompressionLevel::Tiny => "Tiny",
                FluxCompressionLevel::Fast => "Fast",
                FluxCompressionLevel::Balanced => "Balanced",
                FluxCompressionLevel::Maximum => "Maximum",
                FluxCompressionLevel::Extreme => "Extreme",
            };
            let req_mb = est_peak.div_ceil(1024 * 1024);
            let avail_mb = available_ram / (1024 * 1024);
            eprintln!(
                "Warning: {} mode needs ~{} MB but only {} MB is available. Compression may be slow or stress your system. Consider a lower level (e.g. Balanced). Proceeding anyway.",
                level_name, req_mb, avail_mb
            );
        }

        self.buffer = Arc::new(CircularBuffer::new(target_cap));
        flux_debug!("[FLUX] Buffer capacity: {} bytes", self.buffer.capacity);

        let mut bytes_processed = 0u64;
        for entry in &tree.entries {
            if let archive::EntryType::RegularFile { size } = entry.entry_type {
                let file_path = root_resolved.join(&entry.path);

                if let Some(ref cb) = self.progress_callback {
                    cb(
                        bytes_processed,
                        total_bytes,
                        entry.path.to_string_lossy().into_owned(),
                    );
                }

                let data =
                    std::fs::read(&file_path).map_err(|e| ArchiveError::Io(e.to_string()))?;

                // Run content classifier
                let classify_start = std::time::Instant::now();
                let mut classifier = crate::analysis::ContentClassifier::new();
                let classification = classifier.classify(&data);
                add_classify_time(classify_start.elapsed().as_secs_f64());
                classifications.insert(file_idx, classification);

                // Compute file level checksums
                let checksum_start = std::time::Instant::now();
                let checksums = ChecksumSet::compute(&data);
                add_checksum_time(checksum_start.elapsed().as_secs_f64());
                self.integrity.file_checksums.insert(file_idx, checksums);

                let file_entry = archive::format::FileEntry {
                    path: entry.path.to_string_lossy().to_string(),
                    original_size: size,
                    permissions: entry.metadata.permissions,
                    modified_time: entry.metadata.modified_time,
                    created_time: entry.metadata.created_time,
                    accessed_time: entry.metadata.modified_time,
                    solid_block_id: 0,
                    block_offset: 0,
                    original_sha256: checksums.sha256,
                    original_crc32: checksums.crc32,
                };
                files_data.push((file_entry, data));
                file_idx += 1;
                bytes_processed += size;
            }
        }

        // 3. Group files into optimal Solid Blocks
        flux_debug!("[FLUX] Building solid blocks...");
        let grouper = SolidBlockGrouper::new();
        let mut builders = grouper.group_files(files_data, &classifications);
        for builder in &mut builders {
            builder
                .entries_with_data
                .sort_by_key(|(entry, _)| entry.original_size);
        }

        // 4. Setup Parallel CRC32 integrity thread
        let total_blocks = builders.len();
        flux_debug!("[FLUX] Total blocks to process: {}", total_blocks);
        let (signal_tx, signal_rx) = unbounded();
        let parallel_crc = ParallelCrc32::new(self.buffer.clone());

        // Build global list of (file_index, file_size) for the background worker
        let mut parallel_files = Vec::new();
        let mut current_idx = 0u64;
        for builder in &builders {
            for (entry, _) in &builder.entries_with_data {
                parallel_files.push((current_idx, entry.original_size));
                current_idx += 1;
            }
        }
        parallel_crc.set_files(parallel_files);
        let crc_join_handle = parallel_crc.run(signal_rx);

        if self.options.volume_size > 0 {
            return self.compress_tree_split(
                tree,
                output,
                builders,
                is_encrypted,
                key,
                salt,
                sentinel,
                total_bytes,
                signal_tx,
                crc_join_handle,
                file_diagnostics,
                start_time,
            );
        }

        // 5. Open Output File & Write Header Placeholder
        flux_debug!("[FLUX] Writing archive header...");
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(output)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        // Generate random IV for encrypted header
        let mut header_iv_bytes = [0u8; 12];
        if is_encrypted {
            rand::rngs::OsRng.fill_bytes(&mut header_iv_bytes);
        }
        let encrypted_header_iv = crate::crypto::Iv(header_iv_bytes);

        let level = self.options.level;
        let window_size = crate::ffi::window_size_for_level(level) as u32;
        let block_size = window_size;

        let plaintext_header = crate::crypto::header::PlaintextHeader {
            magic: *b"FLUX",
            version_major: 1,
            version_minor: 0,
            compression_level: level as u8,
            window_size,
            block_size,
            flags: if is_encrypted { 1 } else { 0 },
            argon2_memory_kb: 65536,
            argon2_iterations: 3,
            argon2_parallelism: 4,
            salt,
            sentinel_chunk: sentinel,
            encrypted_header_iv,
            is_encrypted,
        };

        let p_serialized = plaintext_header.serialize();
        let write_start = std::time::Instant::now();
        file.write_all(&p_serialized)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        // Encrypted header placeholder (98 bytes if encrypted, 70 bytes if raw)
        let header_data_len = if is_encrypted { 98 } else { 70 };
        file.write_all(&vec![0u8; header_data_len])
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        // 6. Compress and Write Solid Blocks
        let mut solid_blocks_meta = Vec::new();
        let mut final_file_entries = Vec::new();
        let mut current_block_id = 0u32;
        let mut compressed_total_size = 0u64;
        let mut original_total_size = 0u64;

        // Front File Index position
        let front_index_offset = file
            .stream_position()
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        // We will write the Front Index placeholder and seek past it
        // To build the exact index size, we can serialize a draft first
        let mut draft_index = FileIndex::new();
        for builder in &builders {
            for (entry, _) in &builder.entries_with_data {
                draft_index.add_entry(entry.clone());
            }
        }
        let draft_tree_bytes = serialize_directory_tree(&tree);
        let mut draft_metadata_payload = draft_index.serialize();
        draft_metadata_payload.extend_from_slice(&draft_tree_bytes);

        let draft_metadata_encrypted = encrypt_payload(&draft_metadata_payload, &key)?;
        let front_index_size = draft_metadata_encrypted.len() as u64;

        let write_start = std::time::Instant::now();
        file.write_all(&vec![0u8; front_index_size as usize])
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        // Process solid blocks
        for builder in builders {
            let block_type = builder.block_type;
            let (mut block_meta, uncompressed_data, block_files) = builder.build_with_data();
            block_meta.block_id = current_block_id;
            let raw_size = uncompressed_data.len();
            flux_debug!(
                "[FLUX] Processing solid block {}/{} (type={:?}, raw_size={} bytes)...",
                current_block_id + 1,
                total_blocks,
                block_type,
                raw_size
            );

            if let Some(ref cb) = self.progress_callback {
                let current_file = if let Some(first_file) = block_files.first() {
                    first_file.path.clone()
                } else {
                    format!("Solid Block {}", current_block_id)
                };
                cb(original_total_size, total_bytes, current_file);
            }

            // Stream raw data to the circular buffer for background integrity checking
            self.buffer.write(&uncompressed_data);
            let _ = signal_tx.send(BufferSignal::DataAvailable(uncompressed_data.len()));

            let block_size = crate::ffi::window_size_for_level(self.options.level);
            // Split raw data into block_size chunks and compress each independently
            let mut chunk_index = 0;
            let mut compressed_block_data = Vec::new();

            while chunk_index < uncompressed_data.len() {
                let chunk_end = std::cmp::min(chunk_index + block_size, uncompressed_data.len());
                let chunk_data = uncompressed_data[chunk_index..chunk_end].to_vec();
                let chunk_uncompressed_size = chunk_data.len() as u32;

                // Determine transforms to apply
                let level = self.options.level;

                let (compressed_chunk_bytes, final_block_type, mut transform_stack, diag) =
                    if (level == FluxCompressionLevel::Maximum
                        || level == FluxCompressionLevel::Extreme)
                        && block_type == archive::format::BlockType::Text
                    {
                        // Try BWT vs No BWT adaptively
                        // 1. Try No BWT
                        let transform_start = std::time::Instant::now();
                        let mut ts_no_bwt = TransformStack::default();
                        let transformed_no_bwt = ts_no_bwt.apply(
                            chunk_data.clone(),
                            3.0,
                            &ContentType::Text,
                            None,
                            false, // bwt_enabled = false
                        );
                        add_transform_time(transform_start.elapsed().as_secs_f64());

                        let (compressed_no_bwt, bt_no_bwt, diag_no_bwt) = compress_block(
                            &transformed_no_bwt,
                            block_type,
                            level,
                            &ts_no_bwt.media_filter_type,
                        );

                        // 2. Try With BWT
                        let transform_start_bwt = std::time::Instant::now();
                        let mut ts_with_bwt = TransformStack::default();
                        let transformed_with_bwt = ts_with_bwt.apply(
                            chunk_data.clone(),
                            3.0,
                            &ContentType::Text,
                            None,
                            true, // bwt_enabled = true
                        );
                        add_transform_time(transform_start_bwt.elapsed().as_secs_f64());

                        let (compressed_with_bwt, bt_with_bwt, diag_with_bwt) = compress_block(
                            &transformed_with_bwt,
                            block_type,
                            level,
                            &ts_with_bwt.media_filter_type,
                        );

                        // Keep BWT only if it is strictly smaller
                        if compressed_with_bwt.len() < compressed_no_bwt.len() {
                            (compressed_with_bwt, bt_with_bwt, ts_with_bwt, diag_with_bwt)
                        } else {
                            (compressed_no_bwt, bt_no_bwt, ts_no_bwt, diag_no_bwt)
                        }
                    } else {
                        // Fast or Balanced mode, or non-text block
                        let bwt_enabled = false;
                        let transform_start = std::time::Instant::now();
                        let stride = if std::env::var("FLUX_DISABLE_TRANSPOSE").is_ok() {
                            None
                        } else {
                            match block_type {
                                archive::format::BlockType::Multimedia => {
                                    let s = Self::detect_chunk_stride(&chunk_data);
                                    if let Some(stride) = s {
                                        if std::env::var("FLUX_TIMING").is_ok() {
                                            println!("[FLUX DEBUG] Detected stride {} for Multimedia chunk of size {}", stride, chunk_data.len());
                                        }
                                    }
                                    s
                                }
                                _ => None,
                            }
                        };

                        let (compressed, final_block_type, ts, diag) = if let Some(s) = stride {
                            if s % 4 == 0 && block_type == archive::format::BlockType::Multimedia {
                                // Try generic transpose
                                let mut ts_generic = TransformStack::default();
                                let transformed_generic = ts_generic.apply(
                                    chunk_data.clone(),
                                    3.0,
                                    &ContentType::Multimedia,
                                    stride,
                                    false,
                                );
                                let (comp_generic, bt_generic, diag_generic) = compress_block(
                                    &transformed_generic,
                                    block_type,
                                    level,
                                    &ts_generic.media_filter_type,
                                );

                                // Try float channel split filter
                                let mut ts_float = TransformStack {
                                    media_filter_applied: true,
                                    media_filter_type:
                                        transform::MediaFilterType::FloatChannelSplit {
                                            channels: s / 4,
                                            mask: 0,
                                        },
                                    ..Default::default()
                                };
                                let transformed_float = ts_float.apply(
                                    chunk_data.clone(),
                                    3.0,
                                    &ContentType::Multimedia,
                                    None,
                                    false,
                                );
                                let (comp_float, bt_float, diag_float) = compress_block(
                                    &transformed_float,
                                    block_type,
                                    level,
                                    &ts_float.media_filter_type,
                                );

                                // Keep the smaller one
                                if comp_float.len() < comp_generic.len() {
                                    if std::env::var("FLUX_TIMING").is_ok() {
                                        println!("[FLUX DEBUG] FloatChannelSplitFilter chosen ({} bytes) over generic transpose ({} bytes)", comp_float.len(), comp_generic.len());
                                    }
                                    (comp_float, bt_float, ts_float, diag_float)
                                } else {
                                    if std::env::var("FLUX_TIMING").is_ok() {
                                        println!("[FLUX DEBUG] Generic transpose chosen ({} bytes) over FloatChannelSplitFilter ({} bytes)", comp_generic.len(), comp_float.len());
                                    }
                                    (comp_generic, bt_generic, ts_generic, diag_generic)
                                }
                            } else {
                                let mut ts = TransformStack::default();
                                let transformed_chunk_data = ts.apply(
                                    chunk_data,
                                    3.0,
                                    &ContentType::Multimedia,
                                    stride,
                                    bwt_enabled,
                                );
                                let (compressed, bt, diag) = compress_block(
                                    &transformed_chunk_data,
                                    block_type,
                                    level,
                                    &ts.media_filter_type,
                                );
                                (compressed, bt, ts, diag)
                            }
                        } else {
                            let mut ts = TransformStack::default();
                            let transformed_chunk_data = ts.apply(
                                chunk_data,
                                3.0,
                                &match block_type {
                                    archive::format::BlockType::Text => ContentType::Text,
                                    archive::format::BlockType::Multimedia => {
                                        ContentType::Multimedia
                                    }
                                    _ => ContentType::Binary,
                                },
                                stride,
                                bwt_enabled,
                            );
                            let (compressed, bt, diag) = compress_block(
                                &transformed_chunk_data,
                                block_type,
                                level,
                                &ts.media_filter_type,
                            );
                            (compressed, bt, ts, diag)
                        };

                        add_transform_time(transform_start.elapsed().as_secs_f64());
                        (compressed, final_block_type, ts, diag)
                    };

                // PPM (Order-4 context model) is implemented and available but
                // DISABLED by default. Measurement showed it gains only ~1% on
                // prose (literals are ~7% of output in our LZ77-first pipeline)
                // and can slightly hurt mixed data via escape overhead. The code
                // is retained for future research / re-enablement. To re-enable,
                // set ppm_applied = true for the desired levels here.
                transform_stack.ppm_applied = false;
                transform_stack.ppm_arena_size = 0;

                if is_analyze_enabled() {
                    file_diagnostics.merge(&diag);
                }

                block_meta.block_type = final_block_type;
                let chunk_compressed_size = compressed_chunk_bytes.len() as u32;

                compressed_block_data.extend_from_slice(&chunk_uncompressed_size.to_le_bytes());
                compressed_block_data.extend_from_slice(&chunk_compressed_size.to_le_bytes());
                compressed_block_data.extend_from_slice(&transform_stack.serialize());
                compressed_block_data.extend_from_slice(&compressed_chunk_bytes);

                chunk_index = chunk_end;
            }

            // Encrypt block payload if key is present, otherwise reference the original data
            let has_key = key.is_some();
            let payload_to_write = if has_key {
                encrypt_payload(&compressed_block_data, &key)?
            } else {
                Vec::new()
            };

            let payload_ref = if has_key {
                &payload_to_write
            } else {
                &compressed_block_data
            };

            block_meta.compressed_size = payload_ref.len() as u64;
            let checksum_start = std::time::Instant::now();
            block_meta.compressed_hash = crate::integrity::ChecksumSet::compute(payload_ref).sha256;
            add_checksum_time(checksum_start.elapsed().as_secs_f64());

            // Record block checksum
            self.integrity
                .record_block(block_meta.block_id, payload_ref);

            // Serialize Solid Block Header (66 bytes)
            let mut block_header = Vec::new();
            block_header.extend_from_slice(&block_meta.block_id.to_le_bytes());
            block_header.push(match block_meta.block_type {
                archive::format::BlockType::Text => 0,
                archive::format::BlockType::Binary => 1,
                archive::format::BlockType::Multimedia => 2,
                archive::format::BlockType::Mixed => 3,
                archive::format::BlockType::Raw => 4,
            });
            block_header.extend_from_slice(&block_meta.compressed_size.to_le_bytes());
            block_header.extend_from_slice(&block_meta.uncompressed_size.to_le_bytes());
            block_header.extend_from_slice(&block_meta.compressed_hash);
            block_header.extend_from_slice(&TransformStack::default().serialize());

            // Write solid block header + payload to disk
            let write_start = std::time::Instant::now();
            file.write_all(&block_header)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file.write_all(payload_ref)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            add_write_time(write_start.elapsed().as_secs_f64());

            compressed_total_size += block_meta.compressed_size;
            original_total_size += block_meta.uncompressed_size;

            // Associate final file positions
            for mut entry in block_files {
                entry.solid_block_id = current_block_id;
                final_file_entries.push(entry);
            }

            solid_blocks_meta.push(block_meta);
            current_block_id += 1;
        }

        // Flush Circular Buffer EOF signal to background integrity worker
        let _ = signal_tx.send(BufferSignal::EndOfFile);
        let background_crc_map = crc_join_handle.join().unwrap();

        // Update uncompressed files in index with their final background CRCs
        flux_debug!("[FLUX] Writing file index...");
        let mut final_index = FileIndex::new();
        final_index.total_compressed_size = compressed_total_size;
        for (i, mut entry) in final_file_entries.into_iter().enumerate() {
            if let Some(&crc) = background_crc_map.get(&(i as u64)) {
                entry.original_crc32 = crc;
            }
            final_index.add_entry(entry);
        }

        // Serialize metadata payload (Index + Tree)
        let final_tree_bytes = serialize_directory_tree(&tree);
        let mut final_metadata_payload = final_index.serialize();
        final_metadata_payload.extend_from_slice(&final_tree_bytes);
        let final_metadata_encrypted = encrypt_payload(&final_metadata_payload, &key)?;

        let write_start = std::time::Instant::now();
        // Seek back to write the real Front Index
        file.seek(SeekFrom::Start(front_index_offset))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        file.write_all(&final_metadata_encrypted)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        // Seek to the end of the file to append the Back File Index
        file.seek(SeekFrom::End(0))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        file.write_all(&final_metadata_encrypted)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        // Compute correct physical body checksum by re-reading the written body sequentially
        let checksum_start = std::time::Instant::now();
        let body_end_offset = file
            .stream_position()
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        file.seek(SeekFrom::Start(front_index_offset))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        let mut final_body_sha = Sha256Hasher::new();
        let mut final_body_crc = Crc32Hasher::new();
        let mut remaining = (body_end_offset - front_index_offset) as usize;
        let mut read_buf = vec![0u8; 65536];
        while remaining > 0 {
            let to_read = std::cmp::min(remaining, read_buf.len());
            file.read_exact(&mut read_buf[..to_read])
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            final_body_sha.update(&read_buf[..to_read]);
            final_body_crc.update(&read_buf[..to_read]);
            remaining -= to_read;
        }

        let body_sha_digest = final_body_sha.finalize();
        let body_crc_digest = final_body_crc.finalize();
        let archive_checksum = ChecksumSet {
            crc32: body_crc_digest,
            sha256: body_sha_digest,
        };
        add_checksum_time(checksum_start.elapsed().as_secs_f64());

        let write_start = std::time::Instant::now();
        // Seek back to the end to write the trailing Archive Checksum
        file.seek(SeekFrom::Start(body_end_offset))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        // Write the trailing Archive Checksum (36 bytes: 4 bytes CRC32 + 32 bytes SHA-256)
        file.write_all(&archive_checksum.crc32.to_le_bytes())
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        file.write_all(&archive_checksum.sha256)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        // Seek to start of file to rewrite the Encrypted Header Data
        let encrypted_header_data = crate::crypto::header::EncryptedHeaderData {
            file_index_offset: front_index_offset,
            file_index_size: front_index_size,
            solid_block_count: current_block_id,
            compression_level: self.options.level as u8,
            transform_flags: 0,
            original_total_size,
            compressed_total_size,
            archive_sha256: archive_checksum.sha256,
        };

        let header_payload = if is_encrypted {
            crate::crypto::header::encrypt_header_data(&encrypted_header_data, &key.unwrap())
                .map_err(|_| ArchiveError::DecryptionFailed)?
        } else {
            encrypted_header_data.serialize()
        };

        let write_start = std::time::Instant::now();
        file.seek(SeekFrom::Start(p_serialized.len() as u64))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        file.write_all(&header_payload)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        let elapsed = start_time.elapsed().as_millis() as u64;
        let final_size = file.metadata().map(|m| m.len()).unwrap_or(0);
        flux_debug!("[FLUX] Done. Total output: {} bytes", final_size);

        if is_analyze_enabled() {
            print_diagnostics_report(&file_diagnostics);
        }

        Ok(CompressionStats {
            original_size: original_total_size,
            compressed_size: final_size,
            compression_ratio: if original_total_size > 0 {
                final_size as f32 / original_total_size as f32
            } else {
                1.0
            },
            files_processed: final_index.total_files() as u32,
            elapsed_ms: elapsed,
            blocks_created: current_block_id,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn compress_tree_split(
        &mut self,
        tree: crate::archive::DirectoryTree,
        output: &Path,
        builders: Vec<crate::archive::solid::SolidBlockBuilder>,
        is_encrypted: bool,
        key: Option<crate::crypto::EncryptionKey>,
        salt: crate::crypto::Salt,
        sentinel: crate::crypto::stream::EncryptedChunk,
        total_bytes: u64,
        signal_tx: crossbeam::channel::Sender<BufferSignal>,
        crc_join_handle: std::thread::JoinHandle<std::collections::HashMap<u64, u32>>,
        mut file_diagnostics: StreamDiagnostics,
        start_time: std::time::Instant,
    ) -> Result<CompressionStats, ArchiveError> {
        let total_blocks = builders.len();

        // Scan for existing sibling volumes to prevent concurrency/run collisions
        let mut existing_vols = Vec::new();
        for vol_num in 1..=999 {
            let path = get_volume_path(output, vol_num);
            if path.exists() {
                existing_vols.push(path.display().to_string());
            }
        }
        if !existing_vols.is_empty() {
            return Err(ArchiveError::Io(format!(
                "Cannot start compression: the following sibling volume files already exist: {}. Please delete them or use --force to overwrite.",
                existing_vols.join(", ")
            )));
        }

        // Validate volume_size >= nominal block_size
        let nominal_block_size = if self.options.block_size > 0 {
            self.options.block_size
        } else {
            crate::ffi::window_size_for_level(self.options.level) as u32
        };
        if self.options.volume_size > 0 && self.options.volume_size < nominal_block_size as u64 {
            let level_name = match self.options.level {
                crate::ffi::FluxCompressionLevel::Tiny => "Tiny",
                crate::ffi::FluxCompressionLevel::Fast => "Fast",
                crate::ffi::FluxCompressionLevel::Balanced => "Balanced",
                crate::ffi::FluxCompressionLevel::Maximum => "Maximum",
                crate::ffi::FluxCompressionLevel::Extreme => "Extreme",
            };
            let block_size_mb = nominal_block_size as f64 / 1_048_576.0;
            return Err(ArchiveError::Io(format!(
                "Level {} uses {:.2} MB blocks; volume_size must be at least {:.2} MB. Use a lower compression level or increase volume_size.",
                level_name, block_size_mb, block_size_mb
            )));
        }

        // Generate random archive ID (16 bytes)
        let mut archive_id = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut archive_id);

        let mut writer =
            MultiVolumeWriter::new(output.to_path_buf(), self.options.volume_size, archive_id);

        // Generate random IV for encrypted header
        let mut header_iv_bytes = [0u8; 12];
        if is_encrypted {
            rand::rngs::OsRng.fill_bytes(&mut header_iv_bytes);
        }
        let encrypted_header_iv = crate::crypto::Iv(header_iv_bytes);

        let level = self.options.level;
        let window_size = crate::ffi::window_size_for_level(level) as u32;
        let block_size = window_size;

        let plaintext_header = crate::crypto::header::PlaintextHeader {
            magic: *b"FLUX",
            version_major: 1,
            version_minor: 3,
            compression_level: level as u8,
            window_size,
            block_size,
            flags: if is_encrypted { 1 } else { 0 },
            argon2_memory_kb: 65536,
            argon2_iterations: 3,
            argon2_parallelism: 4,
            salt,
            sentinel_chunk: sentinel,
            encrypted_header_iv,
            is_encrypted,
        };

        let p_serialized = plaintext_header.serialize();
        let header_data_len = if is_encrypted { 98 } else { 70 };

        // Determine Front Index size using draft
        let mut draft_index = FileIndex::new();
        for builder in &builders {
            for (entry, _) in &builder.entries_with_data {
                draft_index.add_entry(entry.clone());
            }
        }
        let draft_tree_bytes = serialize_directory_tree(&tree);
        let mut draft_metadata_payload = draft_index.serialize();
        draft_metadata_payload.extend_from_slice(&draft_tree_bytes);

        let draft_metadata_encrypted = encrypt_payload(&draft_metadata_payload, &key)?;
        let front_index_size = draft_metadata_encrypted.len() as u64;

        let initial_metadata_size =
            p_serialized.len() as u64 + header_data_len as u64 + front_index_size;

        // Start block 0 to setup volume 1 and write initial metadata block
        writer
            .start_block(initial_metadata_size)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        let write_start = std::time::Instant::now();
        writer
            .write_payload(&p_serialized)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        writer
            .write_payload(&vec![0u8; header_data_len])
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        let front_index_offset = 50 + p_serialized.len() as u64 + header_data_len as u64;
        writer
            .write_payload(&vec![0u8; front_index_size as usize])
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        add_write_time(write_start.elapsed().as_secs_f64());

        // Process solid blocks
        let mut solid_blocks_meta = Vec::new();
        let mut final_file_entries = Vec::new();
        let mut current_block_id = 0u32;
        let mut compressed_total_size = 0u64;
        let mut original_total_size = 0u64;

        let mut volume_index = crate::crypto::header::VolumeIndex::default();

        for builder in builders {
            let block_type = builder.block_type;
            let (mut block_meta, uncompressed_data, block_files) = builder.build_with_data();
            block_meta.block_id = current_block_id;
            let raw_size = uncompressed_data.len();
            flux_debug!(
                "[FLUX] Processing solid block {}/{} (type={:?}, raw_size={} bytes)...",
                current_block_id + 1,
                total_blocks,
                block_type,
                raw_size
            );

            if let Some(ref cb) = self.progress_callback {
                let current_file = if let Some(first_file) = block_files.first() {
                    first_file.path.clone()
                } else {
                    format!("Solid Block {}", current_block_id)
                };
                cb(original_total_size, total_bytes, current_file);
            }

            self.buffer.write(&uncompressed_data);
            let _ = signal_tx.send(BufferSignal::DataAvailable(uncompressed_data.len()));

            let block_size = crate::ffi::window_size_for_level(self.options.level);
            let mut chunk_index = 0;
            let mut compressed_block_data = Vec::new();

            while chunk_index < uncompressed_data.len() {
                let chunk_end = std::cmp::min(chunk_index + block_size, uncompressed_data.len());
                let chunk_data = uncompressed_data[chunk_index..chunk_end].to_vec();
                let chunk_uncompressed_size = chunk_data.len() as u32;

                let level = self.options.level;
                let (compressed_chunk_bytes, final_block_type, mut transform_stack, diag) =
                    if (level == FluxCompressionLevel::Maximum
                        || level == FluxCompressionLevel::Extreme)
                        && block_type == archive::format::BlockType::Text
                    {
                        let transform_start = std::time::Instant::now();
                        let mut ts_no_bwt = TransformStack::default();
                        let transformed_no_bwt = ts_no_bwt.apply(
                            chunk_data.clone(),
                            3.0,
                            &ContentType::Text,
                            None,
                            false,
                        );
                        add_transform_time(transform_start.elapsed().as_secs_f64());
                        let (compressed_no_bwt, bt_no_bwt, diag_no_bwt) = compress_block(
                            &transformed_no_bwt,
                            block_type,
                            level,
                            &ts_no_bwt.media_filter_type,
                        );

                        let transform_start_bwt = std::time::Instant::now();
                        let mut ts_with_bwt = TransformStack::default();
                        let transformed_with_bwt = ts_with_bwt.apply(
                            chunk_data.clone(),
                            3.0,
                            &ContentType::Text,
                            None,
                            true,
                        );
                        add_transform_time(transform_start_bwt.elapsed().as_secs_f64());
                        let (compressed_with_bwt, bt_with_bwt, diag_with_bwt) = compress_block(
                            &transformed_with_bwt,
                            block_type,
                            level,
                            &ts_with_bwt.media_filter_type,
                        );

                        if compressed_with_bwt.len() < compressed_no_bwt.len() {
                            (compressed_with_bwt, bt_with_bwt, ts_with_bwt, diag_with_bwt)
                        } else {
                            (compressed_no_bwt, bt_no_bwt, ts_no_bwt, diag_no_bwt)
                        }
                    } else {
                        let bwt_enabled = false;
                        let transform_start = std::time::Instant::now();
                        let stride = if std::env::var("FLUX_DISABLE_TRANSPOSE").is_ok() {
                            None
                        } else {
                            match block_type {
                                archive::format::BlockType::Multimedia => {
                                    Self::detect_chunk_stride(&chunk_data)
                                }
                                _ => None,
                            }
                        };

                        let (compressed, final_block_type, ts, diag) = if let Some(s) = stride {
                            if s % 4 == 0 && block_type == archive::format::BlockType::Multimedia {
                                let mut ts_generic = TransformStack::default();
                                let transformed_generic = ts_generic.apply(
                                    chunk_data.clone(),
                                    3.0,
                                    &ContentType::Multimedia,
                                    stride,
                                    false,
                                );
                                let (comp_generic, bt_generic, diag_generic) = compress_block(
                                    &transformed_generic,
                                    block_type,
                                    level,
                                    &ts_generic.media_filter_type,
                                );

                                let mut ts_float = TransformStack {
                                    media_filter_applied: true,
                                    media_filter_type:
                                        transform::MediaFilterType::FloatChannelSplit {
                                            channels: s / 4,
                                            mask: 0,
                                        },
                                    ..Default::default()
                                };
                                let transformed_float = ts_float.apply(
                                    chunk_data.clone(),
                                    3.0,
                                    &ContentType::Multimedia,
                                    None,
                                    false,
                                );
                                let (comp_float, bt_float, diag_float) = compress_block(
                                    &transformed_float,
                                    block_type,
                                    level,
                                    &ts_float.media_filter_type,
                                );

                                if comp_float.len() < comp_generic.len() {
                                    (comp_float, bt_float, ts_float, diag_float)
                                } else {
                                    (comp_generic, bt_generic, ts_generic, diag_generic)
                                }
                            } else {
                                let mut ts = TransformStack::default();
                                let transformed_chunk_data = ts.apply(
                                    chunk_data,
                                    3.0,
                                    &ContentType::Multimedia,
                                    stride,
                                    bwt_enabled,
                                );
                                let (compressed, bt, diag) = compress_block(
                                    &transformed_chunk_data,
                                    block_type,
                                    level,
                                    &ts.media_filter_type,
                                );
                                (compressed, bt, ts, diag)
                            }
                        } else {
                            let mut ts = TransformStack::default();
                            let transformed_chunk_data = ts.apply(
                                chunk_data,
                                3.0,
                                &match block_type {
                                    archive::format::BlockType::Text => ContentType::Text,
                                    archive::format::BlockType::Multimedia => {
                                        ContentType::Multimedia
                                    }
                                    _ => ContentType::Binary,
                                },
                                stride,
                                bwt_enabled,
                            );
                            let (compressed, bt, diag) = compress_block(
                                &transformed_chunk_data,
                                block_type,
                                level,
                                &ts.media_filter_type,
                            );
                            (compressed, bt, ts, diag)
                        };

                        add_transform_time(transform_start.elapsed().as_secs_f64());
                        (compressed, final_block_type, ts, diag)
                    };

                transform_stack.ppm_applied = false;
                transform_stack.ppm_arena_size = 0;

                if is_analyze_enabled() {
                    file_diagnostics.merge(&diag);
                }

                block_meta.block_type = final_block_type;
                let chunk_compressed_size = compressed_chunk_bytes.len() as u32;

                compressed_block_data.extend_from_slice(&chunk_uncompressed_size.to_le_bytes());
                compressed_block_data.extend_from_slice(&chunk_compressed_size.to_le_bytes());
                compressed_block_data.extend_from_slice(&transform_stack.serialize());
                compressed_block_data.extend_from_slice(&compressed_chunk_bytes);

                chunk_index = chunk_end;
            }

            let has_key = key.is_some();
            let payload_to_write = if has_key {
                encrypt_payload(&compressed_block_data, &key)?
            } else {
                Vec::new()
            };

            let payload_ref = if has_key {
                &payload_to_write
            } else {
                &compressed_block_data
            };

            block_meta.compressed_size = payload_ref.len() as u64;
            let checksum_start = std::time::Instant::now();
            block_meta.compressed_hash = crate::integrity::ChecksumSet::compute(payload_ref).sha256;
            add_checksum_time(checksum_start.elapsed().as_secs_f64());

            self.integrity
                .record_block(block_meta.block_id, payload_ref);

            // Serialize Solid Block Header (66 bytes)
            let mut block_header = Vec::new();
            block_header.extend_from_slice(&block_meta.block_id.to_le_bytes());
            block_header.push(match block_meta.block_type {
                archive::format::BlockType::Text => 0,
                archive::format::BlockType::Binary => 1,
                archive::format::BlockType::Multimedia => 2,
                archive::format::BlockType::Mixed => 3,
                archive::format::BlockType::Raw => 4,
            });
            block_header.extend_from_slice(&block_meta.compressed_size.to_le_bytes());
            block_header.extend_from_slice(&block_meta.uncompressed_size.to_le_bytes());
            block_header.extend_from_slice(&block_meta.compressed_hash);
            block_header.extend_from_slice(&TransformStack::default().serialize());

            let block_total_size = 66 + payload_ref.len() as u64;

            // Check volume split and write block
            let (vol, offset) = writer
                .start_block(block_total_size)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let write_start = std::time::Instant::now();
            writer
                .write_payload(&block_header)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            writer
                .write_payload(payload_ref)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            add_write_time(write_start.elapsed().as_secs_f64());

            // Record block mapping in volume index
            volume_index.blocks.push((vol, offset));

            compressed_total_size += block_meta.compressed_size;
            original_total_size += block_meta.uncompressed_size;

            for mut entry in block_files {
                entry.solid_block_id = current_block_id;
                final_file_entries.push(entry);
            }

            solid_blocks_meta.push(block_meta);
            current_block_id += 1;
        }

        let _ = signal_tx.send(BufferSignal::EndOfFile);
        let background_crc_map = crc_join_handle.join().unwrap();

        flux_debug!("[FLUX] Writing file index...");
        let mut final_index = FileIndex::new();
        final_index.total_compressed_size = compressed_total_size;
        for (i, mut entry) in final_file_entries.into_iter().enumerate() {
            if let Some(&crc) = background_crc_map.get(&(i as u64)) {
                entry.original_crc32 = crc;
            }
            final_index.add_entry(entry);
        }

        let final_tree_bytes = serialize_directory_tree(&tree);
        let mut final_metadata_payload = final_index.serialize();
        final_metadata_payload.extend_from_slice(&final_tree_bytes);
        let final_metadata_encrypted = encrypt_payload(&final_metadata_payload, &key)?;

        let n = writer.current_volume;

        if n == 1 {
            // Fits in a single volume, but since volume_size > 0, we write a multi-volume layout on Volume 1.
            let path_1 = get_volume_path(&writer.base_path, 1);
            let mut file_1 = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_1)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 1. Overwrite the Front File Index placeholders
            file_1
                .seek(SeekFrom::Start(front_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&final_metadata_encrypted)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 2. Prepare serialized and optionally encrypted Volume Index
            let index_payload = if is_encrypted {
                crate::crypto::header::encrypt_volume_index(&volume_index, &key.clone().unwrap())
                    .map_err(|_| ArchiveError::DecryptionFailed)?
            } else {
                volume_index.serialize()
            };

            // 3. Append Volume Index (Front/Back copy)
            let volume_index_offset_in_vol_1 = file_1
                .seek(SeekFrom::End(0))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&index_payload)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 4. Append Back File Index Backup
            file_1
                .write_all(&final_metadata_encrypted)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 5. Append 36 bytes of placeholder checksum
            file_1
                .write_all(&[0u8; 36])
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 6. Compute logical trailing checksum (from front_index_offset to the end of the Back File Index)
            let file_len = file_1.metadata().map_err(|e| ArchiveError::Io(e.to_string()))?.len();
            file_1
                .seek(SeekFrom::Start(front_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let limit = file_len - front_index_offset - 36;
            let mut logical_sha = Sha256Hasher::new();
            let mut logical_crc = Crc32Hasher::new();
            let mut remaining = limit as usize;
            let mut buf = vec![0u8; 65536];
            while remaining > 0 {
                let to_read = std::cmp::min(remaining, buf.len());
                file_1.read_exact(&mut buf[..to_read]).map_err(|e| ArchiveError::Io(e.to_string()))?;
                logical_sha.update(&buf[..to_read]);
                logical_crc.update(&buf[..to_read]);
                remaining -= to_read;
            }
            let body_sha_digest = logical_sha.finalize();
            let body_crc_digest = logical_crc.finalize();

            // 7. Write trailing checksum at the end
            file_1
                .seek(SeekFrom::Start(file_len - 36))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&body_crc_digest.to_le_bytes())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&body_sha_digest)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 8. Rewrite Encrypted Header Data
            let encrypted_header_data = crate::crypto::header::EncryptedHeaderData {
                file_index_offset: front_index_offset,
                file_index_size: front_index_size,
                solid_block_count: current_block_id,
                compression_level: self.options.level as u8,
                transform_flags: 0,
                original_total_size,
                compressed_total_size,
                archive_sha256: body_sha_digest,
            };
            let header_payload = if is_encrypted {
                crate::crypto::header::encrypt_header_data(&encrypted_header_data, &key.unwrap())
                    .map_err(|_| ArchiveError::DecryptionFailed)?
            } else {
                encrypted_header_data.serialize()
            };
            file_1
                .seek(SeekFrom::Start(50 + p_serialized.len() as u64))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&header_payload)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 9. Rewrite the Plaintext Header at offset 50 to write version_minor = 3
            let mut plaintext_header_v13 = plaintext_header.clone();
            plaintext_header_v13.version_minor = 3;
            file_1
                .seek(SeekFrom::Start(50))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&plaintext_header_v13.serialize())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 10. Compute final payload CRC and length for Volume 1 (streamed blocks only)
            let vol_1_payload_len = volume_index_offset_in_vol_1 - 50;
            file_1
                .seek(SeekFrom::Start(50))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut vol_1_hasher = crc32fast::Hasher::new();
            let mut remaining = vol_1_payload_len as usize;
            while remaining > 0 {
                let to_read = std::cmp::min(remaining, buf.len());
                file_1.read_exact(&mut buf[..to_read]).map_err(|e| ArchiveError::Io(e.to_string()))?;
                vol_1_hasher.update(&buf[..to_read]);
                remaining -= to_read;
            }
            let vol_1_crc = vol_1_hasher.finalize();

            // 11. Write Volume Header
            let header_1 = crate::crypto::header::VolumeHeader {
                magic: *b"FLXV",
                version_major: 1,
                version_minor: 3,
                volume_number: 1,
                total_volumes: 1,
                archive_id: writer.archive_id,
                volume_payload_crc32: vol_1_crc,
                volume_payload_length: vol_1_payload_len,
                volume_index_offset: volume_index_offset_in_vol_1,
            };
            file_1
                .seek(SeekFrom::Start(0))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&header_1.serialize())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            drop(file_1);
        } else {
            // Multi-volume archive (N >= 2)
            // 1. Write Front File Index in Volume 1
            let path_1 = get_volume_path(&writer.base_path, 1);
            let mut file_1 = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_1)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .seek(SeekFrom::Start(front_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&final_metadata_encrypted)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            drop(file_1);

            // 2. Prepare serialized and optionally encrypted Volume Index
            let index_payload = if is_encrypted {
                crate::crypto::header::encrypt_volume_index(&volume_index, &key.clone().unwrap())
                    .map_err(|_| ArchiveError::DecryptionFailed)?
            } else {
                volume_index.serialize()
            };

            // 3. Write Volume Index (Back Copy) and Back File Index to Volume N
            let vol_n_payload_len = writer.current_payload_written;
            let vol_n_crc = writer.current_hasher.clone().finalize();

            writer
                .write_payload(&index_payload)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let volume_index_offset_in_vol_n =
                50 + writer.current_payload_written - index_payload.len() as u64;
            writer
                .write_payload(&final_metadata_encrypted)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // Reserve 36 bytes for trailing Archive Checksum in Volume N
            writer
                .write_payload(&[0u8; 36])
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 4. Compute logical concatenated archive trailing checksum (excluding headers/footers)
            let mut logical_sha = Sha256Hasher::new();
            let mut logical_crc = Crc32Hasher::new();
            let mut buf = vec![0u8; 65536];
            for vol in 1..=n {
                let path_v = get_volume_path(&writer.base_path, vol);
                let mut file_v =
                    File::open(&path_v).map_err(|e| ArchiveError::Io(e.to_string()))?;
                let file_len = file_v
                    .metadata()
                    .map_err(|e| ArchiveError::Io(e.to_string()))?
                    .len();
                let start_offset = if vol == 1 { front_index_offset } else { 50 };
                file_v
                    .seek(SeekFrom::Start(start_offset))
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
                let limit = if vol == n {
                    if vol == 1 {
                        file_len - front_index_offset - 36
                    } else {
                        file_len - 50 - 36
                    }
                } else {
                    file_len - start_offset
                };
                let mut remaining = limit as usize;
                while remaining > 0 {
                    let to_read = std::cmp::min(remaining, buf.len());
                    file_v
                        .read_exact(&mut buf[..to_read])
                        .map_err(|e| ArchiveError::Io(e.to_string()))?;
                    logical_sha.update(&buf[..to_read]);
                    logical_crc.update(&buf[..to_read]);
                    remaining -= to_read;
                }
            }
            let body_sha_digest = logical_sha.finalize();
            let body_crc_digest = logical_crc.finalize();

            // 5. Write trailing checksum at the end of Volume N
            let path_n = get_volume_path(&writer.base_path, n);
            let mut file_n = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_n)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let file_len_n = file_n
                .metadata()
                .map_err(|e| ArchiveError::Io(e.to_string()))?
                .len();
            file_n
                .seek(SeekFrom::Start(file_len_n - 36))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_n
                .write_all(&body_crc_digest.to_le_bytes())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_n
                .write_all(&body_sha_digest)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 6. Write Volume N Header using the pre-computed streamed payload CRC and length
            let header_n = crate::crypto::header::VolumeHeader {
                magic: *b"FLXV",
                version_major: 1,
                version_minor: 3,
                volume_number: n,
                total_volumes: n,
                archive_id: writer.archive_id,
                volume_payload_crc32: vol_n_crc,
                volume_payload_length: vol_n_payload_len,
                volume_index_offset: volume_index_offset_in_vol_n,
            };
            file_n
                .seek(SeekFrom::Start(0))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_n
                .write_all(&header_n.serialize())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            drop(file_n);

            // 7. Rewrite Encrypted Header Data in Volume 1 (now that we have final body_sha_digest)
            let encrypted_header_data = crate::crypto::header::EncryptedHeaderData {
                file_index_offset: front_index_offset,
                file_index_size: front_index_size,
                solid_block_count: current_block_id,
                compression_level: self.options.level as u8,
                transform_flags: 0,
                original_total_size,
                compressed_total_size,
                archive_sha256: body_sha_digest,
            };
            let header_payload = if is_encrypted {
                crate::crypto::header::encrypt_header_data(&encrypted_header_data, &key.unwrap())
                    .map_err(|_| ArchiveError::DecryptionFailed)?
            } else {
                encrypted_header_data.serialize()
            };
            let mut file_1_write = std::fs::OpenOptions::new()
                .write(true)
                .open(&path_1)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1_write
                .seek(SeekFrom::Start(50 + p_serialized.len() as u64))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1_write
                .write_all(&header_payload)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            drop(file_1_write);

            // 8. Write Volume Index (Front Copy) to Volume 1
            let mut file_1 = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_1)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let volume_index_offset_in_vol_1 = file_1
                .seek(SeekFrom::End(0))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&index_payload)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 9. Rewrite the Plaintext Header at offset 50 to write version_minor = 3
            let mut plaintext_header_v13 = plaintext_header.clone();
            plaintext_header_v13.version_minor = 3;
            file_1
                .seek(SeekFrom::Start(50))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&plaintext_header_v13.serialize())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            // 10. Compute final payload CRC and length for Volume 1 (streamed blocks only)
            let vol_1_payload_len = volume_index_offset_in_vol_1 - 50;
            file_1
                .seek(SeekFrom::Start(50))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut vol_1_hasher = crc32fast::Hasher::new();
            let mut remaining = vol_1_payload_len as usize;
            while remaining > 0 {
                let to_read = std::cmp::min(remaining, buf.len());
                file_1.read_exact(&mut buf[..to_read]).map_err(|e| ArchiveError::Io(e.to_string()))?;
                vol_1_hasher.update(&buf[..to_read]);
                remaining -= to_read;
            }
            let vol_1_crc = vol_1_hasher.finalize();

            let header_1 = crate::crypto::header::VolumeHeader {
                magic: *b"FLXV",
                version_major: 1,
                version_minor: 3,
                volume_number: 1,
                total_volumes: n,
                archive_id: writer.archive_id,
                volume_payload_crc32: vol_1_crc,
                volume_payload_length: vol_1_payload_len,
                volume_index_offset: volume_index_offset_in_vol_1,
            };
            file_1
                .seek(SeekFrom::Start(0))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file_1
                .write_all(&header_1.serialize())
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            drop(file_1);

            // 11. Update middle volume headers with total volume count
            for k in 2..n {
                let path_k = get_volume_path(&writer.base_path, k);
                let mut file_k = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path_k)
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
                let mut header_bytes = [0u8; 50];
                file_k
                    .read_exact(&mut header_bytes)
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
                let mut vol_header =
                    crate::crypto::header::VolumeHeader::deserialize(&header_bytes)
                        .map_err(|_| ArchiveError::HeaderCorrupt)?;
                vol_header.total_volumes = n;
                file_k
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
                file_k
                    .write_all(&vol_header.serialize())
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
            }
        }

        let elapsed = start_time.elapsed().as_millis() as u64;

        let final_size = {
            let mut total_s = 0;
            for vol in 1..=n {
                let p = get_volume_path(output, vol);
                total_s += p.metadata().map(|m| m.len()).unwrap_or(0);
            }
            total_s
        };

        flux_debug!(
            "[FLUX] Done. Total output size: {} bytes across {} volumes",
            final_size,
            n
        );

        if is_analyze_enabled() {
            print_diagnostics_report(&file_diagnostics);
        }

        Ok(CompressionStats {
            original_size: original_total_size,
            compressed_size: final_size,
            compression_ratio: if original_total_size > 0 {
                final_size as f32 / original_total_size as f32
            } else {
                1.0
            },
            files_processed: final_index.total_files() as u32,
            elapsed_ms: elapsed,
            blocks_created: current_block_id,
        })
    }
}

struct VolumeSetInfo {
    _total_volumes: u16,
    _archive_id: [u8; 16],
    volume_paths: Vec<Option<PathBuf>>, // 1-indexed, None if missing or corrupt
    metadata_source_vol: u16,
    is_multi_volume: bool,
    header_1: Option<crate::crypto::header::VolumeHeader>,
}

struct DecryptedArchiveMetadata {
    is_encrypted: bool,
    key: Option<crate::crypto::EncryptionKey>,
    original_total_size: u64,
    compressed_total_size: u64,
    file_index: FileIndex,
    directory_tree: crate::archive::DirectoryTree,
    volume_index: Option<crate::crypto::header::VolumeIndex>,
    plaintext_header: Option<crate::crypto::header::PlaintextHeader>,
    solid_block_count: u32,
    file_index_size: u64,
}

fn get_sibling_volume_path(current_path: &Path, _current_vol: u16, target_vol: u16) -> PathBuf {
    let target_ext = format!("{:03}", target_vol);
    if let Some(ext) = current_path.extension() {
        let ext_str = ext.to_string_lossy();
        if ext_str.len() == 3 && ext_str.chars().all(|c| c.is_ascii_digit()) {
            let mut path = current_path.to_path_buf();
            let filename = path.file_name().unwrap().to_string_lossy().into_owned();
            if filename.len() > 4 {
                let base_filename = &filename[..filename.len() - 4];
                path.set_file_name(format!("{}.{}", base_filename, target_ext));
                return path;
            }
        }
    }
    get_volume_path(current_path, target_vol)
}

/// High-level API for FLUX decompression pipeline.
pub struct FluxDecompressor {
    pub options: FluxOptions,
    pub progress_callback: ProgressCallback,
}

impl FluxDecompressor {
    /// Initializes a new `FluxDecompressor`.
    pub fn new(options: FluxOptions) -> Self {
        Self {
            options,
            progress_callback: None,
        }
    }

    fn resolve_volume_set(
        &self,
        input: &Path,
        verify_crc: bool,
    ) -> Result<VolumeSetInfo, ArchiveError> {
        let mut file = File::open(input).map_err(|e| ArchiveError::Io(e.to_string()))?;
        let mut magic = [0u8; 4];
        if file.read_exact(&mut magic).is_err() {
            return Err(ArchiveError::InvalidMagic);
        }
        if &magic != b"FLXV" {
            return Ok(VolumeSetInfo {
                _total_volumes: 1,
                _archive_id: [0u8; 16],
                volume_paths: vec![None, Some(input.to_path_buf())],
                metadata_source_vol: 1,
                is_multi_volume: false,
                header_1: None,
            });
        }

        // Parse the current volume header
        file.seek(SeekFrom::Start(0))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        let mut header_bytes = [0u8; 50];
        file.read_exact(&mut header_bytes)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        let start_header = crate::crypto::header::VolumeHeader::deserialize(&header_bytes)
            .map_err(|_| ArchiveError::HeaderCorrupt)?;
        if start_header.version_major != 1 {
            return Err(ArchiveError::UnsupportedVersion);
        }

        let total_volumes = start_header.total_volumes;
        let archive_id = start_header.archive_id;
        let mut volume_paths = vec![None; total_volumes as usize + 1];
        let mut header_1 = None;

        // Check Volume 1 first to see if it is healthy
        let path_1 = get_sibling_volume_path(input, start_header.volume_number, 1);
        let mut vol_1_healthy = false;
        if path_1.exists() {
            if let Ok(mut f1) = File::open(&path_1) {
                let mut hb = [0u8; 50];
                if f1.read_exact(&mut hb).is_ok() {
                    if let Ok(h1) = crate::crypto::header::VolumeHeader::deserialize(&hb) {
                        if h1.volume_number == 1 && h1.archive_id == archive_id {
                            let file_len = f1.metadata().map(|m| m.len()).unwrap_or(0);
                            if file_len >= 50 + h1.volume_payload_length {
                                let mut crc_ok = true;
                                if verify_crc {
                                    let mut hasher = crc32fast::Hasher::new();
                                    let mut buf = vec![0u8; 65536];
                                    let mut remaining = h1.volume_payload_length as usize;
                                    while remaining > 0 {
                                        let to_read = std::cmp::min(remaining, buf.len());
                                        if f1.read_exact(&mut buf[..to_read]).is_err() {
                                            crc_ok = false;
                                            break;
                                        }
                                        hasher.update(&buf[..to_read]);
                                        remaining -= to_read;
                                    }
                                    if crc_ok && hasher.finalize() != h1.volume_payload_crc32 {
                                        crc_ok = false;
                                    }
                                }
                                if crc_ok {
                                    vol_1_healthy = true;
                                    header_1 = Some(h1);
                                    volume_paths[1] = Some(path_1);
                                }
                            }
                        }
                    }
                }
            }
        }

        // Determine metadata source volume
        let mut metadata_source_vol = 0;
        if vol_1_healthy {
            metadata_source_vol = 1;
        }

        // Check all other volumes
        for vol_num in 2..=total_volumes {
            let path = get_sibling_volume_path(input, start_header.volume_number, vol_num);
            if path.exists() {
                if let Ok(mut f) = File::open(&path) {
                    let mut hb = [0u8; 50];
                    if f.read_exact(&mut hb).is_ok() {
                        if let Ok(h) = crate::crypto::header::VolumeHeader::deserialize(&hb) {
                            if h.volume_number == vol_num
                                && h.total_volumes == total_volumes
                                && h.archive_id == archive_id
                            {
                                let file_len = f.metadata().map(|m| m.len()).unwrap_or(0);
                                if file_len >= 50 + h.volume_payload_length {
                                    let mut crc_ok = true;
                                    if verify_crc {
                                        let mut hasher = crc32fast::Hasher::new();
                                        let mut buf = vec![0u8; 65536];
                                        let mut remaining = h.volume_payload_length as usize;
                                        while remaining > 0 {
                                            let to_read = std::cmp::min(remaining, buf.len());
                                            if f.read_exact(&mut buf[..to_read]).is_err() {
                                                crc_ok = false;
                                                break;
                                            }
                                            hasher.update(&buf[..to_read]);
                                            remaining -= to_read;
                                        }
                                        if crc_ok && hasher.finalize() != h.volume_payload_crc32 {
                                            crc_ok = false;
                                        }
                                    }
                                    if crc_ok {
                                        volume_paths[vol_num as usize] = Some(path);
                                        if vol_num == total_volumes && metadata_source_vol == 0 {
                                            metadata_source_vol = total_volumes;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // If metadata_source_vol is still 0, we can't read the metadata (index)
        if metadata_source_vol == 0 {
            return Err(ArchiveError::Io(
                "Neither Volume 1 nor the last volume is available or valid".to_string(),
            ));
        }

        Ok(VolumeSetInfo {
            _total_volumes: total_volumes,
            _archive_id: archive_id,
            volume_paths,
            metadata_source_vol,
            is_multi_volume: true,
            header_1,
        })
    }

    fn load_volume_index(
        &self,
        vol_info: &VolumeSetInfo,
        encrypted_header: &crate::crypto::header::EncryptedHeaderData,
        plaintext_header: &crate::crypto::header::PlaintextHeader,
        key: &Option<crate::crypto::EncryptionKey>,
    ) -> Result<crate::crypto::header::VolumeIndex, ArchiveError> {
        let path_1 = vol_info.volume_paths[1].as_ref().unwrap();
        let mut file = File::open(path_1).map_err(|e| ArchiveError::Io(e.to_string()))?;

        let block_count = encrypted_header.solid_block_count as usize;
        let index_size = if plaintext_header.is_encrypted {
            40 + 10 * block_count
        } else {
            12 + 10 * block_count
        };

        let header_1 = vol_info
            .header_1
            .as_ref()
            .ok_or(ArchiveError::HeaderCorrupt)?;
        file.seek(SeekFrom::Start(header_1.volume_index_offset))
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        let mut index_bytes = vec![0u8; index_size];
        file.read_exact(&mut index_bytes)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;

        let decrypted = if plaintext_header.is_encrypted {
            crate::crypto::header::decrypt_volume_index(&index_bytes, key.as_ref().unwrap())
        } else {
            crate::crypto::header::VolumeIndex::deserialize(&index_bytes)
        };

        decrypted.map_err(|_| ArchiveError::CorruptIndex)
    }

    fn load_metadata(
        &self,
        vol_info: &VolumeSetInfo,
    ) -> Result<DecryptedArchiveMetadata, ArchiveError> {
        let password_str = if self.options.password.is_null() {
            None
        } else {
            unsafe {
                std::ffi::CStr::from_ptr(self.options.password)
                    .to_str()
                    .ok()
            }
        };

        if vol_info.metadata_source_vol == 1 {
            // Read from Volume 1 (standard path)
            let path_1 = vol_info.volume_paths[1].as_ref().unwrap();
            let mut file = File::open(path_1).map_err(|e| ArchiveError::Io(e.to_string()))?;

            // Seek past Volume Header if multi-volume
            if vol_info.is_multi_volume {
                file.seek(SeekFrom::Start(50))
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
            } else {
                file.seek(SeekFrom::Start(0))
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;
            }

            let mut bootstrap_base = vec![0u8; 96];
            file.read_exact(&mut bootstrap_base)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            if &bootstrap_base[0..4] != b"FLUX" {
                return Err(ArchiveError::InvalidMagic);
            }
            let ct_len = u32::from_le_bytes(bootstrap_base[92..96].try_into().unwrap()) as usize;

            let mut bootstrap_full = bootstrap_base;
            let mut ct_bytes = vec![0u8; ct_len];
            file.read_exact(&mut ct_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            bootstrap_full.extend_from_slice(&ct_bytes);

            let mut crc_bytes = [0u8; 4];
            file.read_exact(&mut crc_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            bootstrap_full.extend_from_slice(&crc_bytes);

            let plaintext_header =
                crate::crypto::header::PlaintextHeader::deserialize(&bootstrap_full)
                    .map_err(|_| ArchiveError::HeaderCorrupt)?;

            if plaintext_header.version_major != 1 {
                return Err(ArchiveError::UnsupportedVersion);
            }

            let key = if plaintext_header.is_encrypted {
                let pass = password_str.ok_or(ArchiveError::WrongPassword)?;
                let k =
                    crate::crypto::keys::KeyDerivation::derive_key(pass, &plaintext_header.salt)
                        .map_err(|_| ArchiveError::WrongPassword)?;
                let matches =
                    crate::crypto::sentinel::verify_sentinel(&k, &plaintext_header.sentinel_chunk)
                        .map_err(|_| ArchiveError::DecryptionFailed)?;
                if !matches {
                    return Err(ArchiveError::WrongPassword);
                }
                Some(k)
            } else {
                None
            };

            let header_data_len = if plaintext_header.is_encrypted {
                98
            } else {
                70
            };
            let mut header_data_bytes = vec![0u8; header_data_len];
            file.read_exact(&mut header_data_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let encrypted_header = if plaintext_header.is_encrypted {
                crate::crypto::header::decrypt_header_data(
                    &header_data_bytes,
                    &key.clone().unwrap(),
                )
                .map_err(|_| ArchiveError::DecryptionFailed)?
            } else {
                crate::crypto::header::EncryptedHeaderData::deserialize(&header_data_bytes)
                    .map_err(|_| ArchiveError::CorruptIndex)?
            };

            // Read Front File Index
            let mut metadata_payload_bytes = vec![0u8; encrypted_header.file_index_size as usize];
            file.seek(SeekFrom::Start(encrypted_header.file_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            file.read_exact(&mut metadata_payload_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let decrypted_metadata = decrypt_payload(&metadata_payload_bytes, &key)?;

            let (file_index, remaining_bytes) = FileIndex::deserialize_prefix(&decrypted_metadata)?;
            let directory_tree = deserialize_directory_tree(remaining_bytes)
                .map_err(|_| ArchiveError::CorruptIndex)?;

            // Read Volume Index if multi-volume
            let volume_index = if vol_info.is_multi_volume {
                Some(self.load_volume_index(
                    vol_info,
                    &encrypted_header,
                    &plaintext_header,
                    &key,
                )?)
            } else {
                None
            };

            Ok(DecryptedArchiveMetadata {
                is_encrypted: plaintext_header.is_encrypted,
                key,
                original_total_size: encrypted_header.original_total_size,
                compressed_total_size: encrypted_header.compressed_total_size,
                file_index,
                directory_tree,
                volume_index,
                plaintext_header: Some(plaintext_header),
                solid_block_count: encrypted_header.solid_block_count,
                file_index_size: encrypted_header.file_index_size,
            })
        } else {
            // Read from Volume N (only possible for multi-volume where Volume 1 is missing)
            let path_n = vol_info.volume_paths[vol_info.metadata_source_vol as usize]
                .as_ref()
                .unwrap();
            let mut file = File::open(path_n).map_err(|e| ArchiveError::Io(e.to_string()))?;
            let file_len = file
                .metadata()
                .map_err(|e| ArchiveError::Io(e.to_string()))?
                .len();

            let mut hb = [0u8; 50];
            file.read_exact(&mut hb)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let header_n = crate::crypto::header::VolumeHeader::deserialize(&hb)
                .map_err(|_| ArchiveError::HeaderCorrupt)?;
            if header_n.version_major != 1 {
                return Err(ArchiveError::UnsupportedVersion);
            }

            if header_n.volume_index_offset == 0 {
                return Err(ArchiveError::CorruptIndex);
            }

            file.seek(SeekFrom::Start(header_n.volume_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut magic = [0u8; 4];
            file.read_exact(&mut magic)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let is_encrypted = &magic != b"FLXI";

            if is_encrypted {
                return Err(ArchiveError::Io("Volume 1 is missing; cannot decrypt archive metadata without key derivation salt".to_string()));
            }

            // It is not encrypted! So we can load the Volume Index and Back File Index in plaintext.
            // 1. Load Volume Index
            file.seek(SeekFrom::Start(header_n.volume_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut size_buf = [0u8; 8];
            file.read_exact(&mut size_buf)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let block_count = u32::from_le_bytes(size_buf[4..8].try_into().unwrap()) as usize;
            let index_size = 12 + 10 * block_count;

            file.seek(SeekFrom::Start(header_n.volume_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut index_data = vec![0u8; index_size];
            file.read_exact(&mut index_data)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let volume_index = crate::crypto::header::VolumeIndex::deserialize(&index_data)
                .map_err(|_| ArchiveError::CorruptIndex)?;

            // 2. Load Back File Index
            let back_index_offset = header_n.volume_index_offset + index_size as u64;
            let back_index_size = (file_len - 36) - back_index_offset;
            file.seek(SeekFrom::Start(back_index_offset))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut metadata_payload_bytes = vec![0u8; back_index_size as usize];
            file.read_exact(&mut metadata_payload_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let (file_index, remaining_bytes) =
                FileIndex::deserialize_prefix(&metadata_payload_bytes)?;
            let directory_tree = deserialize_directory_tree(remaining_bytes)
                .map_err(|_| ArchiveError::CorruptIndex)?;

            let original_total_size = file_index.entries.iter().map(|e| e.original_size).sum();
            let compressed_total_size = file_index.total_compressed_size;

            Ok(DecryptedArchiveMetadata {
                is_encrypted: false,
                key: None,
                original_total_size,
                compressed_total_size,
                file_index,
                directory_tree,
                volume_index: Some(volume_index),
                plaintext_header: None,
                solid_block_count: block_count as u32,
                file_index_size: back_index_size,
            })
        }
    }

    /// Reads and returns basic information about the archive, including original size, compressed size, file count, and encryption flag.
    pub fn read_archive_info(
        &mut self,
        input: &Path,
    ) -> Result<(u64, u64, u32, bool), ArchiveError> {
        let vol_info = self.resolve_volume_set(input, false)?;
        let meta = self.load_metadata(&vol_info)?;
        Ok((
            meta.original_total_size,
            meta.compressed_total_size,
            meta.file_index.entries.len() as u32,
            meta.is_encrypted,
        ))
    }

    /// Lists all file paths preserved inside the archive.
    pub fn list_files(&mut self, input: &Path) -> Result<Vec<String>, ArchiveError> {
        let vol_info = self.resolve_volume_set(input, false)?;
        let meta = self.load_metadata(&vol_info)?;
        Ok(meta
            .file_index
            .entries
            .into_iter()
            .map(|e| e.path)
            .collect())
    }

    /// Decompresses a FLUX archive, verifying integrity and restoring files/folders.
    pub fn decompress(
        &mut self,
        input: &Path,
        output: &Path,
    ) -> Result<DecompressionStats, ArchiveError> {
        let start_time = std::time::Instant::now();
        let vol_info = self.resolve_volume_set(input, true)?;
        let meta = self.load_metadata(&vol_info)?;

        if let Some(ref plaintext_header) = meta.plaintext_header {
            // RAM check & allocation attempt
            let mut sys = sysinfo::System::new();
            sys.refresh_memory();
            let available_ram = sys.available_memory();
            let required_ram = plaintext_header.window_size as u64;
            let required_mb = required_ram.div_ceil(1024 * 1024);
            let available_mb = available_ram / (1024 * 1024);

            if required_ram > available_ram {
                return Err(ArchiveError::Io(format!(
                    "This archive requires a {} MB decompression window, but only {} MB is available. Free memory or decompress on a machine with more RAM.",
                    required_mb, available_mb
                )));
            }

            let mut test_alloc: Vec<u8> = Vec::new();
            if test_alloc
                .try_reserve_exact(plaintext_header.window_size as usize)
                .is_err()
            {
                return Err(ArchiveError::Io(format!(
                    "This archive requires a {} MB decompression window, but only {} MB is available. Free memory or decompress on a machine with more RAM.",
                    required_mb, available_mb
                )));
            }
            drop(test_alloc);
        }

        if !vol_info.is_multi_volume {
            // Standard single-volume decompression path
            let path_1 = vol_info.volume_paths[1].as_ref().unwrap();
            let mut file = File::open(path_1).map_err(|e| ArchiveError::Io(e.to_string()))?;
            let file_len = file
                .metadata()
                .map_err(|e| ArchiveError::Io(e.to_string()))?
                .len();

            let plaintext_header = meta.plaintext_header.as_ref().unwrap();
            let header_data_len = if plaintext_header.is_encrypted {
                98
            } else {
                70
            };

            // Re-read file index offset from the start
            let p_serialized_len = plaintext_header.serialize().len() as u64;
            let file_index_offset = p_serialized_len + header_data_len as u64;

            let archive_body_start = file_index_offset;
            let archive_body_end = file_len - 36;

            file.seek(SeekFrom::Start(archive_body_start))
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let mut body_bytes = vec![0u8; (archive_body_end - archive_body_start) as usize];
            file.read_exact(&mut body_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let mut trailing_crc_bytes = [0u8; 4];
            file.read_exact(&mut trailing_crc_bytes)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;
            let trailing_crc = u32::from_le_bytes(trailing_crc_bytes);

            let mut trailing_sha = [0u8; 32];
            file.read_exact(&mut trailing_sha)
                .map_err(|e| ArchiveError::Io(e.to_string()))?;

            let computed_cset = ChecksumSet::compute(&body_bytes);
            let expected_sha = trailing_sha;
            if computed_cset.sha256 != expected_sha
                || computed_cset.sha256 != trailing_sha
                || computed_cset.crc32 != trailing_crc
            {
                return Err(ArchiveError::CorruptIndex);
            }

            meta.directory_tree.restore(output)?;

            let mut pos = meta.file_index_size as usize;
            let mut files_extracted = 0;
            let mut bytes_written = 0;
            let total_bytes: u64 = meta
                .file_index
                .entries
                .iter()
                .map(|e| e.original_size)
                .sum();

            for block_id in 0..meta.solid_block_count {
                if pos + 66 > body_bytes.len() {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                // Read block header
                let header_slice = &body_bytes[pos..pos + 66];
                pos += 66;

                let b_id = u32::from_le_bytes(header_slice[0..4].try_into().unwrap());
                let b_type_tag = header_slice[4];
                let compressed_size =
                    u64::from_le_bytes(header_slice[5..13].try_into().unwrap()) as usize;
                let uncompressed_size =
                    u64::from_le_bytes(header_slice[13..21].try_into().unwrap()) as usize;
                let mut compressed_hash = [0u8; 32];
                compressed_hash.copy_from_slice(&header_slice[21..53]);
                let (_transform_stack, _) = TransformStack::deserialize(&header_slice[53..66])
                    .map_err(|e| match e {
                        TransformDeserializationError::UnsupportedFilterType(_) => ArchiveError::UnsupportedVersion,
                        _ => ArchiveError::CorruptBlock(block_id),
                    })?;

                let block_type = match b_type_tag {
                    0 => archive::format::BlockType::Text,
                    1 => archive::format::BlockType::Binary,
                    2 => archive::format::BlockType::Multimedia,
                    3 => archive::format::BlockType::Mixed,
                    4 => archive::format::BlockType::Raw,
                    _ => return Err(ArchiveError::CorruptBlock(block_id)),
                };

                if b_id != block_id {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                if pos + compressed_size > body_bytes.len() {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                let block_payload = &body_bytes[pos..pos + compressed_size];
                pos += compressed_size;

                // Verify block checksum
                let actual_block_hash =
                    crate::integrity::ChecksumSet::compute(block_payload).sha256;
                if actual_block_hash != compressed_hash {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                // Decrypt block payload if encrypted
                let decrypted_payload = decrypt_payload(block_payload, &meta.key)?;

                // Parse and decompress sub-blocks sequentially
                let mut restored_payload = Vec::with_capacity(uncompressed_size);
                let mut sub_pos = 0;
                while sub_pos < decrypted_payload.len() {
                    if sub_pos + 21 > decrypted_payload.len() {
                        return Err(ArchiveError::CorruptBlock(block_id));
                    }

                    // Read sub-block header (21 bytes)
                    let sub_uncompressed_size = u32::from_le_bytes(
                        decrypted_payload[sub_pos..sub_pos + 4].try_into().unwrap(),
                    ) as usize;
                    let sub_compressed_size = u32::from_le_bytes(
                        decrypted_payload[sub_pos + 4..sub_pos + 8]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    let (sub_transform_stack, _) =
                        TransformStack::deserialize(&decrypted_payload[sub_pos + 8..sub_pos + 21])
                            .map_err(|e| match e {
                                TransformDeserializationError::UnsupportedFilterType(_) => ArchiveError::UnsupportedVersion,
                                _ => ArchiveError::CorruptBlock(block_id),
                            })?;

                    sub_pos += 21;

                    if sub_pos + sub_compressed_size > decrypted_payload.len() {
                        return Err(ArchiveError::CorruptBlock(block_id));
                    }
                    let sub_chunk_bytes =
                        &decrypted_payload[sub_pos..sub_pos + sub_compressed_size];
                    sub_pos += sub_compressed_size;

                    // Decompress sub-block
                    let mut lz77_uncompressed_size = sub_uncompressed_size;
                    if sub_transform_stack.bwt_applied {
                        const CHUNK_LIMIT: usize = 1_048_576;
                        if sub_uncompressed_size > CHUNK_LIMIT {
                            let num_chunks = sub_uncompressed_size.div_ceil(CHUNK_LIMIT);
                            lz77_uncompressed_size += 4 + 4 * num_chunks;
                        }
                    }
                    let decompressed_chunk = decompress_block(
                        sub_chunk_bytes,
                        block_type,
                        lz77_uncompressed_size,
                        &sub_transform_stack,
                    )?;

                    // Reverse transform stack for this sub-block
                    let restored_chunk = sub_transform_stack.reverse(decompressed_chunk);
                    restored_payload.extend_from_slice(&restored_chunk);
                }

                if restored_payload.len() != uncompressed_size {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                // Write files in this solid block ONLY after whole block integrity verification passes
                let block_entries: Vec<&archive::format::FileEntry> = meta
                    .file_index
                    .entries
                    .iter()
                    .filter(|e| e.solid_block_id == block_id)
                    .collect();

                for entry in block_entries {
                    let start = entry.block_offset as usize;
                    let end = start + entry.original_size as usize;
                    if end > restored_payload.len() {
                        return Err(ArchiveError::CorruptFile(entry.path.clone()));
                    }

                    if let Some(ref cb) = self.progress_callback {
                        cb(bytes_written, total_bytes, entry.path.clone());
                    }

                    let file_data = &restored_payload[start..end];

                    // Verify file level checksums (CRC32 and SHA-256)
                    let actual_cset = ChecksumSet::compute(file_data);
                    if actual_cset.crc32 != entry.original_crc32
                        || actual_cset.sha256 != entry.original_sha256
                    {
                        return Err(ArchiveError::CorruptFile(entry.path.clone()));
                    }

                    let file_out_path = output.join(&entry.path);

                    // Write file to disk
                    if let Some(parent) = file_out_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| ArchiveError::Io(e.to_string()))?;
                    }
                    std::fs::write(&file_out_path, file_data)
                        .map_err(|e| ArchiveError::Io(e.to_string()))?;

                    files_extracted += 1;
                    bytes_written += entry.original_size;
                }
            }

            meta.directory_tree.restore_metadata(output)?;

            let elapsed = start_time.elapsed().as_millis() as u64;

            Ok(DecompressionStats {
                files_extracted,
                bytes_written,
                elapsed_ms: elapsed,
                integrity_verified: true,
            })
        } else {
            // Multi-volume decompression path
            meta.directory_tree.restore(output)?;

            let mut files_extracted = 0;
            let mut bytes_written = 0;
            let total_bytes: u64 = meta
                .file_index
                .entries
                .iter()
                .map(|e| e.original_size)
                .sum();

            // We will cache open files to avoid opening/closing the same volume repeatedly
            let mut open_files: std::collections::HashMap<u16, File> =
                std::collections::HashMap::new();

            for block_id in 0..meta.solid_block_count {
                // 1. Get volume number and offset for this block from VolumeIndex
                let (vol_num, block_offset) = if let Some(ref vol_idx) = meta.volume_index {
                    if (block_id as usize) < vol_idx.blocks.len() {
                        vol_idx.blocks[block_id as usize]
                    } else {
                        return Err(ArchiveError::CorruptBlock(block_id));
                    }
                } else {
                    return Err(ArchiveError::CorruptBlock(block_id));
                };

                let vol_path_opt = if (vol_num as usize) < vol_info.volume_paths.len() {
                    &vol_info.volume_paths[vol_num as usize]
                } else {
                    &None
                };

                let block_entries: Vec<&archive::format::FileEntry> = meta
                    .file_index
                    .entries
                    .iter()
                    .filter(|e| e.solid_block_id == block_id)
                    .collect();

                if block_entries.is_empty() {
                    continue;
                }

                if vol_path_opt.is_none() {
                    let file_paths: Vec<String> =
                        block_entries.iter().map(|e| e.path.clone()).collect();
                    return Err(ArchiveError::Io(format!(
                        "File extraction failed: volume {} is missing or corrupt (failed files: {})",
                        vol_num, file_paths.join(", ")
                    )));
                }

                let vol_path = vol_path_opt.as_ref().unwrap();

                if let std::collections::hash_map::Entry::Vacant(e) = open_files.entry(vol_num) {
                    let f = File::open(vol_path).map_err(|e| ArchiveError::Io(e.to_string()))?;
                    e.insert(f);
                }
                let file = open_files.get_mut(&vol_num).unwrap();

                file.seek(SeekFrom::Start(block_offset))
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;

                let mut header_slice = [0u8; 66];
                file.read_exact(&mut header_slice)
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;

                let b_id = u32::from_le_bytes(header_slice[0..4].try_into().unwrap());
                let b_type_tag = header_slice[4];
                let compressed_size =
                    u64::from_le_bytes(header_slice[5..13].try_into().unwrap()) as usize;
                let uncompressed_size =
                    u64::from_le_bytes(header_slice[13..21].try_into().unwrap()) as usize;
                let mut compressed_hash = [0u8; 32];
                compressed_hash.copy_from_slice(&header_slice[21..53]);
                let (_transform_stack, _) = TransformStack::deserialize(&header_slice[53..66])
                    .map_err(|e| match e {
                        TransformDeserializationError::UnsupportedFilterType(_) => ArchiveError::UnsupportedVersion,
                        _ => ArchiveError::CorruptBlock(block_id),
                    })?;

                let block_type = match b_type_tag {
                    0 => archive::format::BlockType::Text,
                    1 => archive::format::BlockType::Binary,
                    2 => archive::format::BlockType::Multimedia,
                    3 => archive::format::BlockType::Mixed,
                    4 => archive::format::BlockType::Raw,
                    _ => return Err(ArchiveError::CorruptBlock(block_id)),
                };

                if b_id != block_id {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                let mut block_payload = vec![0u8; compressed_size];
                file.read_exact(&mut block_payload)
                    .map_err(|e| ArchiveError::Io(e.to_string()))?;

                let actual_block_hash =
                    crate::integrity::ChecksumSet::compute(&block_payload).sha256;
                if actual_block_hash != compressed_hash {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                let decrypted_payload = decrypt_payload(&block_payload, &meta.key)?;

                let mut restored_payload = Vec::with_capacity(uncompressed_size);
                let mut sub_pos = 0;
                while sub_pos < decrypted_payload.len() {
                    if sub_pos + 21 > decrypted_payload.len() {
                        return Err(ArchiveError::CorruptBlock(block_id));
                    }

                    let sub_uncompressed_size = u32::from_le_bytes(
                        decrypted_payload[sub_pos..sub_pos + 4].try_into().unwrap(),
                    ) as usize;
                    let sub_compressed_size = u32::from_le_bytes(
                        decrypted_payload[sub_pos + 4..sub_pos + 8]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    let (sub_transform_stack, _) =
                        TransformStack::deserialize(&decrypted_payload[sub_pos + 8..sub_pos + 21])
                            .map_err(|e| match e {
                                TransformDeserializationError::UnsupportedFilterType(_) => ArchiveError::UnsupportedVersion,
                                _ => ArchiveError::CorruptBlock(block_id),
                            })?;

                    sub_pos += 21;

                    if sub_pos + sub_compressed_size > decrypted_payload.len() {
                        return Err(ArchiveError::CorruptBlock(block_id));
                    }
                    let sub_chunk_bytes =
                        &decrypted_payload[sub_pos..sub_pos + sub_compressed_size];
                    sub_pos += sub_compressed_size;

                    let mut lz77_uncompressed_size = sub_uncompressed_size;
                    if sub_transform_stack.bwt_applied {
                        const CHUNK_LIMIT: usize = 1_048_576;
                        if sub_uncompressed_size > CHUNK_LIMIT {
                            let num_chunks = sub_uncompressed_size.div_ceil(CHUNK_LIMIT);
                            lz77_uncompressed_size += 4 + 4 * num_chunks;
                        }
                    }
                    let decompressed_chunk = decompress_block(
                        sub_chunk_bytes,
                        block_type,
                        lz77_uncompressed_size,
                        &sub_transform_stack,
                    )?;

                    let restored_chunk = sub_transform_stack.reverse(decompressed_chunk);
                    restored_payload.extend_from_slice(&restored_chunk);
                }

                if restored_payload.len() != uncompressed_size {
                    return Err(ArchiveError::CorruptBlock(block_id));
                }

                for entry in block_entries {
                    let start = entry.block_offset as usize;
                    let end = start + entry.original_size as usize;
                    if end > restored_payload.len() {
                        return Err(ArchiveError::CorruptFile(entry.path.clone()));
                    }

                    if let Some(ref cb) = self.progress_callback {
                        cb(bytes_written, total_bytes, entry.path.clone());
                    }

                    let file_data = &restored_payload[start..end];

                    let actual_cset = ChecksumSet::compute(file_data);
                    if actual_cset.crc32 != entry.original_crc32
                        || actual_cset.sha256 != entry.original_sha256
                    {
                        return Err(ArchiveError::CorruptFile(entry.path.clone()));
                    }

                    let file_out_path = output.join(&entry.path);

                    if let Some(parent) = file_out_path.parent() {
                        std::fs::create_dir_all(parent)
                            .map_err(|e| ArchiveError::Io(e.to_string()))?;
                    }
                    std::fs::write(&file_out_path, file_data)
                        .map_err(|e| ArchiveError::Io(e.to_string()))?;

                    files_extracted += 1;
                    bytes_written += entry.original_size;
                }
            }

            meta.directory_tree.restore_metadata(output)?;

            let elapsed = start_time.elapsed().as_millis() as u64;

            Ok(DecompressionStats {
                files_extracted,
                bytes_written,
                elapsed_ms: elapsed,
                integrity_verified: true,
            })
        }
    }
}

// =========================================================================
// Pipeline-specific block-level compressor / decompressor logic
// =========================================================================

fn serialize_frequency_table(table: &crate::compress::rans::ProbabilityTable) -> Vec<u8> {
    let mut buf = Vec::with_capacity(512);
    for sym in &table.symbols {
        buf.extend_from_slice(&(sym.freq as u16).to_le_bytes());
    }
    buf
}

fn deserialize_frequency_table(
    data: &[u8],
) -> Result<(crate::compress::rans::ProbabilityTable, &[u8]), ArchiveError> {
    if data.len() < 512 {
        return Err(ArchiveError::CorruptIndex);
    }
    let mut freq = [0u32; 256];
    for i in 0..256 {
        let f = u16::from_le_bytes(data[i * 2..i * 2 + 2].try_into().unwrap()) as u32;
        freq[i] = f;
    }

    let sum: u32 = freq.iter().sum();
    if sum != 4096 {
        return Err(ArchiveError::CorruptIndex);
    }

    let mut symbols = Vec::with_capacity(256);
    let mut cumfreq = 0;
    for &f in &freq {
        symbols.push(crate::compress::rans::RansSymbol { freq: f, cumfreq });
        cumfreq += f;
    }

    let mut alias_table = vec![0u32; 4096];
    for (sym, s_info) in symbols.iter().enumerate() {
        for offset in 0..s_info.freq {
            let slot = (s_info.cumfreq + offset) as usize;
            if slot < 4096 {
                alias_table[slot] = sym as u32;
            }
        }
    }

    Ok((
        crate::compress::rans::ProbabilityTable {
            symbols,
            alias_table,
        },
        &data[512..],
    ))
}

fn compress_block(
    data: &[u8],
    block_type: archive::format::BlockType,
    level: FluxCompressionLevel,
    media_filter: &transform::MediaFilterType,
) -> (Vec<u8>, archive::format::BlockType, StreamDiagnostics) {
    let mut estimator = crate::analysis::EntropyEstimator::new();
    estimator.update(data);
    let entropy = estimator.shannon_entropy();

    // V1 KISS: BlockType::Raw or entropy > 7.95 -> store raw (no compression)
    if block_type == archive::format::BlockType::Raw || entropy > 7.95 {
        return (
            data.to_vec(),
            archive::format::BlockType::Raw,
            StreamDiagnostics::default(),
        );
    }

    match block_type {
        archive::format::BlockType::Raw => unreachable!(),
        archive::format::BlockType::Text
        | archive::format::BlockType::Binary
        | archive::format::BlockType::Mixed
        | archive::format::BlockType::Multimedia => {
            // LZ77 then rANS directly (NO PPM)
            let lz77_start = std::time::Instant::now();
            let window_size = crate::ffi::window_size_for_level(level);
            let mut lz77 = match level {
                FluxCompressionLevel::Tiny => {
                    crate::compress::lz77::Lz77Encoder::new_with_params(
                        window_size,
                        true, // lazy_matching
                        256,  // max_chain_depth
                        16,   // lazy_match_threshold
                        16,   // good_match
                        crate::compress::lz77::MatchFinder::HashChain,
                    )
                }
                FluxCompressionLevel::Fast => {
                    crate::compress::lz77::Lz77Encoder::new_with_params(
                        window_size,
                        false, // lazy_matching
                        128,   // max_chain_depth
                        0,     // lazy_match_threshold
                        8,     // good_match
                        crate::compress::lz77::MatchFinder::HashChain,
                    )
                }
                FluxCompressionLevel::Balanced => {
                    crate::compress::lz77::Lz77Encoder::new_with_params(
                        window_size,
                        true, // lazy_matching
                        1024, // max_chain_depth
                        128,  // lazy_match_threshold
                        32,   // good_match
                        crate::compress::lz77::MatchFinder::HashChain,
                    )
                }
                FluxCompressionLevel::Maximum => {
                    crate::compress::lz77::Lz77Encoder::new_with_params(
                        window_size,
                        true, // lazy_matching
                        4096, // max_chain_depth
                        258,  // lazy_match_threshold
                        258,  // good_match
                        crate::compress::lz77::MatchFinder::BinaryTree,
                    )
                }
                FluxCompressionLevel::Extreme => {
                    crate::compress::lz77::Lz77Encoder::new_with_params(
                        window_size,
                        true, // lazy_matching
                        8192, // max_chain_depth
                        258,  // lazy_match_threshold
                        258,  // good_match
                        crate::compress::lz77::MatchFinder::BinaryTree,
                    )
                }
            };
            let tokens = lz77.encode_with_media_filter(data, media_filter);
            // PPM (Order-4 context model) is implemented and available but
            // DISABLED by default. Measurement showed it gains only ~1% on
            // prose (literals are ~7% of output in our LZ77-first pipeline)
            // and can slightly hurt mixed data via escape overhead. The code
            // is retained for future research / re-enablement. To re-enable,
            // set ppm_applied = true for the desired levels here.
            let ppm_applied = false;
            let ppm_arena_size = 0;
            let (output, diag) =
                serialize_lz77_tokens(&tokens, media_filter, data, ppm_applied, ppm_arena_size);
            add_lz77_time(lz77_start.elapsed().as_secs_f64());

            (output, block_type, diag)
        }
    }
}

fn decompress_block(
    compressed: &[u8],
    block_type: archive::format::BlockType,
    uncompressed_size: usize,
    transform_stack: &transform::TransformStack,
) -> Result<Vec<u8>, ArchiveError> {
    match block_type {
        archive::format::BlockType::Raw => Ok(compressed.to_vec()),
        archive::format::BlockType::Text
        | archive::format::BlockType::Binary
        | archive::format::BlockType::Mixed
        | archive::format::BlockType::Multimedia => {
            let tokens = deserialize_lz77_tokens(compressed, transform_stack, uncompressed_size)?;
            let mut decoded = Vec::with_capacity(uncompressed_size);
            for token in tokens {
                match token {
                    Lz77Token::Literal(b) => {
                        decoded.push(b);
                    }
                    Lz77Token::Match { distance, length } => {
                        let dist = distance as usize;
                        let len = length as usize;
                        if dist > decoded.len() {
                            flux_debug!("[DECOMPRESS_BLOCK FAIL] match distance {} exceeds decoded len {}", dist, decoded.len());
                            return Err(ArchiveError::CorruptIndex);
                        }
                        let start = decoded.len() - dist;
                        for j in 0..len {
                            let b = decoded[start + j];
                            decoded.push(b);
                        }
                    }
                    Lz77Token::RepMatch { .. } => unreachable!("RepMatch tokens should be reconstructed to Match in deserialize_lz77_tokens"),
                }
            }
            if decoded.len() != uncompressed_size {
                flux_debug!(
                    "[DECOMPRESS_BLOCK FAIL] decoded len {} != uncompressed_size {}",
                    decoded.len(),
                    uncompressed_size
                );
                return Err(ArchiveError::CorruptIndex);
            }
            Ok(decoded)
        }
    }
}

// =========================================================================
// LZ77 Token serialization / deserialization helpers
// =========================================================================

fn build_probability_table_for_stream(data: &[u8]) -> crate::compress::rans::ProbabilityTable {
    if data.is_empty() {
        return crate::compress::rans::ProbabilityTable::from_probabilities(&[]);
    }
    let mut counts = [0u32; 256];
    for &sym in data {
        counts[sym as usize] += 1;
    }
    let total = data.len() as f32;
    let mut probs_list = Vec::new();
    for (i, &count) in counts.iter().enumerate() {
        if count > 0 {
            probs_list.push((i as u8, count as f32 / total));
        }
    }
    crate::compress::rans::ProbabilityTable::from_probabilities(&probs_list)
}

fn rans_encode_stream(data: &[u8], table: &crate::compress::rans::ProbabilityTable) -> Vec<u8> {
    let mut encoder = crate::compress::rans::RansEncoder::new();
    for i in (0..data.len()).rev() {
        encoder.encode_symbol(data[i], table);
    }
    encoder.flush()
}

fn rans_decode_stream(
    compressed: &[u8],
    len: usize,
    table: &crate::compress::rans::ProbabilityTable,
) -> Result<Vec<u8>, ArchiveError> {
    if compressed.len() < 8 {
        return Err(ArchiveError::CorruptIndex);
    }
    let mut decoder = crate::compress::rans::RansDecoder::new(compressed.to_vec());
    let mut decoded = vec![0u8; len];
    for sym in &mut decoded {
        *sym = decoder.decode_symbol(table);
    }
    Ok(decoded)
}

fn is_analyze_enabled() -> bool {
    std::env::var("FLUX_TIMING").is_ok() || std::env::var("FLUX_ANALYZE").is_ok()
}

#[derive(Default, Clone, Debug)]
struct StreamDiagnostics {
    total_flags_bytes: usize,
    total_literals_bytes: usize,
    total_lengths_bytes: usize,
    total_slots_bytes: usize,
    total_extra_bits_bytes: usize,
    total_rep_indices_bytes: usize,
    total_table_bytes: usize,
    total_sub_block_headers_bytes: usize,
    num_sub_blocks: usize,
    total_literals_count: usize,
    total_matches_count: usize,
    total_rep_matches_count: usize,
    literals_raw: Vec<u8>,
    lengths_raw: Vec<u8>,
    slots_raw: Vec<u8>,
    rep_indices_raw: Vec<u8>,
}

impl StreamDiagnostics {
    fn merge(&mut self, other: &Self) {
        self.total_flags_bytes += other.total_flags_bytes;
        self.total_literals_bytes += other.total_literals_bytes;
        self.total_lengths_bytes += other.total_lengths_bytes;
        self.total_slots_bytes += other.total_slots_bytes;
        self.total_extra_bits_bytes += other.total_extra_bits_bytes;
        self.total_rep_indices_bytes += other.total_rep_indices_bytes;
        self.total_table_bytes += other.total_table_bytes;
        self.total_sub_block_headers_bytes += other.total_sub_block_headers_bytes;
        self.num_sub_blocks += other.num_sub_blocks;
        self.total_literals_count += other.total_literals_count;
        self.total_matches_count += other.total_matches_count;
        self.total_rep_matches_count += other.total_rep_matches_count;
        self.literals_raw.extend_from_slice(&other.literals_raw);
        self.lengths_raw.extend_from_slice(&other.lengths_raw);
        self.slots_raw.extend_from_slice(&other.slots_raw);
        self.rep_indices_raw
            .extend_from_slice(&other.rep_indices_raw);
    }
}

fn compute_entropies(data: &[u8]) -> (f64, f64) {
    if data.is_empty() {
        return (0.0, 0.0);
    }

    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let len = data.len() as f64;
    let mut h0 = 0.0;
    for &count in &counts {
        if count > 0 {
            let p = count as f64 / len;
            h0 -= p * p.log2();
        }
    }

    if data.len() <= 1 {
        return (h0, 0.0);
    }

    let mut transitions = vec![[0u64; 256]; 256];
    let mut prev_counts = [0u64; 256];
    for window in data.windows(2) {
        let prev = window[0] as usize;
        let curr = window[1] as usize;
        transitions[prev][curr] += 1;
        prev_counts[prev] += 1;
    }

    let total_transitions = (data.len() - 1) as f64;
    let mut h1 = 0.0;
    for prev in 0..256 {
        let prev_count = prev_counts[prev];
        if prev_count > 0 {
            let p_prev = prev_count as f64 / total_transitions;
            let mut cond_entropy = 0.0;
            for &trans_count in &transitions[prev] {
                if trans_count > 0 {
                    let p_cond = trans_count as f64 / prev_count as f64;
                    cond_entropy -= p_cond * p_cond.log2();
                }
            }
            h1 += p_prev * cond_entropy;
        }
    }

    (h0, h1)
}

fn print_diagnostics_report(diag: &StreamDiagnostics) {
    if !is_analyze_enabled() {
        return;
    }
    let total_bytes = diag.total_flags_bytes
        + diag.total_literals_bytes
        + diag.total_lengths_bytes
        + diag.total_slots_bytes
        + diag.total_extra_bits_bytes
        + diag.total_rep_indices_bytes
        + diag.total_table_bytes
        + diag.total_sub_block_headers_bytes;

    let flags_pct = if total_bytes > 0 {
        (diag.total_flags_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let literals_pct = if total_bytes > 0 {
        (diag.total_literals_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let lengths_pct = if total_bytes > 0 {
        (diag.total_lengths_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let distance_pct = if total_bytes > 0 {
        ((diag.total_slots_bytes + diag.total_extra_bits_bytes) as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let rep_indices_pct = if total_bytes > 0 {
        (diag.total_rep_indices_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let tables_pct = if total_bytes > 0 {
        (diag.total_table_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };
    let headers_pct = if total_bytes > 0 {
        (diag.total_sub_block_headers_bytes as f64 / total_bytes as f64) * 100.0
    } else {
        0.0
    };

    let avg_literals = if diag.num_sub_blocks > 0 {
        diag.total_literals_count as f64 / diag.num_sub_blocks as f64
    } else {
        0.0
    };
    let avg_matches = if diag.num_sub_blocks > 0 {
        diag.total_matches_count as f64 / diag.num_sub_blocks as f64
    } else {
        0.0
    };
    let avg_rep_matches = if diag.num_sub_blocks > 0 {
        diag.total_rep_matches_count as f64 / diag.num_sub_blocks as f64
    } else {
        0.0
    };

    println!("   === FLUX STREAM ANALYSIS ===");
    println!("   Total compressed size:        {} bytes", total_bytes);
    println!(
        "   Flags stream:        {} bytes ({:.2}%)",
        diag.total_flags_bytes, flags_pct
    );
    println!(
        "   Literals stream:     {} bytes ({:.2}%)",
        diag.total_literals_bytes, literals_pct
    );
    println!(
        "   Lengths stream:      {} bytes ({:.2}%)",
        diag.total_lengths_bytes, lengths_pct
    );
    println!(
        "   Distance stream:     {} bytes ({:.2}%)",
        diag.total_slots_bytes + diag.total_extra_bits_bytes,
        distance_pct
    );
    println!(
        "   Rep indices stream:  {} bytes ({:.2}%)",
        diag.total_rep_indices_bytes, rep_indices_pct
    );
    println!(
        "   Frequency tables:    {} bytes ({:.2}%)   <- table overhead",
        diag.total_table_bytes, tables_pct
    );
    println!(
        "   Sub-block headers:   {} bytes ({:.2}%)",
        diag.total_sub_block_headers_bytes, headers_pct
    );
    println!("   Number of sub-blocks: {}", diag.num_sub_blocks);
    println!("   Avg literals per block: {:.2}", avg_literals);
    println!("   Avg matches per block:  {:.2}", avg_matches);
    println!("   Avg rep matches per block: {:.2}", avg_rep_matches);
    println!("   ============================");

    let (lit_h0, lit_h1) = compute_entropies(&diag.literals_raw);
    let (len_h0, len_h1) = compute_entropies(&diag.lengths_raw);
    let (dist_h0, dist_h1) = compute_entropies(&diag.slots_raw);
    let (rep_h0, rep_h1) = compute_entropies(&diag.rep_indices_raw);

    let lit_theoretical = (diag.literals_raw.len() as f64 * lit_h0 / 8.0).round() as usize;
    let len_theoretical = (diag.lengths_raw.len() as f64 * len_h0 / 8.0).round() as usize;
    let dist_theoretical = ((diag.slots_raw.len() as f64 * dist_h0 / 8.0)
        + diag.total_extra_bits_bytes as f64)
        .round() as usize;
    let rep_theoretical = (diag.rep_indices_raw.len() as f64 * rep_h0 / 8.0).round() as usize;

    let lit_actual = diag.total_literals_bytes;
    let len_actual = diag.total_lengths_bytes;
    let dist_actual = diag.total_slots_bytes + diag.total_extra_bits_bytes;
    let rep_actual = diag.total_rep_indices_bytes;

    let lit_eff = if lit_actual > 0 {
        (lit_theoretical as f64 / lit_actual as f64) * 100.0
    } else {
        0.0
    };
    let len_eff = if len_actual > 0 {
        (len_theoretical as f64 / len_actual as f64) * 100.0
    } else {
        0.0
    };
    let dist_eff = if dist_actual > 0 {
        (dist_theoretical as f64 / dist_actual as f64) * 100.0
    } else {
        0.0
    };
    let rep_eff = if rep_actual > 0 {
        (rep_theoretical as f64 / rep_actual as f64) * 100.0
    } else {
        0.0
    };

    println!();
    println!("   Stream      Actual    Theoretical(order-0)   Efficiency");
    println!("   ──────────────────────────────────────────────────────");
    println!(
        "   Literals    {:5} bytes   {:5} bytes                {:5.1}%",
        lit_actual, lit_theoretical, lit_eff
    );
    println!(
        "   Lengths     {:5} bytes   {:5} bytes                {:5.1}%",
        len_actual, len_theoretical, len_eff
    );
    println!(
        "   Distances   {:5} bytes   {:5} bytes                {:5.1}%",
        dist_actual, dist_theoretical, dist_eff
    );
    println!(
        "   Rep Codes   {:5} bytes   {:5} bytes                {:5.1}%",
        rep_actual, rep_theoretical, rep_eff
    );
    println!();

    let lit_savings = if lit_h0 > 0.0 {
        (lit_h0 - lit_h1).max(0.0) / lit_h0 * 100.0
    } else {
        0.0
    };
    println!("   Literals order-0 entropy: {:.4} bits/byte", lit_h0);
    println!("   Literals order-1 entropy: {:.4} bits/byte", lit_h1);
    println!(
        "   Potential savings from order-1 modeling: {:.2}%",
        lit_savings
    );
    println!();

    let len_savings = if len_h0 > 0.0 {
        (len_h0 - len_h1).max(0.0) / len_h0 * 100.0
    } else {
        0.0
    };
    println!("   Lengths order-0 entropy: {:.4} bits/byte", len_h0);
    println!("   Lengths order-1 entropy: {:.4} bits/byte", len_h1);
    println!(
        "   Potential savings from order-1 modeling: {:.2}%",
        len_savings
    );
    println!();

    let dist_savings = if dist_h0 > 0.0 {
        (dist_h0 - dist_h1).max(0.0) / dist_h0 * 100.0
    } else {
        0.0
    };
    println!("   Distances order-0 entropy: {:.4} bits/byte", dist_h0);
    println!("   Distances order-1 entropy: {:.4} bits/byte", dist_h1);
    println!(
        "   Potential savings from order-1 modeling: {:.2}%",
        dist_savings
    );
    println!();

    let rep_savings = if rep_h0 > 0.0 {
        (rep_h0 - rep_h1).max(0.0) / rep_h0 * 100.0
    } else {
        0.0
    };
    println!("   Rep Codes order-0 entropy: {:.4} bits/byte", rep_h0);
    println!("   Rep Codes order-1 entropy: {:.4} bits/byte", rep_h1);
    println!(
        "   Potential savings from order-1 modeling: {:.2}%",
        rep_savings
    );
    println!("   ============================");
}

struct TwoBitWriter {
    bytes: Vec<u8>,
    current_byte: u8,
    shift: u8,
}

impl TwoBitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current_byte: 0,
            shift: 0,
        }
    }

    fn write(&mut self, val: u8) {
        self.current_byte |= (val & 3) << self.shift;
        self.shift += 2;
        if self.shift == 8 {
            self.bytes.push(self.current_byte);
            self.current_byte = 0;
            self.shift = 0;
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.shift > 0 {
            self.bytes.push(self.current_byte);
        }
        self.bytes
    }
}

struct TwoBitReader<'a> {
    bytes: &'a [u8],
    byte_idx: usize,
    shift: u8,
}

impl<'a> TwoBitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            byte_idx: 0,
            shift: 0,
        }
    }

    fn read(&mut self) -> Result<u8, ArchiveError> {
        if self.byte_idx >= self.bytes.len() {
            return Err(ArchiveError::CorruptIndex);
        }
        let val = (self.bytes[self.byte_idx] >> self.shift) & 3;
        self.shift += 2;
        if self.shift == 8 {
            self.byte_idx += 1;
            self.shift = 0;
        }
        Ok(val)
    }
}

fn serialize_lz77_tokens(
    tokens: &[Lz77Token],
    media_filter: &transform::MediaFilterType,
    data: &[u8],
    ppm_applied: bool,
    ppm_arena_size: usize,
) -> (Vec<u8>, StreamDiagnostics) {
    let num_tokens = tokens.len() as u32;
    let mut literals_stream = Vec::new();
    let mut lengths_stream = Vec::new();
    let mut slots_stream = Vec::new();
    let mut rep_indices_stream = Vec::new();

    struct BitWriter {
        bytes: Vec<u8>,
        current_byte: u8,
        bit_count: u8,
    }

    impl BitWriter {
        fn new() -> Self {
            Self {
                bytes: Vec::new(),
                current_byte: 0,
                bit_count: 0,
            }
        }

        fn write_bits(&mut self, value: u32, count: u8) {
            for i in 0..count {
                let bit = ((value >> i) & 1) as u8;
                self.current_byte |= bit << self.bit_count;
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.bytes.push(self.current_byte);
                    self.current_byte = 0;
                    self.bit_count = 0;
                }
            }
        }

        fn finish(mut self) -> Vec<u8> {
            if self.bit_count > 0 {
                self.bytes.push(self.current_byte);
            }
            self.bytes
        }
    }

    let mut extra_writer = BitWriter::new();
    let mut flags_writer = TwoBitWriter::new();
    let min_match_map = crate::compress::lz77::build_min_match_map(media_filter, data.len());
    let mut pos = 0;

    for token in tokens {
        match token {
            Lz77Token::Literal(b) => {
                flags_writer.write(0);
                literals_stream.push(*b);
                pos += 1;
            }
            Lz77Token::Match { distance, length } => {
                flags_writer.write(1);
                let min_match = if pos < min_match_map.len() {
                    min_match_map[pos] as u16
                } else {
                    4u16
                };
                let len_val = (length.saturating_sub(min_match)).min(255) as u8;
                lengths_stream.push(len_val);

                let (slot, extra_val, extra_bits) =
                    crate::compress::lz77::distance_to_slot(*distance);
                slots_stream.push(slot);
                extra_writer.write_bits(extra_val, extra_bits);
                pos += *length as usize;
            }
            Lz77Token::RepMatch { index, length } => {
                flags_writer.write(2);
                let min_match = if pos < min_match_map.len() {
                    min_match_map[pos] as u16
                } else {
                    4u16
                };
                let len_val = (length.saturating_sub(min_match)).min(255) as u8;
                lengths_stream.push(len_val);
                rep_indices_stream.push(*index);
                pos += *length as usize;
            }
        }
    }
    let flags_stream = flags_writer.finish();
    let extra_bits_bytes = extra_writer.finish();

    let freq_start = std::time::Instant::now();
    let flags_table = build_probability_table_for_stream(&flags_stream);
    let literals_table = build_probability_table_for_stream(&literals_stream);
    let lengths_table = build_probability_table_for_stream(&lengths_stream);
    let slots_table = build_probability_table_for_stream(&slots_stream);
    let rep_indices_table = build_probability_table_for_stream(&rep_indices_stream);
    add_freq_table_time(freq_start.elapsed().as_secs_f64());

    let flags_table_bytes = serialize_frequency_table(&flags_table);
    let literals_table_bytes = if ppm_applied {
        Vec::new()
    } else {
        serialize_frequency_table(&literals_table)
    };
    let lengths_table_bytes = serialize_frequency_table(&lengths_table);
    let slots_table_bytes = serialize_frequency_table(&slots_table);
    let rep_indices_table_bytes = serialize_frequency_table(&rep_indices_table);

    let rans_start = std::time::Instant::now();
    let flags_encoded = rans_encode_stream(&flags_stream, &flags_table);

    let literals_encoded = if ppm_applied {
        let mut ppm = crate::compress::ppm::PpmModel::new(ppm_arena_size);
        let mut ppm_events = Vec::new();
        let mut pos = 0;
        for token in tokens {
            match token {
                Lz77Token::Literal(b) => {
                    ppm.encode_literal(*b, &mut ppm_events);
                    pos += 1;
                }
                Lz77Token::Match { length, .. } | Lz77Token::RepMatch { length, .. } => {
                    let len = *length as usize;
                    for i in 0..len {
                        ppm.update(data[pos + i]);
                    }
                    pos += len;
                }
            }
        }
        let mut encoder = crate::compress::rans::RansEncoder::new();
        for ev in ppm_events.iter().rev() {
            encoder.encode_symbol_with_freq(ev.freq, ev.cumfreq);
        }
        encoder.flush()
    } else {
        rans_encode_stream(&literals_stream, &literals_table)
    };

    let lengths_encoded = rans_encode_stream(&lengths_stream, &lengths_table);
    let slots_encoded = rans_encode_stream(&slots_stream, &slots_table);
    let rep_indices_encoded = rans_encode_stream(&rep_indices_stream, &rep_indices_table);
    add_rans_time(rans_start.elapsed().as_secs_f64());

    let mut output = Vec::new();
    output.extend_from_slice(&num_tokens.to_le_bytes());
    output.extend_from_slice(&(flags_encoded.len() as u32).to_le_bytes());
    output.extend_from_slice(&(literals_encoded.len() as u32).to_le_bytes());
    output.extend_from_slice(&(lengths_encoded.len() as u32).to_le_bytes());
    output.extend_from_slice(&(slots_encoded.len() as u32).to_le_bytes());
    output.extend_from_slice(&(rep_indices_encoded.len() as u32).to_le_bytes());

    output.extend_from_slice(&flags_table_bytes);
    if !ppm_applied {
        output.extend_from_slice(&literals_table_bytes);
    }
    output.extend_from_slice(&lengths_table_bytes);
    output.extend_from_slice(&slots_table_bytes);
    output.extend_from_slice(&rep_indices_table_bytes);

    output.extend_from_slice(&flags_encoded);
    output.extend_from_slice(&literals_encoded);
    output.extend_from_slice(&lengths_encoded);
    output.extend_from_slice(&slots_encoded);
    output.extend_from_slice(&rep_indices_encoded);

    output.extend_from_slice(&extra_bits_bytes);

    let diag = StreamDiagnostics {
        total_flags_bytes: flags_encoded.len(),
        total_literals_bytes: literals_encoded.len(),
        total_lengths_bytes: lengths_encoded.len(),
        total_slots_bytes: slots_encoded.len(),
        total_extra_bits_bytes: extra_bits_bytes.len(),
        total_rep_indices_bytes: rep_indices_encoded.len(),
        total_table_bytes: flags_table_bytes.len()
            + literals_table_bytes.len()
            + lengths_table_bytes.len()
            + slots_table_bytes.len()
            + rep_indices_table_bytes.len(),
        total_sub_block_headers_bytes: 21 + 24,
        num_sub_blocks: 1,
        total_literals_count: literals_stream.len(),
        total_matches_count: slots_stream.len(),
        total_rep_matches_count: rep_indices_stream.len(),
        literals_raw: literals_stream,
        lengths_raw: lengths_stream,
        slots_raw: slots_stream,
        rep_indices_raw: rep_indices_stream,
    };

    (output, diag)
}

fn deserialize_lz77_tokens(
    data: &[u8],
    transform_stack: &transform::TransformStack,
    uncompressed_size: usize,
) -> Result<Vec<Lz77Token>, ArchiveError> {
    if data.len() < 24 {
        flux_debug!("[LZ77 DECODE FAIL] data too short ({} < 24)", data.len());
        return Err(ArchiveError::CorruptIndex);
    }
    let num_tokens = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let flags_encoded_len = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
    let literals_encoded_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let lengths_encoded_len = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let distance_slots_encoded_len = u32::from_le_bytes(data[16..20].try_into().unwrap()) as usize;
    let rep_indices_encoded_len = u32::from_le_bytes(data[20..24].try_into().unwrap()) as usize;

    let mut pos_offset = 24;
    let expected_table_overhead = if transform_stack.ppm_applied {
        2048
    } else {
        2560
    };
    if data.len() < pos_offset + expected_table_overhead {
        flux_debug!(
            "[LZ77 DECODE FAIL] data too short for tables ({} < {}), offsets: flags={}, lits={}, lens={}, slots={}, reps={}",
            data.len(),
            pos_offset + expected_table_overhead,
            flags_encoded_len,
            literals_encoded_len,
            lengths_encoded_len,
            distance_slots_encoded_len,
            rep_indices_encoded_len
        );
        return Err(ArchiveError::CorruptIndex);
    }

    let (flags_table, _) = deserialize_frequency_table(&data[pos_offset..pos_offset + 512])?;
    pos_offset += 512;
    let literals_table = if !transform_stack.ppm_applied {
        let (table, _) = deserialize_frequency_table(&data[pos_offset..pos_offset + 512])?;
        pos_offset += 512;
        table
    } else {
        crate::compress::rans::ProbabilityTable {
            symbols: Vec::new(),
            alias_table: Vec::new(),
        }
    };
    let (lengths_table, _) = deserialize_frequency_table(&data[pos_offset..pos_offset + 512])?;
    pos_offset += 512;
    let (slots_table, _) = deserialize_frequency_table(&data[pos_offset..pos_offset + 512])?;
    pos_offset += 512;
    let (rep_indices_table, _) = deserialize_frequency_table(&data[pos_offset..pos_offset + 512])?;
    pos_offset += 512;

    if data.len()
        < pos_offset
            + flags_encoded_len
            + literals_encoded_len
            + lengths_encoded_len
            + distance_slots_encoded_len
            + rep_indices_encoded_len
    {
        flux_debug!(
            "[LZ77 DECODE FAIL] data too short for streams ({} < {}), offsets: flags={}, lits={}, lens={}, slots={}, reps={}",
            data.len(),
            pos_offset + flags_encoded_len + literals_encoded_len + lengths_encoded_len + distance_slots_encoded_len + rep_indices_encoded_len,
            flags_encoded_len,
            literals_encoded_len,
            lengths_encoded_len,
            distance_slots_encoded_len,
            rep_indices_encoded_len
        );
        return Err(ArchiveError::CorruptIndex);
    }

    let flags_encoded = &data[pos_offset..pos_offset + flags_encoded_len];
    pos_offset += flags_encoded_len;
    let literals_encoded = &data[pos_offset..pos_offset + literals_encoded_len];
    pos_offset += literals_encoded_len;
    let lengths_encoded = &data[pos_offset..pos_offset + lengths_encoded_len];
    pos_offset += lengths_encoded_len;
    let slots_encoded = &data[pos_offset..pos_offset + distance_slots_encoded_len];
    pos_offset += distance_slots_encoded_len;
    let rep_indices_encoded = &data[pos_offset..pos_offset + rep_indices_encoded_len];
    pos_offset += rep_indices_encoded_len;

    let extra_bits_bytes = &data[pos_offset..];

    let num_flag_bytes = num_tokens.div_ceil(4);
    let flags_stream =
        rans_decode_stream(flags_encoded, num_flag_bytes, &flags_table).map_err(|e| {
            flux_debug!(
                "[LZ77 DECODE FAIL] rans_decode_stream flags failed: {:?}",
                e
            );
            e
        })?;

    let mut num_literals = 0;
    let mut num_matches = 0;
    let mut num_rep_matches = 0;

    let mut flags_reader = TwoBitReader::new(&flags_stream);
    for _ in 0..num_tokens {
        let flag = flags_reader.read().map_err(|e| {
            flux_debug!("[LZ77 DECODE FAIL] TwoBitReader read flag failed: {:?}", e);
            e
        })?;
        match flag {
            0 => num_literals += 1,
            1 => num_matches += 1,
            2 => num_rep_matches += 1,
            _ => {
                flux_debug!("[LZ77 DECODE FAIL] invalid flag value: {}", flag);
                return Err(ArchiveError::CorruptIndex);
            }
        }
    }

    let literals_stream = if !transform_stack.ppm_applied {
        rans_decode_stream(literals_encoded, num_literals, &literals_table)?
    } else {
        Vec::new()
    };
    let lengths_stream = rans_decode_stream(
        lengths_encoded,
        num_matches + num_rep_matches,
        &lengths_table,
    )
    .map_err(|e| {
        flux_debug!(
            "[LZ77 DECODE FAIL] rans_decode_stream lengths failed: {:?}",
            e
        );
        e
    })?;
    let slots_stream =
        rans_decode_stream(slots_encoded, num_matches, &slots_table).map_err(|e| {
            flux_debug!(
                "[LZ77 DECODE FAIL] rans_decode_stream slots failed: {:?}",
                e
            );
            e
        })?;
    let rep_indices_stream =
        rans_decode_stream(rep_indices_encoded, num_rep_matches, &rep_indices_table).map_err(
            |e| {
                flux_debug!(
                    "[LZ77 DECODE FAIL] rans_decode_stream rep_indices failed: {:?}",
                    e
                );
                e
            },
        )?;

    let mut tokens = Vec::with_capacity(num_tokens);
    let mut lit_idx = 0;
    let mut match_idx = 0;
    let mut rep_match_idx = 0;
    let mut len_idx = 0;

    struct BitReader<'a> {
        bytes: &'a [u8],
        byte_idx: usize,
        bit_idx: u8,
    }

    impl<'a> BitReader<'a> {
        fn new(bytes: &'a [u8]) -> Self {
            Self {
                bytes,
                byte_idx: 0,
                bit_idx: 0,
            }
        }

        fn read_bits(&mut self, count: u8) -> Result<u32, ArchiveError> {
            let mut value = 0u32;
            for i in 0..count {
                if self.byte_idx >= self.bytes.len() {
                    return Err(ArchiveError::CorruptIndex);
                }
                let bit = (self.bytes[self.byte_idx] >> self.bit_idx) & 1;
                value |= (bit as u32) << i;
                self.bit_idx += 1;
                if self.bit_idx == 8 {
                    self.byte_idx += 1;
                    self.bit_idx = 0;
                }
            }
            Ok(value)
        }
    }

    let mut extra_reader = BitReader::new(extra_bits_bytes);
    let mut flags_reader = TwoBitReader::new(&flags_stream);
    let mut rep_offsets = [1u32, 4u32, 8u32];
    let min_match_map = crate::compress::lz77::build_min_match_map(
        &transform_stack.media_filter_type,
        uncompressed_size,
    );
    let mut pos = 0;

    let mut literals_decoder = if transform_stack.ppm_applied {
        if literals_encoded.is_empty() {
            if num_literals > 0 {
                return Err(ArchiveError::CorruptIndex);
            }
            None
        } else {
            if literals_encoded.len() < 8 {
                return Err(ArchiveError::CorruptIndex);
            }
            Some(crate::compress::rans::RansDecoder::new(
                literals_encoded.to_vec(),
            ))
        }
    } else {
        None
    };
    let mut ppm = if transform_stack.ppm_applied {
        Some(crate::compress::ppm::PpmModel::new(
            transform_stack.ppm_arena_size,
        ))
    } else {
        None
    };
    let mut decompressed_bytes = if transform_stack.ppm_applied {
        Vec::with_capacity(uncompressed_size)
    } else {
        Vec::new()
    };

    for _ in 0..num_tokens {
        let flag = flags_reader.read()?;
        let min_match = if pos < min_match_map.len() {
            min_match_map[pos] as u16
        } else {
            4u16
        };
        match flag {
            0 => {
                let b = if transform_stack.ppm_applied {
                    let dec = literals_decoder
                        .as_mut()
                        .ok_or(ArchiveError::CorruptIndex)?;
                    let p = ppm.as_mut().ok_or(ArchiveError::CorruptIndex)?;
                    let val = p.decode_literal(dec)?;
                    decompressed_bytes.push(val);
                    val
                } else {
                    if lit_idx >= literals_stream.len() {
                        return Err(ArchiveError::CorruptIndex);
                    }
                    let val = literals_stream[lit_idx];
                    lit_idx += 1;
                    val
                };
                tokens.push(Lz77Token::Literal(b));
                pos += 1;
            }
            1 => {
                if len_idx >= lengths_stream.len() || match_idx >= slots_stream.len() {
                    return Err(ArchiveError::CorruptIndex);
                }
                let len_offset = lengths_stream[len_idx];
                len_idx += 1;
                let slot = slots_stream[match_idx];
                match_idx += 1;

                let length = len_offset as u16 + min_match;

                let extra_bits = if slot <= 3 { 0 } else { (slot - 2) / 2 };

                let extra_val = extra_reader.read_bits(extra_bits)?;
                let distance = crate::compress::lz77::slot_to_distance(slot, extra_val);

                rep_offsets[2] = rep_offsets[1];
                rep_offsets[1] = rep_offsets[0];
                rep_offsets[0] = distance;

                if transform_stack.ppm_applied {
                    let dist = distance as usize;
                    let len = length as usize;
                    if dist > decompressed_bytes.len() {
                        return Err(ArchiveError::CorruptIndex);
                    }
                    let start = decompressed_bytes.len() - dist;
                    let p = ppm.as_mut().ok_or(ArchiveError::CorruptIndex)?;
                    for j in 0..len {
                        let b = decompressed_bytes[start + j];
                        p.update(b);
                        decompressed_bytes.push(b);
                    }
                }

                tokens.push(Lz77Token::Match { distance, length });
                pos += length as usize;
            }
            2 => {
                if len_idx >= lengths_stream.len() || rep_match_idx >= rep_indices_stream.len() {
                    return Err(ArchiveError::CorruptIndex);
                }
                let len_offset = lengths_stream[len_idx];
                len_idx += 1;
                let idx = rep_indices_stream[rep_match_idx] as usize;
                rep_match_idx += 1;

                if idx >= 3 {
                    return Err(ArchiveError::CorruptIndex);
                }
                let distance = rep_offsets[idx];
                let length = len_offset as u16 + min_match;

                let d = rep_offsets[idx];
                if idx == 1 {
                    let tmp = rep_offsets[0];
                    rep_offsets[0] = d;
                    rep_offsets[1] = tmp;
                } else if idx == 2 {
                    let tmp0 = rep_offsets[0];
                    let tmp1 = rep_offsets[1];
                    rep_offsets[0] = d;
                    rep_offsets[1] = tmp0;
                    rep_offsets[2] = tmp1;
                }

                if transform_stack.ppm_applied {
                    let dist = distance as usize;
                    let len = length as usize;
                    if dist > decompressed_bytes.len() {
                        return Err(ArchiveError::CorruptIndex);
                    }
                    let start = decompressed_bytes.len() - dist;
                    let p = ppm.as_mut().ok_or(ArchiveError::CorruptIndex)?;
                    for j in 0..len {
                        let b = decompressed_bytes[start + j];
                        p.update(b);
                        decompressed_bytes.push(b);
                    }
                }

                tokens.push(Lz77Token::Match { distance, length });
                pos += length as usize;
            }
            _ => return Err(ArchiveError::CorruptIndex),
        }
    }

    Ok(tokens)
}

// =========================================================================
// Cryptography / Payload Packaging helpers
// =========================================================================

fn serialize_encrypted_chunks(chunks: &[crate::crypto::stream::EncryptedChunk]) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    for chunk in chunks {
        buf.extend_from_slice(&chunk.chunk_index.to_le_bytes());
        buf.extend_from_slice(&chunk.iv.0);
        buf.extend_from_slice(&chunk.auth_tag.0);
        buf.extend_from_slice(&(chunk.ciphertext.len() as u32).to_le_bytes());
        buf.extend_from_slice(&chunk.ciphertext);
    }
    buf
}

fn deserialize_encrypted_chunks(
    data: &[u8],
) -> Result<(Vec<crate::crypto::stream::EncryptedChunk>, &[u8]), String> {
    if data.len() < 4 {
        return Err("Data too short for chunk count".to_string());
    }
    let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    let mut pos = 4;
    let mut chunks = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 8 + 12 + 16 + 4 > data.len() {
            return Err("Data too short for chunk header".to_string());
        }
        let chunk_index = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let mut iv_bytes = [0u8; 12];
        iv_bytes.copy_from_slice(&data[pos..pos + 12]);
        pos += 12;
        let mut tag_bytes = [0u8; 16];
        tag_bytes.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        let ct_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + ct_len > data.len() {
            return Err("Data too short for chunk ciphertext".to_string());
        }
        let ciphertext = data[pos..pos + ct_len].to_vec();
        pos += ct_len;

        chunks.push(crate::crypto::stream::EncryptedChunk {
            iv: crate::crypto::Iv(iv_bytes),
            ciphertext,
            auth_tag: crate::crypto::AuthTag(tag_bytes),
            chunk_index,
        });
    }
    Ok((chunks, &data[pos..]))
}

fn encrypt_payload(
    data: &[u8],
    key: &Option<crate::crypto::EncryptionKey>,
) -> Result<Vec<u8>, ArchiveError> {
    if let Some(k) = key {
        let mut encryptor = crate::crypto::stream::StreamEncryptor::new(k.clone());
        let chunks = encryptor
            .encrypt_stream(data)
            .map_err(|e| ArchiveError::Io(e.to_string()))?;
        Ok(serialize_encrypted_chunks(&chunks))
    } else {
        Ok(data.to_vec())
    }
}

fn decrypt_payload(
    data: &[u8],
    key: &Option<crate::crypto::EncryptionKey>,
) -> Result<Vec<u8>, ArchiveError> {
    if let Some(k) = key {
        let mut decryptor = crate::crypto::stream::StreamDecryptor::new(k.clone());
        let (chunks, _) =
            deserialize_encrypted_chunks(data).map_err(|_| ArchiveError::DecryptionFailed)?;
        let decrypted = decryptor
            .decrypt_stream(&chunks)
            .map_err(|_| ArchiveError::DecryptionFailed)?;
        Ok(decrypted)
    } else {
        Ok(data.to_vec())
    }
}

// =========================================================================
// Directory tree serialization / deserialization helpers
// =========================================================================

fn serialize_directory_tree(tree: &crate::archive::DirectoryTree) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&(tree.root.to_string_lossy().len() as u32).to_le_bytes());
    buf.extend_from_slice(tree.root.to_string_lossy().as_bytes());
    buf.extend_from_slice(&(tree.entries.len() as u32).to_le_bytes());
    for entry in &tree.entries {
        let path_str = entry.path.to_string_lossy();
        let path_bytes = path_str.as_bytes();
        buf.extend_from_slice(&(path_bytes.len() as u32).to_le_bytes());
        buf.extend_from_slice(path_bytes);

        match &entry.entry_type {
            crate::archive::EntryType::RegularFile { size } => {
                buf.push(0);
                buf.extend_from_slice(&size.to_le_bytes());
            }
            crate::archive::EntryType::Directory => {
                buf.push(1);
            }
            crate::archive::EntryType::Symlink { target } => {
                buf.push(2);
                let target_str = target.to_string_lossy();
                let target_bytes = target_str.as_bytes();
                buf.extend_from_slice(&(target_bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(target_bytes);
            }
            crate::archive::EntryType::EmptyDirectory => {
                buf.push(3);
            }
        }

        buf.extend_from_slice(&entry.metadata.permissions.to_le_bytes());
        buf.extend_from_slice(&entry.metadata.modified_time.to_le_bytes());
        buf.extend_from_slice(&entry.metadata.created_time.to_le_bytes());
        buf.extend_from_slice(&entry.metadata.owner_uid.to_le_bytes());
        buf.extend_from_slice(&entry.metadata.owner_gid.to_le_bytes());
    }
    buf
}

fn deserialize_directory_tree(data: &[u8]) -> Result<crate::archive::DirectoryTree, String> {
    if data.len() < 8 {
        return Err("Data too short for DirectoryTree header".to_string());
    }
    let root_len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if 4 + root_len + 4 > data.len() {
        return Err("Data too short for root path".to_string());
    }
    let root_str = String::from_utf8(data[4..4 + root_len].to_vec()).map_err(|e| e.to_string())?;
    let root = PathBuf::from(root_str);

    let count =
        u32::from_le_bytes(data[4 + root_len..4 + root_len + 4].try_into().unwrap()) as usize;
    let mut pos = 4 + root_len + 4;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        if pos + 4 > data.len() {
            return Err("Data too short for entry path length".to_string());
        }
        let path_len = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + path_len > data.len() {
            return Err("Data too short for entry path".to_string());
        }
        let path_str =
            String::from_utf8(data[pos..pos + path_len].to_vec()).map_err(|e| e.to_string())?;
        pos += path_len;
        let path = PathBuf::from(path_str);

        if pos >= data.len() {
            return Err("Data too short for entry type".to_string());
        }
        let tag = data[pos];
        pos += 1;

        let entry_type = match tag {
            0 => {
                if pos + 8 > data.len() {
                    return Err("Data too short for file size".to_string());
                }
                let size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
                pos += 8;
                crate::archive::EntryType::RegularFile { size }
            }
            1 => crate::archive::EntryType::Directory,
            2 => {
                if pos + 4 > data.len() {
                    return Err("Data too short for symlink target length".to_string());
                }
                let target_len =
                    u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
                pos += 4;
                if pos + target_len > data.len() {
                    return Err("Data too short for symlink target".to_string());
                }
                let target_str = String::from_utf8(data[pos..pos + target_len].to_vec())
                    .map_err(|e| e.to_string())?;
                pos += target_len;
                crate::archive::EntryType::Symlink {
                    target: PathBuf::from(target_str),
                }
            }
            3 => crate::archive::EntryType::EmptyDirectory,
            _ => return Err("Invalid entry type tag".to_string()),
        };

        if pos + 28 > data.len() {
            return Err("Data too short for entry metadata".to_string());
        }
        let permissions = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let modified_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let created_time = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let owner_uid = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let owner_gid = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;

        entries.push(crate::archive::DirectoryEntry {
            path,
            entry_type,
            metadata: crate::archive::EntryMetadata {
                permissions,
                modified_time,
                created_time,
                owner_uid,
                owner_gid,
            },
        });
    }

    Ok(crate::archive::DirectoryTree { root, entries })
}

fn get_volume_path(base_path: &Path, vol_num: u16) -> PathBuf {
    let ext = format!("{:03}", vol_num);
    let mut path = base_path.to_path_buf();
    if let Some(existing_ext) = path.extension() {
        if existing_ext == "flx" {
            let filename = path.file_name().unwrap().to_string_lossy().into_owned();
            path.set_file_name(format!("{}.{}", filename, ext));
            return path;
        }
    }
    let filename = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    path.set_file_name(format!("{}.{}", filename, ext));
    path
}

struct MultiVolumeWriter {
    base_path: PathBuf,
    volume_size: u64,
    archive_id: [u8; 16],
    current_volume: u16,
    current_file: Option<File>,
    current_payload_written: u64,
    current_hasher: crc32fast::Hasher,
    volume_payloads: Vec<(u32, u64)>, // (payload_crc, payload_length)
}

impl MultiVolumeWriter {
    fn new(base_path: PathBuf, volume_size: u64, archive_id: [u8; 16]) -> Self {
        Self {
            base_path,
            volume_size,
            archive_id,
            current_volume: 1,
            current_file: None,
            current_payload_written: 0,
            current_hasher: crc32fast::Hasher::new(),
            volume_payloads: Vec::new(),
        }
    }

    fn write_payload(&mut self, buf: &[u8]) -> std::io::Result<()> {
        if let Some(ref mut file) = self.current_file {
            file.write_all(buf)?;
            self.current_payload_written += buf.len() as u64;
            self.current_hasher.update(buf);
            Ok(())
        } else {
            Err(std::io::Error::other("No active volume file"))
        }
    }

    fn start_block(&mut self, block_size: u64) -> std::io::Result<(u16, u64)> {
        if self.current_file.is_none() {
            let path = get_volume_path(&self.base_path, self.current_volume);
            let mut file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?;
            file.write_all(&[0u8; 50])?;
            self.current_file = Some(file);
            self.current_payload_written = 0;
            self.current_hasher = crc32fast::Hasher::new();
        } else if 50 + self.current_payload_written + block_size > self.volume_size {
            // Finalize current volume
            let len = self.current_payload_written;
            let crc = self.current_hasher.clone().finalize();
            self.volume_payloads.push((crc, len));

            let header = crate::crypto::header::VolumeHeader {
                magic: *b"FLXV",
                version_major: 1,
                version_minor: 3,
                volume_number: self.current_volume,
                total_volumes: 0, // Filled at the end
                archive_id: self.archive_id,
                volume_payload_crc32: crc,
                volume_payload_length: len,
                volume_index_offset: 0,
            };

            let mut file = self.current_file.take().unwrap();
            file.seek(SeekFrom::Start(0))?;
            file.write_all(&header.serialize())?;
            drop(file);

            // Start next volume
            self.current_volume += 1;
            let path = get_volume_path(&self.base_path, self.current_volume);
            let mut next_file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&path)?;
            next_file.write_all(&[0u8; 50])?;
            self.current_file = Some(next_file);
            self.current_payload_written = 0;
            self.current_hasher = crc32fast::Hasher::new();
        }

        Ok((self.current_volume, 50 + self.current_payload_written))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_compress_decompress_directory() {
        let temp_src = tempdir().unwrap();
        let temp_dest = tempdir().unwrap();
        let src_path = temp_src.path();
        let dest_path = temp_dest.path();

        // Create 1. Text file
        let txt_path = src_path.join("file1.txt");
        let txt_data = b"This is a repeated text block. This is a repeated text block. This is a repeated text block.";
        std::fs::write(&txt_path, txt_data).unwrap();

        // Create 2. Binary file
        let bin_path = src_path.join("file2.bin");
        let mut bin_data = vec![0u8; 1000];
        for (i, val) in bin_data.iter_mut().enumerate() {
            *val = (i % 256) as u8;
        }
        std::fs::write(&bin_path, &bin_data).unwrap();

        // Create 3. Subdirectory with a file
        let sub_dir = src_path.join("subdir");
        std::fs::create_dir(&sub_dir).unwrap();
        let sub_file_path = sub_dir.join("subfile.txt");
        let sub_file_data = b"Some content in a subdirectory file.";
        std::fs::write(&sub_file_path, sub_file_data).unwrap();

        // Create 4. Empty directory
        let empty_dir = src_path.join("empty_dir");
        std::fs::create_dir(&empty_dir).unwrap();

        // Compress
        let archive_path = temp_src.path().join("archive.flx");
        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: std::ptr::null(),
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };
        let mut compressor = FluxCompressor::new(options);
        let c_stats = compressor
            .compress_directory(src_path, &archive_path)
            .unwrap();
        assert!(c_stats.original_size > 0);
        assert!(c_stats.compressed_size > 0);
        assert_eq!(c_stats.files_processed, 3); // file1.txt, file2.bin, subfile.txt

        // Decompress
        let mut decompressor = FluxDecompressor::new(options);
        let d_stats = decompressor.decompress(&archive_path, dest_path).unwrap();
        assert_eq!(d_stats.files_extracted, 3);
        assert_eq!(d_stats.bytes_written, c_stats.original_size);
        assert!(d_stats.integrity_verified);

        // Verify file1.txt matches
        let dec_txt_data = std::fs::read(dest_path.join("file1.txt")).unwrap();
        assert_eq!(dec_txt_data, txt_data);

        // Verify file2.bin matches
        let dec_bin_data = std::fs::read(dest_path.join("file2.bin")).unwrap();
        assert_eq!(dec_bin_data, bin_data);

        // Verify subfile.txt matches
        let dec_sub_data = std::fs::read(dest_path.join("subdir/subfile.txt")).unwrap();
        assert_eq!(dec_sub_data, sub_file_data);

        // Verify empty_dir exists and is a directory
        assert!(dest_path.join("empty_dir").exists());
        assert!(dest_path.join("empty_dir").is_dir());
    }

    #[test]
    fn test_compress_decompress_directory_encrypted() {
        let temp_src = tempdir().unwrap();
        let temp_dest = tempdir().unwrap();
        let src_path = temp_src.path();
        let dest_path = temp_dest.path();

        let txt_path = src_path.join("secret.txt");
        let txt_data = b"This is classified top secret content.";
        std::fs::write(&txt_path, txt_data).unwrap();

        let archive_path = temp_src.path().join("archive.flx");
        let password = b"SecretPassword123\0";
        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: password.as_ptr() as *const std::os::raw::c_char,
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };

        // Compress encrypted
        let mut compressor = FluxCompressor::new(options);
        let c_stats = compressor
            .compress_directory(src_path, &archive_path)
            .unwrap();
        assert_eq!(c_stats.files_processed, 1);

        // Decompress encrypted
        let mut decompressor = FluxDecompressor::new(options);
        let d_stats = decompressor.decompress(&archive_path, dest_path).unwrap();
        assert_eq!(d_stats.files_extracted, 1);
        assert!(d_stats.integrity_verified);

        let dec_txt_data = std::fs::read(dest_path.join("secret.txt")).unwrap();
        assert_eq!(dec_txt_data, txt_data);
    }

    #[test]
    fn test_block_splitting_roundtrip() {
        let temp_src = tempdir().unwrap();
        let temp_dest = tempdir().unwrap();
        let src_path = temp_src.path();
        let dest_path = temp_dest.path();

        // Create a file larger than 1MB (e.g. 2.5MB)
        let large_path = src_path.join("large_text.txt");
        let pattern = b"This is a repeated sentence used to build a large test file that spans across multiple BWT compression blocks to verify sub-block splitting and concatenation. ";
        let mut txt_data = Vec::new();
        while txt_data.len() < 2_500_000 {
            txt_data.extend_from_slice(pattern);
        }
        std::fs::write(&large_path, &txt_data).unwrap();

        let archive_path = temp_src.path().join("archive_large.flx");
        let options = FluxOptions {
            level: FluxCompressionLevel::Balanced,
            password: std::ptr::null(),
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };
        let mut compressor = FluxCompressor::new(options);
        let c_stats = compressor
            .compress_directory(src_path, &archive_path)
            .unwrap();
        assert!(c_stats.original_size >= 2_500_000);
        assert_eq!(c_stats.files_processed, 1);

        // Decompress
        let mut decompressor = FluxDecompressor::new(options);
        let d_stats = decompressor.decompress(&archive_path, dest_path).unwrap();
        assert_eq!(d_stats.files_extracted, 1);
        assert_eq!(d_stats.bytes_written, c_stats.original_size);
        assert!(d_stats.integrity_verified);

        // Verify content matches exactly
        let dec_data = std::fs::read(dest_path.join("large_text.txt")).unwrap();
        assert_eq!(dec_data, txt_data);
    }

    fn get_bwt_applied_flags(archive_path: &Path) -> Vec<bool> {
        use std::io::Seek;
        let mut file = File::open(archive_path).unwrap();
        let file_len = file.metadata().unwrap().len();

        // Read PlaintextHeader base (96 bytes)
        let mut bootstrap_base = vec![0u8; 96];
        file.read_exact(&mut bootstrap_base).unwrap();
        let ct_len = u32::from_le_bytes(bootstrap_base[92..96].try_into().unwrap()) as usize;

        // Read full PlaintextHeader if there's any sentinel ciphertext
        let mut bootstrap_full = bootstrap_base;
        let mut ct_bytes = vec![0u8; ct_len];
        file.read_exact(&mut ct_bytes).unwrap();
        bootstrap_full.extend_from_slice(&ct_bytes);

        // Read CRC32 (4 bytes)
        let mut crc_bytes = [0u8; 4];
        file.read_exact(&mut crc_bytes).unwrap();
        bootstrap_full.extend_from_slice(&crc_bytes);

        let plaintext_header =
            crate::crypto::header::PlaintextHeader::deserialize(&bootstrap_full).unwrap();
        assert!(!plaintext_header.is_encrypted); // The test doesn't encrypt

        // Read EncryptedHeaderData (70 bytes, since is_encrypted is false)
        let mut header_data_bytes = vec![0u8; 70];
        file.read_exact(&mut header_data_bytes).unwrap();
        let encrypted_header =
            crate::crypto::header::EncryptedHeaderData::deserialize(&header_data_bytes).unwrap();

        // Read the entire body
        let archive_body_start = encrypted_header.file_index_offset;
        let archive_body_end = file_len - 36;
        file.seek(SeekFrom::Start(archive_body_start)).unwrap();
        let mut body_bytes = vec![0u8; (archive_body_end - archive_body_start) as usize];
        file.read_exact(&mut body_bytes).unwrap();

        // Parse solid blocks
        let mut pos = encrypted_header.file_index_size as usize;
        let mut bwt_flags = Vec::new();

        for _block_id in 0..encrypted_header.solid_block_count {
            // Read 66-byte block header
            let header_slice = &body_bytes[pos..pos + 66];
            pos += 66;

            let compressed_size =
                u64::from_le_bytes(header_slice[5..13].try_into().unwrap()) as usize;
            let block_payload = &body_bytes[pos..pos + compressed_size];
            pos += compressed_size;

            // Parse sub-blocks in payload
            let mut sub_pos = 0;
            while sub_pos < block_payload.len() {
                // Read sub-block header (21 bytes)
                let _sub_uncompressed_size =
                    u32::from_le_bytes(block_payload[sub_pos..sub_pos + 4].try_into().unwrap())
                        as usize;
                let sub_compressed_size =
                    u32::from_le_bytes(block_payload[sub_pos + 4..sub_pos + 8].try_into().unwrap())
                        as usize;

                // transform_stack is bytes 8..21 of sub-block header.
                // bwt_applied is byte index 4 of transform_stack, which is byte index 12 of sub-block header.
                let bwt_applied = (block_payload[sub_pos + 12] & 1) != 0;
                bwt_flags.push(bwt_applied);

                sub_pos += 21 + sub_compressed_size;
            }
        }
        bwt_flags
    }

    #[test]
    fn test_adaptive_bwt_decision() {
        use rand::SeedableRng;

        let temp_src = tempdir().unwrap();
        let src_path = temp_src.path();

        let mut bwt_friendly_data = Vec::new();
        let mut found_friendly = false;

        // Search for a candidate where BWT is strictly smaller
        let mut rng = rand::rngs::StdRng::seed_from_u64(42);
        for len in (1000..50000).step_by(1000) {
            let mut candidate = Vec::with_capacity(len);
            for i in 0..(len / 4) {
                candidate.push(b'a');
                let s = format!("{:03}", i);
                candidate.extend_from_slice(s.as_bytes());
            }
            while candidate.len() < len {
                candidate.push(b' ');
            }

            let mut ts_no_bwt = TransformStack::default();
            let trans_no_bwt =
                ts_no_bwt.apply(candidate.clone(), 3.0, &ContentType::Text, None, false);
            let (comp_no_bwt, _, _) = compress_block(
                &trans_no_bwt,
                archive::format::BlockType::Text,
                FluxCompressionLevel::Maximum,
                &transform::MediaFilterType::None,
            );

            let mut ts_with_bwt = TransformStack::default();
            let trans_with_bwt =
                ts_with_bwt.apply(candidate.clone(), 3.0, &ContentType::Text, None, true);
            let (comp_with_bwt, _, _) = compress_block(
                &trans_with_bwt,
                archive::format::BlockType::Text,
                FluxCompressionLevel::Maximum,
                &transform::MediaFilterType::None,
            );

            if comp_with_bwt.len() < comp_no_bwt.len() {
                bwt_friendly_data = candidate;
                found_friendly = true;
                println!(
                    "[TEST] Found friendly size: {}, no-BWT: {}, with-BWT: {}",
                    len,
                    comp_no_bwt.len(),
                    comp_with_bwt.len()
                );
                break;
            }
        }

        assert!(
            found_friendly,
            "Failed to find a BWT-friendly text size where BWT helps"
        );

        // 2. Text where BWT does not help (or is equal/larger)
        let mut bwt_unfriendly_data = Vec::new();
        let mut found_unfriendly = false;

        for len in (1000..50000).step_by(1000) {
            let mut candidate = vec![0u8; len];
            for byte in candidate.iter_mut() {
                *byte = (rng.next_u32() % 95 + 32) as u8;
            }

            let mut ts_no_bwt = TransformStack::default();
            let trans_no_bwt =
                ts_no_bwt.apply(candidate.clone(), 3.0, &ContentType::Text, None, false);
            let (comp_no_bwt, _, _) = compress_block(
                &trans_no_bwt,
                archive::format::BlockType::Text,
                FluxCompressionLevel::Maximum,
                &transform::MediaFilterType::None,
            );

            let mut ts_with_bwt = TransformStack::default();
            let trans_with_bwt =
                ts_with_bwt.apply(candidate.clone(), 3.0, &ContentType::Text, None, true);
            let (comp_with_bwt, _, _) = compress_block(
                &trans_with_bwt,
                archive::format::BlockType::Text,
                FluxCompressionLevel::Maximum,
                &transform::MediaFilterType::None,
            );

            if comp_with_bwt.len() >= comp_no_bwt.len() {
                bwt_unfriendly_data = candidate;
                found_unfriendly = true;
                println!(
                    "[TEST] Found unfriendly size: {}, no-BWT: {}, with-BWT: {}",
                    len,
                    comp_no_bwt.len(),
                    comp_with_bwt.len()
                );
                break;
            }
        }

        assert!(
            found_unfriendly,
            "Failed to find a BWT-unfriendly text size where BWT does not help"
        );

        // Write friendly and unfriendly files
        let path_friendly = src_path.join("friendly.txt");
        std::fs::write(&path_friendly, &bwt_friendly_data).unwrap();

        let path_unfriendly = src_path.join("unfriendly.txt");
        std::fs::write(&path_unfriendly, &bwt_unfriendly_data).unwrap();

        // Compress friendly with Maximum mode
        let archive_friendly = src_path.join("friendly.flx");
        let options_max = FluxOptions {
            level: FluxCompressionLevel::Maximum,
            password: std::ptr::null(),
            thread_count: 0,
            block_size: 0,
            volume_size: 0,
        };
        let mut compressor = FluxCompressor::new(options_max);
        compressor
            .compress_file(&path_friendly, &archive_friendly)
            .unwrap();

        // Compress unfriendly with Maximum mode
        let archive_unfriendly = src_path.join("unfriendly.flx");
        compressor
            .compress_file(&path_unfriendly, &archive_unfriendly)
            .unwrap();

        // Inspect BWT flags
        let friendly_flags = get_bwt_applied_flags(&archive_friendly);
        let unfriendly_flags = get_bwt_applied_flags(&archive_unfriendly);

        println!("Friendly BWT flags: {:?}", friendly_flags);
        println!("Unfriendly BWT flags: {:?}", unfriendly_flags);

        assert!(
            !friendly_flags.is_empty(),
            "Friendly archive should have sub-blocks"
        );
        assert!(
            !unfriendly_flags.is_empty(),
            "Unfriendly archive should have sub-blocks"
        );

        // The friendly text should keep BWT
        assert!(
            friendly_flags.iter().any(|&f| f),
            "BWT should be applied to BWT-friendly text"
        );

        // The unfriendly text should skip BWT
        assert!(
            unfriendly_flags.iter().all(|&f| !f),
            "BWT should be skipped for BWT-unfriendly text"
        );
    }

    #[test]
    fn test_stream_separated_roundtrip() {
        let test_cases = vec![
            // Case 1: Empty
            vec![],
            // Case 2: Literals only
            vec![
                Lz77Token::Literal(b'h'),
                Lz77Token::Literal(b'e'),
                Lz77Token::Literal(b'l'),
                Lz77Token::Literal(b'l'),
                Lz77Token::Literal(b'o'),
            ],
            // Case 3: Matches only
            vec![
                Lz77Token::Match {
                    distance: 1,
                    length: 10,
                },
                Lz77Token::Match {
                    distance: 300,
                    length: 258,
                },
                Lz77Token::Match {
                    distance: 32768,
                    length: 4,
                },
            ],
            // Case 4: Mixed
            vec![
                Lz77Token::Literal(b'a'),
                Lz77Token::Match {
                    distance: 10,
                    length: 15,
                },
                Lz77Token::Literal(b'b'),
                Lz77Token::Match {
                    distance: 5,
                    length: 5,
                },
                Lz77Token::Literal(b'c'),
                Lz77Token::Match {
                    distance: 30000,
                    length: 100,
                },
            ],
        ];

        let ts = transform::TransformStack::default();
        for tokens in test_cases {
            let (serialized, _) =
                serialize_lz77_tokens(&tokens, &ts.media_filter_type, &[0u8; 1000], false, 0);
            let deserialized = deserialize_lz77_tokens(&serialized, &ts, 1000).unwrap();
            assert_eq!(tokens, deserialized);
        }
    }

    #[test]
    fn test_coord_mismatch() {
        use crate::threads::signals::ContentType;
        use crate::transform::TransformStack;

        // Generate some coordinates_xyz data (1MB)
        let mut data = Vec::new();
        for i in 0..87381 {
            let t = i as f32 / 10000.0;
            let theta = t;
            let r = 10.0 + theta / 100.0;
            let x = 1000.0 + r * theta.cos();
            let y = 1000.0 + r * theta.sin();
            let z = 1000.0 + theta / 5.0;
            data.extend_from_slice(&x.to_le_bytes());
            data.extend_from_slice(&y.to_le_bytes());
            data.extend_from_slice(&z.to_le_bytes());
        }
        // data.len() = 87381 * 12 = 1048572. Pad to 1048576
        data.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(data.len(), 1048576);

        let mut ts = TransformStack {
            media_filter_applied: true,
            media_filter_type: transform::MediaFilterType::FloatChannelSplit {
                channels: 3,
                mask: 0,
            },
            ..Default::default()
        };
        let transformed = ts.apply(data.clone(), 3.0, &ContentType::Multimedia, None, false);

        let mut encoder = crate::compress::lz77::Lz77Encoder::new(true);
        let tokens = encoder.encode_with_media_filter(&transformed, &ts.media_filter_type);

        let (serialized, _) =
            serialize_lz77_tokens(&tokens, &ts.media_filter_type, &transformed, false, 0);
        let deserialized_tokens =
            deserialize_lz77_tokens(&serialized, &ts, transformed.len()).unwrap();

        // Decode original tokens
        let mut original_decoder = crate::compress::lz77::Lz77Decoder::new();
        let decoded_from_orig = original_decoder.decode(&tokens);

        // Decode deserialized tokens
        let mut dese_decoder = crate::compress::lz77::Lz77Decoder::new();
        let decoded_from_dese = dese_decoder.decode(&deserialized_tokens);

        assert_eq!(decoded_from_orig.len(), transformed.len());
        assert_eq!(decoded_from_dese.len(), transformed.len());
        assert_eq!(decoded_from_orig, transformed);
        assert_eq!(decoded_from_dese, transformed);
    }

    #[test]
    fn test_coord_full_file() {
        use crate::threads::signals::ContentType;
        use crate::transform::TransformStack;

        // Read 15MB coordinates_xyz data from disk
        let data = match std::fs::read("data/coordinates_xyz.bin")
            .or_else(|_| std::fs::read("../../data/coordinates_xyz.bin"))
        {
            Ok(d) => d,
            Err(_) => {
                println!("data/coordinates_xyz.bin not found, skipping test");
                return;
            }
        };

        let chunk_size = 1024 * 1024;
        let mut chunk_index = 0;
        let mut chunk_id = 0;

        while chunk_index < data.len() {
            let chunk_end = std::cmp::min(chunk_index + chunk_size, data.len());
            let chunk_data = data[chunk_index..chunk_end].to_vec();

            let stride = crate::FluxCompressor::detect_chunk_stride(&chunk_data);

            // Replicate block compression decision
            let (compressed_chunk, ts) = if let Some(s) = stride {
                if s % 4 == 0 {
                    // Try generic transpose
                    let mut ts_generic = TransformStack::default();
                    let transformed_generic = ts_generic.apply(
                        chunk_data.clone(),
                        3.0,
                        &ContentType::Multimedia,
                        stride,
                        false,
                    );
                    let (comp_generic, _, _) = compress_block(
                        &transformed_generic,
                        crate::archive::format::BlockType::Multimedia,
                        crate::FluxCompressionLevel::Balanced,
                        &ts_generic.media_filter_type,
                    );

                    // Try float channel split
                    let mut ts_float = TransformStack {
                        media_filter_applied: true,
                        media_filter_type: crate::transform::MediaFilterType::FloatChannelSplit {
                            channels: s / 4,
                            mask: 0,
                        },
                        ..Default::default()
                    };
                    let transformed_float = ts_float.apply(
                        chunk_data.clone(),
                        3.0,
                        &ContentType::Multimedia,
                        None,
                        false,
                    );
                    let (comp_float, _, _) = compress_block(
                        &transformed_float,
                        crate::archive::format::BlockType::Multimedia,
                        crate::FluxCompressionLevel::Balanced,
                        &ts_float.media_filter_type,
                    );

                    if comp_float.len() < comp_generic.len() {
                        println!("Chunk {}: chose FloatChannelSplit ({} bytes) over Generic Transpose ({} bytes)", chunk_id, comp_float.len(), comp_generic.len());
                        (comp_float, ts_float)
                    } else {
                        println!("Chunk {}: chose Generic Transpose ({} bytes) over FloatChannelSplit ({} bytes)", chunk_id, comp_generic.len(), comp_float.len());
                        (comp_generic, ts_generic)
                    }
                } else {
                    let mut ts_normal = TransformStack::default();
                    let transformed_normal = ts_normal.apply(
                        chunk_data.clone(),
                        3.0,
                        &ContentType::Multimedia,
                        stride,
                        false,
                    );
                    let (comp_normal, _, _) = compress_block(
                        &transformed_normal,
                        crate::archive::format::BlockType::Multimedia,
                        crate::FluxCompressionLevel::Balanced,
                        &ts_normal.media_filter_type,
                    );
                    (comp_normal, ts_normal)
                }
            } else {
                let mut ts_normal = TransformStack::default();
                let transformed_normal = ts_normal.apply(
                    chunk_data.clone(),
                    3.0,
                    &ContentType::Multimedia,
                    stride,
                    false,
                );
                let (comp_normal, _, _) = compress_block(
                    &transformed_normal,
                    crate::archive::format::BlockType::Multimedia,
                    crate::FluxCompressionLevel::Balanced,
                    &ts_normal.media_filter_type,
                );
                (comp_normal, ts_normal)
            };

            // Decompress chunk
            let decompressed_tokens =
                deserialize_lz77_tokens(&compressed_chunk, &ts, chunk_data.len());
            if let Err(e) = decompressed_tokens {
                panic!("Chunk {} failed to deserialize tokens: {:?}", chunk_id, e);
            }
            let tokens = decompressed_tokens.unwrap();

            let mut decoder = crate::compress::lz77::Lz77Decoder::new();
            let decoded = decoder.decode(&tokens);

            if decoded.len() != chunk_data.len() {
                println!(
                    "Chunk {} failed: decoded.len() = {}, expected = {}, stride = {:?}, ts = {:?}",
                    chunk_id,
                    decoded.len(),
                    chunk_data.len(),
                    stride,
                    ts
                );
                panic!("Decoded len mismatch!");
            }

            let restored = ts.reverse(decoded);
            assert_eq!(restored, chunk_data, "Chunk {} failed roundtrip!", chunk_id);

            chunk_index = chunk_end;
            chunk_id += 1;
        }
    }

    #[test]
    fn test_multi_level_roundtrips() {
        let temp_src = tempdir().unwrap();
        let src_path = temp_src.path().join("input.txt");
        let archive_path = temp_src.path().join("archive.flx");
        let dest_path = temp_src.path().join("out_dir");

        let content = b"FLUX multi-level compression testing. This text must be round-tripped successfully at Tiny, Fast, Balanced, Maximum, and Extreme levels. Let's make it relatively long with some repetition so that compression is exercised properly! FLUX multi-level compression testing. Repeat this sentence to make sure we have repetition to compress. ".repeat(10);
        std::fs::write(&src_path, &content).unwrap();

        let levels = [
            FluxCompressionLevel::Tiny,
            FluxCompressionLevel::Fast,
            FluxCompressionLevel::Balanced,
            FluxCompressionLevel::Maximum,
            FluxCompressionLevel::Extreme,
        ];

        for &level in &levels {
            if archive_path.exists() {
                let _ = std::fs::remove_file(&archive_path);
            }
            if dest_path.exists() {
                let _ = std::fs::remove_dir_all(&dest_path);
            }

            let options = FluxOptions {
                level,
                password: std::ptr::null(),
                thread_count: 0,
                block_size: 0,
                volume_size: 0,
            };

            // Compress
            let mut compressor = FluxCompressor::new(options);
            let c_stats = compressor.compress_file(&src_path, &archive_path).unwrap();
            assert_eq!(c_stats.original_size, content.len() as u64);

            // Read archive info to verify stored level/window size mapping
            let mut decompressor = FluxDecompressor::new(options);
            let (orig_sz, _comp_sz, file_count, encrypted) =
                decompressor.read_archive_info(&archive_path).unwrap();
            assert_eq!(orig_sz, content.len() as u64);
            assert_eq!(file_count, 1);
            assert!(!encrypted);

            // Decompress
            let d_stats = decompressor.decompress(&archive_path, &dest_path).unwrap();
            assert_eq!(d_stats.files_extracted, 1);
            assert_eq!(d_stats.bytes_written, content.len() as u64);
            assert!(d_stats.integrity_verified);

            let recovered = std::fs::read(dest_path.join("input.txt")).unwrap();
            assert_eq!(recovered, content, "Mismatch at level {:?}", level);
        }
    }

    #[test]
    fn test_bwt_ppm_roundtrip() {
        use crate::threads::signals::ContentType;
        let content = b"This is a repeated sentence. This is a repeated sentence. This is a repeated sentence. ".repeat(15000);
        let mut ts = transform::TransformStack::default();
        let transformed = ts.apply(content.clone(), 1.0, &ContentType::Text, None, true);
        assert!(ts.bwt_applied, "BWT should be applied");

        let mut lz77_enc = crate::compress::lz77::Lz77Encoder::new_with_params(
            128 * 1024 * 1024,
            true, // lazy_matching
            4096, // max_chain_depth
            258,  // lazy_match_threshold
            258,  // good_match
            crate::compress::lz77::MatchFinder::HashChain,
        );
        let tokens = lz77_enc.encode(&transformed);

        ts.ppm_applied = true;
        ts.ppm_arena_size = 32 * 1024 * 1024;
        let (compressed, _) = serialize_lz77_tokens(
            &tokens,
            &transform::MediaFilterType::None,
            &transformed,
            true,
            ts.ppm_arena_size,
        );

        let decompressed = decompress_block(
            &compressed,
            archive::format::BlockType::Text,
            transformed.len(),
            &ts,
        )
        .unwrap();
        assert_eq!(decompressed, transformed);

        let restored = ts.reverse(decompressed);
        assert_eq!(restored, content);
    }

    #[test]
    #[ignore = "diagnostic test: real_world_corpus.flx must be regenerated with current PPM code before enabling"]
    fn test_diagnose_corpus_block() {
        use crate::crypto::header::PlaintextHeader;

        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let workspace_root = manifest_dir.parent().unwrap().parent().unwrap();
        let archive_path = workspace_root.join("scratch/real_world_corpus.flx");
        let archive_bytes = std::fs::read(&archive_path).unwrap();

        let ct_len = u32::from_le_bytes(archive_bytes[92..96].try_into().unwrap()) as usize;
        let bootstrap_len = 96 + ct_len + 4;
        let header = PlaintextHeader::deserialize(&archive_bytes[..bootstrap_len]).unwrap();

        let header_data_len = if header.is_encrypted { 98 } else { 70 };
        let header_data_bytes = &archive_bytes[bootstrap_len..bootstrap_len + header_data_len];

        let encrypted_header = if header.is_encrypted {
            panic!("Encrypted header not supported in this test");
        } else {
            crate::crypto::header::EncryptedHeaderData::deserialize(header_data_bytes).unwrap()
        };

        let file_len = archive_bytes.len();
        let archive_body_start = encrypted_header.file_index_offset;
        let archive_body_end = file_len - 36;

        let body_bytes = &archive_bytes[archive_body_start as usize..archive_body_end as usize];
        let metadata_payload_bytes = &body_bytes[..encrypted_header.file_index_size as usize];

        let decrypted_metadata = decrypt_payload(metadata_payload_bytes, &None).unwrap();
        let (index, _) = FileIndex::deserialize_prefix(&decrypted_metadata).unwrap();

        // Find mod.rs entry
        let mod_entry = index
            .entries
            .iter()
            .find(|e| e.path.contains("components/mod.rs") || e.path.contains("components\\mod.rs"))
            .unwrap()
            .clone();
        let block_id_target = mod_entry.solid_block_id;
        println!("mod.rs block_id_target: {}", block_id_target);

        // Read original files in this solid block
        let mut block_entries: Vec<&crate::archive::format::FileEntry> = index
            .entries
            .iter()
            .filter(|e| e.solid_block_id == block_id_target)
            .collect();
        block_entries.sort_by_key(|e| e.block_offset);

        let mut original_concat = Vec::new();
        for entry in &block_entries {
            let path = workspace_root
                .join("scratch/real_world_corpus")
                .join(&entry.path);
            let data = std::fs::read(&path).unwrap();
            original_concat.extend_from_slice(&data);
        }

        // Find block payload in archive body
        let mut pos = encrypted_header.file_index_size as usize;
        let mut target_payload = Vec::new();
        let mut target_block_type = None;
        let mut target_uncompressed_size = 0;

        for _block_id in 0..encrypted_header.solid_block_count {
            let header_slice = &body_bytes[pos..pos + 66];
            pos += 66;

            let b_id = u32::from_le_bytes(header_slice[0..4].try_into().unwrap());
            let b_type_tag = header_slice[4];
            let compressed_size =
                u64::from_le_bytes(header_slice[5..13].try_into().unwrap()) as usize;
            let uncompressed_size =
                u64::from_le_bytes(header_slice[13..21].try_into().unwrap()) as usize;

            let block_type = match b_type_tag {
                0 => archive::format::BlockType::Text,
                1 => archive::format::BlockType::Binary,
                2 => archive::format::BlockType::Multimedia,
                3 => archive::format::BlockType::Mixed,
                4 => archive::format::BlockType::Raw,
                _ => panic!("invalid block type"),
            };

            if b_id == block_id_target {
                target_payload = body_bytes[pos..pos + compressed_size].to_vec();
                target_block_type = Some(block_type);
                target_uncompressed_size = uncompressed_size;
                break;
            }
            pos += compressed_size;
        }

        let block_type = target_block_type.unwrap();
        println!(
            "Target block type: {:?}, compressed size: {}, original size: {}",
            block_type,
            target_payload.len(),
            target_uncompressed_size
        );

        let decrypted_payload = target_payload; // plain text

        let mut restored_payload = Vec::new();
        let mut sub_pos = 0;
        let mut chunk_id = 0;

        while sub_pos < decrypted_payload.len() {
            let sub_uncompressed_size =
                u32::from_le_bytes(decrypted_payload[sub_pos..sub_pos + 4].try_into().unwrap())
                    as usize;
            let sub_compressed_size = u32::from_le_bytes(
                decrypted_payload[sub_pos + 4..sub_pos + 8]
                    .try_into()
                    .unwrap(),
            ) as usize;
            let (sub_transform_stack, _) = crate::transform::TransformStack::deserialize(
                &decrypted_payload[sub_pos + 8..sub_pos + 21],
            )
            .unwrap();
            println!(
                "Sub-block {}: uncompressed_size={}, compressed_size={}, ts={:?}",
                chunk_id, sub_uncompressed_size, sub_compressed_size, sub_transform_stack
            );
            sub_pos += 21;

            let sub_chunk_bytes = &decrypted_payload[sub_pos..sub_pos + sub_compressed_size];
            sub_pos += sub_compressed_size;

            let mut lz77_uncompressed_size = sub_uncompressed_size;
            if sub_transform_stack.bwt_applied {
                const CHUNK_LIMIT: usize = 1_048_576;
                if sub_uncompressed_size > CHUNK_LIMIT {
                    let num_chunks = sub_uncompressed_size.div_ceil(CHUNK_LIMIT);
                    lz77_uncompressed_size += 4 + 4 * num_chunks;
                }
            }

            let decompressed_chunk = decompress_block(
                sub_chunk_bytes,
                block_type,
                lz77_uncompressed_size,
                &sub_transform_stack,
            )
            .unwrap();
            let restored_chunk = sub_transform_stack.reverse(decompressed_chunk);
            restored_payload.extend_from_slice(&restored_chunk);
            chunk_id += 1;
        }

        assert_eq!(restored_payload, original_concat);
    }
}
