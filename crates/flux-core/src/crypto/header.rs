//! Archive header handling.
//!
//! Encrypted FLUX archives contain two header parts:
//! 1. **Plaintext bootstrap header**: Visible metadata that allows configuring key derivation
//!    and password sentinel verification.
//! 2. **Encrypted header section**: Private metadata (e.g. index offset, block counts) readable
//!    only with the correct key.

use crate::crypto::stream::EncryptedChunk;
use crate::crypto::{AuthTag, CryptoError, EncryptionKey, Iv, Salt};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::{rngs::OsRng, RngCore};

/// Plaintext bootstrap header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaintextHeader {
    /// Magic signature bytes: `[0x46, 0x4C, 0x55, 0x58]` (FLUX).
    pub magic: [u8; 4],
    /// Major format version.
    pub version_major: u8,
    /// Minor format version.
    pub version_minor: u8,
    /// Compression level enum (human-friendly metadata).
    pub compression_level: u8,
    /// Window size in bytes (u32, little-endian) ← SOURCE OF TRUTH
    pub window_size: u32,
    /// Block size in bytes (u32, little-endian)
    pub block_size: u32,
    /// Flags byte (bit0: encrypted, etc.).
    pub flags: u8,
    /// Argon2id memory requirement parameter.
    pub argon2_memory_kb: u32,
    /// Argon2id iterations parameter.
    pub argon2_iterations: u32,
    /// Argon2id parallelism parameter.
    pub argon2_parallelism: u32,
    /// Salt used to derive the encryption key.
    pub salt: Salt,
    /// Sentinel chunk used to verify password correctness.
    pub sentinel_chunk: EncryptedChunk,
    /// IV used to encrypt the private header block.
    pub encrypted_header_iv: Iv,
    /// True if the archive body and index are encrypted.
    pub is_encrypted: bool,
}

/// Encrypted archive header section.
/// Stored in ciphertext form inside the archive, readable only with the correct key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedHeaderData {
    /// Offset of the file index block.
    pub file_index_offset: u64,
    /// Size in bytes of the file index block.
    pub file_index_size: u64,
    /// Total solid blocks in the archive.
    pub solid_block_count: u32,
    /// Target compression level.
    pub compression_level: u8,
    /// Transform flags applied.
    pub transform_flags: u8,
    /// Sum of all original uncompressed file sizes.
    pub original_total_size: u64,
    /// Sum of all compressed block sizes.
    pub compressed_total_size: u64,
    /// SHA-256 integrity checksum of the archive body.
    pub archive_sha256: [u8; 32],
}

impl PlaintextHeader {
    /// Serializes the plaintext header into binary format, appending CRC32.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.magic);
        buf.push(self.version_major);
        buf.push(self.version_minor);
        buf.push(self.compression_level);
        buf.extend_from_slice(&self.window_size.to_le_bytes());
        buf.extend_from_slice(&self.block_size.to_le_bytes());
        let final_flags = if self.is_encrypted {
            self.flags | 1
        } else {
            self.flags & !1
        };
        buf.push(final_flags);
        buf.extend_from_slice(&self.argon2_memory_kb.to_le_bytes());
        buf.extend_from_slice(&self.argon2_iterations.to_le_bytes());
        buf.extend_from_slice(&self.argon2_parallelism.to_le_bytes());
        buf.extend_from_slice(&self.salt.0);
        buf.extend_from_slice(&self.encrypted_header_iv.0);

        // sentinel_chunk
        buf.extend_from_slice(&self.sentinel_chunk.chunk_index.to_le_bytes());
        buf.extend_from_slice(&self.sentinel_chunk.iv.0);
        buf.extend_from_slice(&self.sentinel_chunk.auth_tag.0);
        let ct_len = self.sentinel_chunk.ciphertext.len() as u32;
        buf.extend_from_slice(&ct_len.to_le_bytes());
        buf.extend_from_slice(&self.sentinel_chunk.ciphertext);

