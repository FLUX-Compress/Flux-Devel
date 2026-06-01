//! SHA-256 integrity verification
//!
//! Provides SHA-256 hash computation, checksum aggregation, and multi-level hierarchy verification.

use std::collections::HashMap;
use sha2::{Sha256, Digest};

/// Checksum set containing both CRC32 and SHA-256 hashes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ChecksumSet {
    /// 32-bit Cyclic Redundancy Check checksum.
    pub crc32: u32,
    /// 256-bit Secure Hash Algorithm signature.
    pub sha256: [u8; 32],
}

impl ChecksumSet {
    /// Computes both CRC32 and SHA-256 checksums in a single pass over the data.
    pub fn compute(data: &[u8]) -> Self {
        let mut crc_hasher = crc32fast::Hasher::new();
        let mut sha_hasher = Sha256::new();

        // Feed data to both hashers
        crc_hasher.update(data);
        sha_hasher.update(data);

        let crc32 = crc_hasher.finalize();
        let mut sha256 = [0u8; 32];
        sha256.copy_from_slice(&sha_hasher.finalize());

        Self { crc32, sha256 }
    }

    /// Recomputes checksums of the data and compares them with this ChecksumSet.
    pub fn verify(&self, data: &[u8]) -> bool {
        let computed = Self::compute(data);
        *self == computed
    }

    /// Formats the checksums into lowercase hexadecimal string representations.
    pub fn to_hex_strings(&self) -> (String, String) {
        let crc_hex = format!("{:08x}", self.crc32);
        let sha_hex = self.sha256.iter().map(|b| format!("{:02x}", b)).collect();
        (crc_hex, sha_hex)
    }
}

/// Accumulator for SHA-256 digests.
pub struct Sha256Hasher {
    hasher: Sha256,
    bytes_processed: u64,
}

impl Sha256Hasher {
    /// Creates a new `Sha256Hasher`.
    pub fn new() -> Self {
        Self {
            hasher: Sha256::new(),
            bytes_processed: 0,
        }
    }

    /// Feeds data into the running SHA-256 calculation.
    pub fn update(&mut self, data: &[u8]) {
        self.hasher.update(data);
        self.bytes_processed += data.len() as u64;
    }

    /// Consumes the hasher and returns the final 256-bit hash.
    pub fn finalize(self) -> [u8; 32] {
        let result = self.hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&result);
        out
    }

    /// Returns the total bytes processed by this hasher.
    pub fn bytes_processed(&self) -> u64 {
        self.bytes_processed
    }
}

impl Default for Sha256Hasher {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of an integrity verification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrityResult {
    /// Checksums matched expected values.
    Ok,
    /// CRC32 mismatch.
    CrcMismatch { expected: u32, actual: u32 },
    /// SHA-256 mismatch.
    Sha256Mismatch { expected: [u8; 32], actual: [u8; 32] },
    /// Checksum not found in records.
    NotFound,
}

/// Manages checksum records across files, solid blocks, and the whole archive.
#[derive(Debug, Clone, Default)]
pub struct MultiLevelIntegrity {
    /// File index to checksum mapping.
    pub file_checksums: HashMap<u64, ChecksumSet>,
    /// Block index to checksum mapping.
    pub block_checksums: HashMap<u32, ChecksumSet>,
    /// Overall archive checksum.
    pub archive_checksum: Option<ChecksumSet>,
}

impl MultiLevelIntegrity {
    /// Creates a new empty `MultiLevelIntegrity` manager.
    pub fn new() -> Self {
        Self {
            file_checksums: HashMap::new(),
            block_checksums: HashMap::new(),
            archive_checksum: None,
        }
    }

    /// Computes and records the checksum of a specific file.
    pub fn record_file(&mut self, file_index: u64, data: &[u8]) {
        let checksums = ChecksumSet::compute(data);
        self.file_checksums.insert(file_index, checksums);
    }

    /// Computes and records the checksum of a specific solid block.
    pub fn record_block(&mut self, block_index: u32, data: &[u8]) {
        let checksums = ChecksumSet::compute(data);
        self.block_checksums.insert(block_index, checksums);
    }

    /// Computes and finalizes the overall archive checksum.
    pub fn finalize_archive(&mut self, all_data: &[u8]) {
        let checksums = ChecksumSet::compute(all_data);
        self.archive_checksum = Some(checksums);
    }

