//! AES-256-GCM streaming encryption and decryption.
//!
//! AES-256-GCM is an authenticated encryption standard (AEAD) that provides both confidentiality
//! (via encryption) and integrity/authenticity (via an authentication tag). If any byte of the
//! ciphertext or metadata is altered, the decryption engine rejects the stream completely.

use crate::crypto::{AuthTag, CryptoError, EncryptionKey, Iv};
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use rand::{rngs::OsRng, RngCore};

/// 1MB chunks.
/// Performing authentication on 1MB boundaries prevents the decompressor from having
/// to process the entire file before detecting corruption. It also allows granular recovery of
/// non-corrupt solid blocks.
pub const CHUNK_SIZE: usize = 1024 * 1024;

/// 128-bit (16-byte) authentication tag for AES-GCM.
pub const GCM_TAG_SIZE: usize = 16;

/// 96-bit (12-byte) initialization vector (nonce) standard for AES-GCM.
pub const GCM_IV_SIZE: usize = 12;

/// A single encrypted chunk of data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncryptedChunk {
    /// Cryptographically random nonce unique to this chunk.
    pub iv: Iv,
    /// Encrypted data.
    pub ciphertext: Vec<u8>,
    /// Authentication tag validating ciphertext and AAD.
    pub auth_tag: AuthTag,
    /// Sequence number of this chunk in the stream.
    pub chunk_index: u64,
}

/// Streaming encryptor state.
pub struct StreamEncryptor {
    /// 256-bit encryption key.
    pub key: EncryptionKey,
    /// Sequence number index incremented per chunk.
    pub chunk_index: u64,
}

impl StreamEncryptor {
    /// Creates a new StreamEncryptor.
    pub fn new(key: EncryptionKey) -> Self {
        Self {
            key,
            chunk_index: 0,
        }
    }

    /// Encrypts one chunk using AES-256-GCM.
    ///
    /// Generates a fresh random IV per chunk. Includes `chunk_index` in AAD to prevent chunk reordering
    /// and deletion attacks.
    pub fn encrypt_chunk(&mut self, plaintext: &[u8]) -> Result<EncryptedChunk, CryptoError> {
        let mut iv_bytes = [0u8; GCM_IV_SIZE];
        OsRng.fill_bytes(&mut iv_bytes);
        let iv = Iv(iv_bytes);

        let aes_key = Key::<Aes256Gcm>::from_slice(&self.key.0);
        let cipher = Aes256Gcm::new(aes_key);
        let nonce = Nonce::from_slice(&iv.0);

        let aad = self.chunk_index.to_le_bytes();
        let payload = Payload {
            msg: plaintext,
            aad: &aad,
        };

        // aes-gcm appends the 16-byte authentication tag to the ciphertext
        let ciphertext_with_tag = cipher
            .encrypt(nonce, payload)
            .map_err(|e| CryptoError::InvalidParameter(e.to_string()))?;

        if ciphertext_with_tag.len() < GCM_TAG_SIZE {
            return Err(CryptoError::CorruptData);
        }

        let tag_start = ciphertext_with_tag.len() - GCM_TAG_SIZE;
        let ciphertext = ciphertext_with_tag[..tag_start].to_vec();
        let mut tag_bytes = [0u8; GCM_TAG_SIZE];
        tag_bytes.copy_from_slice(&ciphertext_with_tag[tag_start..]);
        let auth_tag = AuthTag(tag_bytes);

        let chunk = EncryptedChunk {
            iv,
            ciphertext,
            auth_tag,
            chunk_index: self.chunk_index,
        };

        self.chunk_index += 1;
        Ok(chunk)
    }

    /// Splits data into CHUNK_SIZE chunks and encrypts each.
    pub fn encrypt_stream(&mut self, data: &[u8]) -> Result<Vec<EncryptedChunk>, CryptoError> {
        let mut chunks = Vec::new();
        if data.is_empty() {
            // Encode at least one empty chunk to maintain stream formatting
            chunks.push(self.encrypt_chunk(&[])?);
        } else {
            for chunk_slice in data.chunks(CHUNK_SIZE) {
                chunks.push(self.encrypt_chunk(chunk_slice)?);
            }
        }
        Ok(chunks)
    }
}

/// Streaming decryptor state.
pub struct StreamDecryptor {
    /// 256-bit encryption key.
    pub key: EncryptionKey,
    /// Expected chunk sequence number index.
    pub expected_chunk_index: u64,
    /// Whether the decryptor state is poisoned due to index mismatch or decryption failure.
    pub poisoned: bool,
}

impl StreamDecryptor {
    /// Creates a new StreamDecryptor.
    pub fn new(key: EncryptionKey) -> Self {
        Self {
            key,
            expected_chunk_index: 0,
            poisoned: false,
        }
    }