        // Compute and append CRC32
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());

        buf
    }

    /// Deserializes the plaintext header from binary format and verifies CRC32.
    pub fn deserialize(data: &[u8]) -> Result<Self, CryptoError> {
        // Base size limit check: 96 base bytes + 4 CRC32 bytes = 100
        if data.len() < 100 {
            return Err(CryptoError::CorruptData);
        }

        // Verify CRC32
        let stored_crc = u32::from_le_bytes(
            data[data.len() - 4..]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        let computed_crc = crc32fast::hash(&data[..data.len() - 4]);
        if stored_crc != computed_crc {
            return Err(CryptoError::CorruptData);
        }

        let mut pos = 0;
        let mut magic = [0u8; 4];
        magic.copy_from_slice(&data[pos..pos + 4]);
        pos += 4;

        let version_major = data[pos];
        pos += 1;

        let version_minor = data[pos];
        pos += 1;

        let compression_level = data[pos];
        pos += 1;

        let window_size = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 4;

        let block_size = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 4;

        let flags = data[pos];
        pos += 1;

        let is_encrypted = (flags & 1) != 0;

        let argon2_memory_kb = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 4;

        let argon2_iterations = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 4;

        let argon2_parallelism = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 4;

        let mut salt_bytes = [0u8; 16];
        salt_bytes.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;
        let salt = Salt(salt_bytes);

        let mut iv_bytes = [0u8; 12];
        iv_bytes.copy_from_slice(&data[pos..pos + 12]);
        pos += 12;
        let encrypted_header_iv = Iv(iv_bytes);

        // sentinel_chunk
        let chunk_index = u64::from_le_bytes(
            data[pos..pos + 8]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        pos += 8;

        let mut s_iv = [0u8; 12];
        s_iv.copy_from_slice(&data[pos..pos + 12]);
        pos += 12;

        let mut s_tag = [0u8; 16];
        s_tag.copy_from_slice(&data[pos..pos + 16]);
        pos += 16;

        let ct_len = u32::from_le_bytes(
            data[pos..pos + 4]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        ) as usize;
        pos += 4;

        if pos + ct_len + 4 > data.len() {
            return Err(CryptoError::CorruptData);
        }
        let ciphertext = data[pos..pos + ct_len].to_vec();

        let sentinel_chunk = EncryptedChunk {
            iv: Iv(s_iv),
            ciphertext,
            auth_tag: AuthTag(s_tag),
            chunk_index,
        };

        Ok(PlaintextHeader {
            magic,
            version_major,
            version_minor,
            compression_level,
            window_size,
            block_size,
            flags,
            argon2_memory_kb,
            argon2_iterations,
            argon2_parallelism,
            salt,
            sentinel_chunk,
            encrypted_header_iv,
            is_encrypted,
        })
    }
}

impl EncryptedHeaderData {
    /// Serializes encrypted header fields.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.file_index_offset.to_le_bytes());
        buf.extend_from_slice(&self.file_index_size.to_le_bytes());
        buf.extend_from_slice(&self.solid_block_count.to_le_bytes());
        buf.push(self.compression_level);
        buf.push(self.transform_flags);
        buf.extend_from_slice(&self.original_total_size.to_le_bytes());
        buf.extend_from_slice(&self.compressed_total_size.to_le_bytes());
        buf.extend_from_slice(&self.archive_sha256);
        buf
    }

    /// Deserializes encrypted header fields.
    pub fn deserialize(data: &[u8]) -> Result<Self, CryptoError> {
        if data.len() < 70 {
            return Err(CryptoError::CorruptData);
        }
        let mut pos = 0;
        let file_index_offset = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let file_index_size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let solid_block_count = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        pos += 4;
        let compression_level = data[pos];
        pos += 1;
        let transform_flags = data[pos];
        pos += 1;
        let original_total_size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let compressed_total_size = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap());
        pos += 8;
        let mut archive_sha256 = [0u8; 32];
        archive_sha256.copy_from_slice(&data[pos..pos + 32]);

        Ok(EncryptedHeaderData {
            file_index_offset,
            file_index_size,
            solid_block_count,
            compression_level,
            transform_flags,
            original_total_size,
            compressed_total_size,
            archive_sha256,
        })
    }
}

