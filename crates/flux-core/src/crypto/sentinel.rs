//! Password verification sentinel.
//!
//! Enables immediate detection of incorrect password inputs without trying to decrypt
//! the entire archive payload.

use crate::crypto::stream::{EncryptedChunk, StreamDecryptor, StreamEncryptor};
use crate::crypto::{CryptoError, EncryptionKey};

/// Plaintext string encrypted to form the password verification sentinel.
pub const SENTINEL_PLAINTEXT: &[u8] = b"FLUX_VALID_KEY_V1";

/// Encrypts `SENTINEL_PLAINTEXT` with the derived key to construct the sentinel.
pub fn create_sentinel(key: &EncryptionKey) -> Result<EncryptedChunk, CryptoError> {
    let mut encryptor = StreamEncryptor::new(key.clone());
    encryptor.encrypt_chunk(SENTINEL_PLAINTEXT)
}

/// Attempts to decrypt and verify the sentinel chunk.
///
/// Returns `Ok(true)` if the sentinel decrypts successfully and matches `SENTINEL_PLAINTEXT` (correct password).
/// Returns `Ok(false)` if GCM verification fails (incorrect password).
/// Returns `Err(e)` for any structural or formatting errors (corrupt header).
pub fn verify_sentinel(
    key: &EncryptionKey,
    sentinel: &EncryptedChunk,
) -> Result<bool, CryptoError> {
    let mut decryptor = StreamDecryptor::new(key.clone());

    match decryptor.decrypt_chunk(sentinel) {
        Ok(plaintext) => Ok(plaintext == SENTINEL_PLAINTEXT),
        Err(CryptoError::AuthenticationFailed) => {
            // Decryption failed due to wrong password verification
            Ok(false)
        }
        Err(e) => {
            // Formatting or index mismatch error
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::KeyDerivation;
    use crate::crypto::Salt;

    #[test]
    fn test_sentinel_correct_password_verifies() {
        let password = "SuperSecurePassword123!";
        let salt = Salt([9u8; 16]);
        let key = KeyDerivation::derive_key(password, &salt).unwrap();

        let sentinel = create_sentinel(&key).unwrap();
        let is_valid = verify_sentinel(&key, &sentinel).unwrap();

        assert!(is_valid);
    }

    #[test]
    fn test_sentinel_wrong_password_fails() {
        let salt = Salt([9u8; 16]);
        let key_correct = KeyDerivation::derive_key("SuperSecurePassword123!", &salt).unwrap();
        let key_wrong = KeyDerivation::derive_key("WrongPassword123!", &salt).unwrap();

        let sentinel = create_sentinel(&key_correct).unwrap();
        let is_valid = verify_sentinel(&key_wrong, &sentinel).unwrap();

        assert!(!is_valid);
    }

    #[test]
    fn test_sentinel_tampered_fails() {
        let password = "SuperSecurePassword123!";
        let salt = Salt([9u8; 16]);
        let key = KeyDerivation::derive_key(password, &salt).unwrap();

        let mut sentinel = create_sentinel(&key).unwrap();
        // Tamper with sentinel ciphertext
        sentinel.ciphertext[0] ^= 1;

        let is_valid = verify_sentinel(&key, &sentinel).unwrap();
        // Authentic tag validation fails
        assert!(!is_valid);
    }
}