    /// Decrypts and verifies one chunk.
    ///
    /// Verifies GCM authentication tags and enforces strict sequence matching. Halts immediately
    /// and returns `CryptoError::AuthenticationFailed` if verification fails.
    pub fn decrypt_chunk(&mut self, chunk: &EncryptedChunk) -> Result<Vec<u8>, CryptoError> {
        if self.poisoned {
            return Err(CryptoError::CorruptData);
        }

        if chunk.chunk_index != self.expected_chunk_index {
            self.poisoned = true;
            return Err(CryptoError::CorruptData);
        }

        let aes_key = Key::<Aes256Gcm>::from_slice(&self.key.0);
        let cipher = Aes256Gcm::new(aes_key);
        let nonce = Nonce::from_slice(&chunk.iv.0);

        // Concatenate ciphertext and authentication tag back together
        let mut combined = chunk.ciphertext.clone();
        combined.extend_from_slice(&chunk.auth_tag.0);

        let aad = chunk.chunk_index.to_le_bytes();
        let payload = Payload {
            msg: &combined,
            aad: &aad,
        };

        let plaintext = cipher.decrypt(nonce, payload).map_err(|_| {
            self.poisoned = true;
            CryptoError::AuthenticationFailed
        })?;

        self.expected_chunk_index += 1;
        Ok(plaintext)
    }

    /// Decrypts all chunks in order, verify each, and returns concatenated plaintext.
    pub fn decrypt_stream(&mut self, chunks: &[EncryptedChunk]) -> Result<Vec<u8>, CryptoError> {
        let mut decrypted = Vec::new();
        for chunk in chunks {
            decrypted.extend_from_slice(&self.decrypt_chunk(chunk)?);
        }
        Ok(decrypted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip_single_chunk() {
        let key = EncryptionKey([7u8; 32]);
        let data = b"Hello, this is a secret message!";

        let mut encryptor = StreamEncryptor::new(key.clone());
        let chunk = encryptor.encrypt_chunk(data).unwrap();

        let mut decryptor = StreamDecryptor::new(key);
        let decrypted = decryptor.decrypt_chunk(&chunk).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip_multi_chunk() {
        let key = EncryptionKey([7u8; 32]);
        // Data larger than 1MB
        let mut data = vec![0u8; CHUNK_SIZE + 500];
        OsRng.fill_bytes(&mut data);

        let mut encryptor = StreamEncryptor::new(key.clone());
        let chunks = encryptor.encrypt_stream(&data).unwrap();
        assert_eq!(chunks.len(), 2);

        let mut decryptor = StreamDecryptor::new(key);
        let decrypted = decryptor.decrypt_stream(&chunks).unwrap();

        assert_eq!(decrypted, data);
    }

    #[test]
    fn test_authentication_fails_on_tampered_ciphertext() {
        let key = EncryptionKey([7u8; 32]);
        let data = b"Hello, this is a secret message!";

        let mut encryptor = StreamEncryptor::new(key.clone());
        let mut chunk = encryptor.encrypt_chunk(data).unwrap();

        // Tamper with the ciphertext (flip first byte)
        chunk.ciphertext[0] ^= 1;

        let mut decryptor = StreamDecryptor::new(key);
        let res = decryptor.decrypt_chunk(&chunk);
        assert_eq!(res, Err(CryptoError::AuthenticationFailed));
    }

    #[test]
    fn test_authentication_fails_on_tampered_tag() {
        let key = EncryptionKey([7u8; 32]);
        let data = b"Hello, this is a secret message!";

        let mut encryptor = StreamEncryptor::new(key.clone());
        let mut chunk = encryptor.encrypt_chunk(data).unwrap();

        // Tamper with tag
        chunk.auth_tag.0[0] ^= 1;

        let mut decryptor = StreamDecryptor::new(key);
        let res = decryptor.decrypt_chunk(&chunk);
        assert_eq!(res, Err(CryptoError::AuthenticationFailed));
    }

    #[test]
    fn test_chunk_reordering_detected() {
        let key = EncryptionKey([7u8; 32]);
        let data1 = b"First block data";
        let data2 = b"Second block data";

        let mut encryptor = StreamEncryptor::new(key.clone());
        let chunk1 = encryptor.encrypt_chunk(data1).unwrap();
        let chunk2 = encryptor.encrypt_chunk(data2).unwrap();

        // Reorder chunks: feed chunk2 as chunk0
        let mut decryptor = StreamDecryptor::new(key);
        let res = decryptor.decrypt_chunk(&chunk2);
        // Expect index mismatch error (CorruptData)
        assert_eq!(res, Err(CryptoError::CorruptData));

        // Re-aligning checks
        let res_correct = decryptor.decrypt_chunk(&chunk1);
        assert_eq!(res_correct, Err(CryptoError::CorruptData));
    }

    #[test]
    fn test_different_keys_fail_decryption() {
        let key_enc = EncryptionKey([7u8; 32]);
        let key_dec = EncryptionKey([8u8; 32]);
        let data = b"Hello, this is a secret message!";

        let mut encryptor = StreamEncryptor::new(key_enc);
        let chunk = encryptor.encrypt_chunk(data).unwrap();

        let mut decryptor = StreamDecryptor::new(key_dec);
        let res = decryptor.decrypt_chunk(&chunk);
        assert_eq!(res, Err(CryptoError::AuthenticationFailed));
    }

    #[test]
    fn test_iv_is_unique_per_chunk() {
        let key = EncryptionKey([7u8; 32]);
        let data = b"Identical data sequence";

        let mut encryptor1 = StreamEncryptor::new(key.clone());
        let chunk1 = encryptor1.encrypt_chunk(data).unwrap();

        let mut encryptor2 = StreamEncryptor::new(key);
        let chunk2 = encryptor2.encrypt_chunk(data).unwrap();

        assert_ne!(chunk1.iv.0, chunk2.iv.0);
    }
}