/// Encrypts header data with the key, prepending a random 12-byte IV to the ciphertext.
pub fn encrypt_header_data(
    data: &EncryptedHeaderData,
    key: &EncryptionKey,
) -> Result<Vec<u8>, CryptoError> {
    let mut iv_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut iv_bytes);
    let iv = Iv(iv_bytes);

    let aes_key = Key::<Aes256Gcm>::from_slice(&key.0);
    let cipher = Aes256Gcm::new(aes_key);
    let nonce = Nonce::from_slice(&iv.0);

    let plaintext = data.serialize();
    let payload = Payload {
        msg: &plaintext,
        aad: &[],
    };

    let ciphertext_with_tag = cipher
        .encrypt(nonce, payload)
        .map_err(|e| CryptoError::InvalidParameter(e.to_string()))?;

    let mut result = Vec::new();
    result.extend_from_slice(&iv.0);
    result.extend_from_slice(&ciphertext_with_tag);
    Ok(result)
}

/// Decrypts header data, expecting the first 12 bytes to be the IV.
pub fn decrypt_header_data(
    encrypted: &[u8],
    key: &EncryptionKey,
) -> Result<EncryptedHeaderData, CryptoError> {
    if encrypted.len() < 12 + 16 {
        return Err(CryptoError::CorruptData);
    }
    let mut iv_bytes = [0u8; 12];
    iv_bytes.copy_from_slice(&encrypted[..12]);
    let iv = Iv(iv_bytes);

    let aes_key = Key::<Aes256Gcm>::from_slice(&key.0);
    let cipher = Aes256Gcm::new(aes_key);
    let nonce = Nonce::from_slice(&iv.0);

    let payload = Payload {
        msg: &encrypted[12..],
        aad: &[],
    };

    let plaintext = cipher
        .decrypt(nonce, payload)
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    EncryptedHeaderData::deserialize(&plaintext)
}

/// Multi-volume outer header (50 bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VolumeHeader {
    /// Magic signature bytes: `[0x46, 0x4C, 0x55, 0x56]` (FLXV).
    pub magic: [u8; 4],
    /// Major format version (1).
    pub version_major: u8,
    /// Minor format version (3).
    pub version_minor: u8,
    /// Volume number (1-indexed).
    pub volume_number: u16,
    /// Total volumes in the set.
    pub total_volumes: u16,
    /// Archive set identifier.
    pub archive_id: [u8; 16],
    /// CRC32 of all bytes in the volume file after this 50-byte header.
    pub volume_payload_crc32: u32,
    /// Length of payload in this volume.
    pub volume_payload_length: u64,
    /// Offset of the Volume Index relative to the start of this file. `0` if not present.
    pub volume_index_offset: u64,
}

impl VolumeHeader {
    /// Serializes the Volume Header into binary format, appending CRC32.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(50);
        buf.extend_from_slice(&self.magic);
        buf.push(self.version_major);
        buf.push(self.version_minor);
        buf.extend_from_slice(&self.volume_number.to_le_bytes());
        buf.extend_from_slice(&self.total_volumes.to_le_bytes());
        buf.extend_from_slice(&self.archive_id);
        buf.extend_from_slice(&self.volume_payload_crc32.to_le_bytes());
        buf.extend_from_slice(&self.volume_payload_length.to_le_bytes());
        buf.extend_from_slice(&self.volume_index_offset.to_le_bytes());

