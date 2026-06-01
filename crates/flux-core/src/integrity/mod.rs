//! # Integrity Group
//!
//! ## Multi-Level Integrity Checking Strategy
//! FLUX implements a comprehensive, three-layer integrity verification system:
//! 1. **Per-File Checksums**: Each individual file has its uncompressed CRC32 and SHA-256
//!    recorded. This ensures that after decompression and optional transform reversal, the
//!    exact original file is recovered byte-for-byte.
//! 2. **Per-Block Checksums**: Each solid block stores a SHA-256 hash of its compressed/encrypted
//!    payload on disk. This allows verification of block integrity before loading it into
//!    decompression and decryption engines, adhering to the "authenticate-then-decrypt" design.
//! 3. **Per-Archive Checksums**: A final checksum (both CRC32 and SHA-256) of the archive's body,
//!    guaranteeing that the archive file has not been truncated or tampered with as a whole.
//!
//! ## Integrity Technologies
//! * **CRC32**: Used for fast error/corruption detection (e.g. bit-rots) with low computational overhead.
//! * **SHA-256**: Used for strong cryptographic tamper detection, preimage resistance, and authenticity.
//!
//! ## Parallel Computation Loop
//! To eliminate the I/O cost of integrity checking, FLUX computes checksums in a dedicated
//! background thread (`ParallelCrc32`) that reads uncompressed data from the circular buffer
//! in parallel with compression threads. Because it reads from the same memory-mapped/mirrored circular
//! buffer as compression, it incurs zero additional disk I/O overhead.
//!
//! ## Verification Order on Extraction
//! 1. **Archive Level**: Check the overall archive checksum first to detect truncation.
//! 2. **Block Level**: Verify each block's checksum before decrypting/decompressing it.
//! 3. **File Level**: Recompute and verify the CRC32 and SHA-256 of each reconstructed file before
//!    writing it to its final destination on disk.

pub mod crc32;
pub mod sha256;

pub use crc32::{Crc32Hasher, ParallelCrc32};
pub use sha256::{Sha256Hasher, ChecksumSet, MultiLevelIntegrity, IntegrityResult};