    /// Verifies the recorded checksum of a file against a candidate buffer.
    pub fn verify_file(&self, file_index: u64, data: &[u8]) -> IntegrityResult {
        if let Some(expected) = self.file_checksums.get(&file_index) {
            let actual = ChecksumSet::compute(data);
            if expected.crc32 != actual.crc32 {
                return IntegrityResult::CrcMismatch {
                    expected: expected.crc32,
                    actual: actual.crc32,
                };
            }
            if expected.sha256 != actual.sha256 {
                return IntegrityResult::Sha256Mismatch {
                    expected: expected.sha256,
                    actual: actual.sha256,
                };
            }
            IntegrityResult::Ok
        } else {
            IntegrityResult::NotFound
        }
    }

    /// Verifies the recorded checksum of a solid block against a candidate buffer.
    pub fn verify_block(&self, block_index: u32, data: &[u8]) -> IntegrityResult {
        if let Some(expected) = self.block_checksums.get(&block_index) {
            let actual = ChecksumSet::compute(data);
            if expected.crc32 != actual.crc32 {
                return IntegrityResult::CrcMismatch {
                    expected: expected.crc32,
                    actual: actual.crc32,
                };
            }
            if expected.sha256 != actual.sha256 {
                return IntegrityResult::Sha256Mismatch {
                    expected: expected.sha256,
                    actual: actual.sha256,
                };
            }
            IntegrityResult::Ok
        } else {
            IntegrityResult::NotFound
        }
    }

    /// Verifies the recorded archive checksum against a candidate buffer.
    pub fn verify_archive(&self, all_data: &[u8]) -> IntegrityResult {
        if let Some(expected) = &self.archive_checksum {
            let actual = ChecksumSet::compute(all_data);
            if expected.crc32 != actual.crc32 {
                return IntegrityResult::CrcMismatch {
                    expected: expected.crc32,
                    actual: actual.crc32,
                };
            }
            if expected.sha256 != actual.sha256 {
                return IntegrityResult::Sha256Mismatch {
                    expected: expected.sha256,
                    actual: actual.sha256,
                };
            }
            IntegrityResult::Ok
        } else {
            IntegrityResult::NotFound
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_known_value() {
        let hasher = Sha256Hasher::new();
        let digest = hasher.finalize();
        // SHA-256 of empty bytes
        let expected = [
            0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
            0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
            0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
            0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
        ];
        assert_eq!(digest, expected);
    }

    #[test]
    fn test_sha256_incremental_equals_single_pass() {
        let data = b"the quick brown fox jumps over the lazy dog";
        
        let mut hasher1 = Sha256Hasher::new();
        hasher1.update(data);
        let val1 = hasher1.finalize();

        let mut hasher2 = Sha256Hasher::new();
        hasher2.update(&data[..15]);
        hasher2.update(&data[15..30]);
        hasher2.update(&data[30..]);
        let bytes_processed = hasher2.bytes_processed();
        let val2 = hasher2.finalize();

        assert_eq!(val1, val2);
        assert_eq!(bytes_processed, data.len() as u64);
    }

    #[test]
    fn test_checksum_set_compute_and_verify() {
        let data = b"some check data";
        let cset = ChecksumSet::compute(data);
        
        assert!(cset.verify(data));
        
        let (crc_hex, sha_hex) = cset.to_hex_strings();
        assert_eq!(crc_hex.len(), 8);
        assert_eq!(sha_hex.len(), 64);
    }

    #[test]
    fn test_checksum_set_detects_corruption() {
        let mut data = b"some check data".to_vec();
        let cset = ChecksumSet::compute(&data);
        
        assert!(cset.verify(&data));
        
        data[0] ^= 1; // flip a bit
        assert!(!cset.verify(&data));
    }

    #[test]
    fn test_multi_level_integrity_roundtrip() {
        let mut mli = MultiLevelIntegrity::new();
        let file_data = b"file contents";
        let block_data = b"block contents";
        let archive_data = b"archive contents";

        mli.record_file(100, file_data);
        mli.record_block(5, block_data);
        mli.finalize_archive(archive_data);

        assert_eq!(mli.verify_file(100, file_data), IntegrityResult::Ok);
        assert_eq!(mli.verify_block(5, block_data), IntegrityResult::Ok);
        assert_eq!(mli.verify_archive(archive_data), IntegrityResult::Ok);

        assert_eq!(mli.verify_file(101, file_data), IntegrityResult::NotFound);
        assert!(matches!(mli.verify_file(100, b"wrong contents"), IntegrityResult::CrcMismatch { .. }));
    }
}