        // Compute and append CRC32
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Deserializes the Volume Header from binary format and verifies CRC32.
    pub fn deserialize(data: &[u8]) -> Result<Self, CryptoError> {
        if data.len() < 50 {
            return Err(CryptoError::CorruptData);
        }

        // Verify CRC32
        let stored_crc = u32::from_le_bytes(
            data[46..50]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        let computed_crc = crc32fast::hash(&data[..46]);
        if stored_crc != computed_crc {
            return Err(CryptoError::CorruptData);
        }

        let mut magic = [0u8; 4];
        magic.copy_from_slice(&data[0..4]);
        if &magic != b"FLXV" {
            return Err(CryptoError::CorruptData);
        }

        let version_major = data[4];
        let version_minor = data[5];
        let volume_number = u16::from_le_bytes(data[6..8].try_into().unwrap());
        let total_volumes = u16::from_le_bytes(data[8..10].try_into().unwrap());

        let mut archive_id = [0u8; 16];
        archive_id.copy_from_slice(&data[10..26]);

        let volume_payload_crc32 = u32::from_le_bytes(data[26..30].try_into().unwrap());
        let volume_payload_length = u64::from_le_bytes(data[30..38].try_into().unwrap());
        let volume_index_offset = u64::from_le_bytes(data[38..46].try_into().unwrap());

        Ok(VolumeHeader {
            magic,
            version_major,
            version_minor,
            volume_number,
            total_volumes,
            archive_id,
            volume_payload_crc32,
            volume_payload_length,
            volume_index_offset,
        })
    }
}

/// Table mapping block IDs to volume number and byte offset.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct VolumeIndex {
    /// Maps solid_block_id to (volume_number, block_offset)
    pub blocks: Vec<(u16, u64)>,
}

impl VolumeIndex {
    /// Serializes the Volume Index into a binary byte representation with CRC32.
    pub fn serialize(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"FLXI");
        buf.extend_from_slice(&(self.blocks.len() as u32).to_le_bytes());
        for &(vol, offset) in &self.blocks {
            buf.extend_from_slice(&vol.to_le_bytes());
            buf.extend_from_slice(&offset.to_le_bytes());
        }
        let crc = crc32fast::hash(&buf);
        buf.extend_from_slice(&crc.to_le_bytes());
        buf
    }

    /// Deserializes the Volume Index from binary format and verifies CRC32.
    pub fn deserialize(data: &[u8]) -> Result<Self, CryptoError> {
        if data.len() < 12 {
            return Err(CryptoError::CorruptData);
        }
        let crc_pos = data.len() - 4;
        let stored_crc = u32::from_le_bytes(
            data[crc_pos..]
                .try_into()
                .map_err(|_| CryptoError::CorruptData)?,
        );
        let computed_crc = crc32fast::hash(&data[..crc_pos]);
        if stored_crc != computed_crc {
            return Err(CryptoError::CorruptData);
        }

        if &data[0..4] != b"FLXI" {
            return Err(CryptoError::CorruptData);
        }

        let block_count = u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize;
        if data.len() < 8 + block_count * 10 + 4 {
            return Err(CryptoError::CorruptData);
        }

        let mut blocks = Vec::with_capacity(block_count);
        let mut pos = 8;
        for _ in 0..block_count {
            let vol = u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap());
            let offset = u64::from_le_bytes(data[pos + 2..pos + 10].try_into().unwrap());
            blocks.push((vol, offset));
            pos += 10;
        }

        Ok(VolumeIndex { blocks })
    }
}

/// Encrypts volume index payload with the key, prepending a random 12-byte IV.
pub fn encrypt_volume_index(
    index: &VolumeIndex,
    key: &EncryptionKey,
) -> Result<Vec<u8>, CryptoError> {
    let mut iv_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut iv_bytes);
    let iv = Iv(iv_bytes);

    let aes_key = Key::<Aes256Gcm>::from_slice(&key.0);
    let cipher = Aes256Gcm::new(aes_key);
    let nonce = Nonce::from_slice(&iv.0);

    let plaintext = index.serialize();
    let payload = Payload {
        msg: &plaintext,
        aad: &[],
    };

    let ciphertext_with_tag = cipher
        .encrypt(nonce, payload)
        .map_err(|e| CryptoError::InvalidParameter(e.to_string()))?;

    let mut result = Vec::new();
    result.extend_from_slice(&iv.0);
    result.extend_from_slice(&ciphertext_with_tag);
    Ok(result)
}

/// Decrypts volume index, expecting the first 12 bytes to be the IV.
pub fn decrypt_volume_index(
    encrypted: &[u8],
    key: &EncryptionKey,
) -> Result<VolumeIndex, CryptoError> {
    if encrypted.len() < 12 + 16 {
        return Err(CryptoError::CorruptData);
    }
    let mut iv_bytes = [0u8; 12];
    iv_bytes.copy_from_slice(&encrypted[..12]);
    let iv = Iv(iv_bytes);

    let aes_key = Key::<Aes256Gcm>::from_slice(&key.0);
    let cipher = Aes256Gcm::new(aes_key);
    let nonce = Nonce::from_slice(&iv.0);

    let payload = Payload {
        msg: &encrypted[12..],
        aad: &[],
    };

    let plaintext = cipher
        .decrypt(nonce, payload)
        .map_err(|_| CryptoError::AuthenticationFailed)?;

    VolumeIndex::deserialize(&plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_plaintext_header() -> PlaintextHeader {
        PlaintextHeader {
            magic: *b"FLUX",
            version_major: 1,
            version_minor: 0,
            compression_level: 2,
            window_size: 33554432,
            block_size: 33554432,
            flags: 1,
            argon2_memory_kb: 65536,
            argon2_iterations: 3,
            argon2_parallelism: 4,
            salt: Salt([1u8; 16]),
            sentinel_chunk: EncryptedChunk {
                iv: Iv([2u8; 12]),
                ciphertext: vec![3u8; 17],
                auth_tag: AuthTag([4u8; 16]),
                chunk_index: 0,
            },
            encrypted_header_iv: Iv([5u8; 12]),
            is_encrypted: true,
        }
    }

    fn mock_encrypted_header_data() -> EncryptedHeaderData {
        EncryptedHeaderData {
            file_index_offset: 1024,
            file_index_size: 512,
            solid_block_count: 5,
            compression_level: 6,
            transform_flags: 7,
            original_total_size: 99999,
            compressed_total_size: 12345,
            archive_sha256: [8u8; 32],
        }
    }

    #[test]
    fn test_plaintext_header_serialize_deserialize() {
        let header = mock_plaintext_header();
        let serialized = header.serialize();
        let deserialized = PlaintextHeader::deserialize(&serialized).unwrap();
        assert_eq!(header, deserialized);
    }

    #[test]
    fn test_encrypted_header_roundtrip() {
        let key = EncryptionKey([7u8; 32]);
        let data = mock_encrypted_header_data();

        let encrypted = encrypt_header_data(&data, &key).unwrap();
        let decrypted = decrypt_header_data(&encrypted, &key).unwrap();

        assert_eq!(data, decrypted);
    }

    #[test]
    fn test_encrypted_header_wrong_key_fails() {
        let key_enc = EncryptionKey([7u8; 32]);
        let key_dec = EncryptionKey([8u8; 32]);
        let data = mock_encrypted_header_data();

        let encrypted = encrypt_header_data(&data, &key_enc).unwrap();
        let res = decrypt_header_data(&encrypted, &key_dec);

        assert_eq!(res, Err(CryptoError::AuthenticationFailed));
    }

    #[test]
    fn test_window_size_for_level_mapping() {
        use crate::ffi::{window_size_for_level, FluxCompressionLevel};
        assert_eq!(
            window_size_for_level(FluxCompressionLevel::Tiny),
            256 * 1024
        );
        assert_eq!(
            window_size_for_level(FluxCompressionLevel::Fast),
            4 * 1024 * 1024
        );
        assert_eq!(
            window_size_for_level(FluxCompressionLevel::Balanced),
            32 * 1024 * 1024
        );
        assert_eq!(
            window_size_for_level(FluxCompressionLevel::Maximum),
            128 * 1024 * 1024
        );
        assert_eq!(
            window_size_for_level(FluxCompressionLevel::Extreme),
            256 * 1024 * 1024
        );
    }

    #[test]
    fn test_plaintext_header_crc_corruption_detection() {
        let header = mock_plaintext_header();
        let mut serialized = header.serialize();

        // Confirm it deserializes successfully originally
        assert!(PlaintextHeader::deserialize(&serialized).is_ok());

        // Corrupt a single byte in the serialized stream
        let corrupt_idx = 10;
        serialized[corrupt_idx] ^= 0xFF;

        let res = PlaintextHeader::deserialize(&serialized);
        assert!(res.is_err(), "CRC mismatch was not detected!");
        assert_eq!(res.err().unwrap(), CryptoError::CorruptData);
    }
}
