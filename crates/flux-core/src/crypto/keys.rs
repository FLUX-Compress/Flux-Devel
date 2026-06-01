//! Key derivation using Argon2id.
//!
//! Argon2id is the winner of the Password Hashing Competition (PHC) and is the current industry
//! standard (recommended by OWASP) for password hashing and key derivation. It is memory-hard,
//! making GPU and ASIC brute force attacks extremely expensive.

use rand::{RngCore, rngs::OsRng};
use argon2::{Argon2, Params, Algorithm, Version};
use crate::crypto::{EncryptionKey, Salt, CryptoError};

/// 64MB memory requirement.
/// At 64MB per hash check, a GPU with 8GB VRAM can run only ~128 hashing threads in parallel,
/// preventing mass brute-force dictionary attacks.
pub const ARGON2_MEMORY_KB: u32 = 65536;

/// Time cost parameter: 3 iterations over memory.
/// Higher iterations increase derivation time, establishing a balance between user response
/// latency and security strength.
pub const ARGON2_ITERATIONS: u32 = 3;

/// 4 threads (parallel lanes) mapped to key derivation.
/// Establishes multi-threaded GPU defense.
pub const ARGON2_PARALLELISM: u32 = 4;

/// 128-bit (16-byte) salt length.
/// Prevents rainbow table attack tables by ensuring a massive namespace of possible salting combinations.
pub const SALT_LENGTH: usize = 16;

/// 256-bit output key size (32 bytes) suitable for AES-256-GCM.
pub const KEY_LENGTH: usize = 32;

/// Enum describing assessed password complexity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasswordStrength {
    /// Password is short (under 8 chars) or highly predictable.
    Weak,
    /// Adequate for basic security; 8-12 characters with mixed symbols/case.
    Moderate,
    /// High complexity: 12+ characters with solid symbol/case variety.
    Strong,
    /// Outstanding passphrase: 20+ characters.
    VeryStrong,
}

/// Key derivation utilities.
pub struct KeyDerivation;

impl KeyDerivation {
    /// Generates a cryptographically random 128-bit salt.
    ///
    /// Uses `OsRng` which hooks directly into the operating system's Cryptographically Secure
    /// Pseudo-Random Number Generator (CSPRNG), seeded by hardware entropy.
    /// A unique random salt prevents precomputation attacks (e.g. rainbow tables) by ensuring
    /// two identical passwords will hash to completely different keys.
    pub fn generate_salt() -> Salt {
        let mut bytes = [0u8; SALT_LENGTH];
        OsRng.fill_bytes(&mut bytes);
        Salt(bytes)
    }

    /// Derives a 256-bit `EncryptionKey` from a password and salt using Argon2id.
    ///
    /// ### Salt Plaintext Storage
    /// The salt is stored in plaintext in the archive header bootstrap block. This is standard and
    /// cryptographically secure because a salt does not need to remain secret. Its only purpose is to
    /// guarantee key diversity, ensuring that identical passwords across different archives map
    /// to different key material and preventing dictionary attacks.
    pub fn derive_key(password: &str, salt: &Salt) -> Result<EncryptionKey, CryptoError> {
        let params = Params::new(
            ARGON2_MEMORY_KB,
            ARGON2_ITERATIONS,
            ARGON2_PARALLELISM,
            Some(KEY_LENGTH),
        ).map_err(|e| CryptoError::KeyDerivationFailed(e.to_string()))?;

        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let mut key_bytes = [0u8; KEY_LENGTH];
        argon2.hash_password_into(password.as_bytes(), &salt.0, &mut key_bytes)
            .map_err(|e| CryptoError::KeyDerivationFailed(e.to_string()))?;

        Ok(EncryptionKey(key_bytes))
    }

    /// Evaluates password strength.
    ///
    /// Analyzes the length and variety of characters (case, digits, special symbols).
    pub fn verify_key_strength(password: &str) -> PasswordStrength {
        let len = password.len();
        if len < 8 {
            return PasswordStrength::Weak;
        }
        if len >= 20 {
            return PasswordStrength::VeryStrong;
        }

        let has_upper = password.chars().any(|c| c.is_uppercase());
        let has_lower = password.chars().any(|c| c.is_lowercase());
        let has_digit = password.chars().any(|c| c.is_numeric());
        let has_special = password.chars().any(|c| !c.is_alphanumeric());
        let variety = (has_upper as usize) + (has_lower as usize) + (has_digit as usize) + (has_special as usize);

        if len >= 12 {
            if variety >= 3 {
                PasswordStrength::Strong
            } else {
                PasswordStrength::Moderate
            }
        } else {
            // len between 8 and 11 inclusive
            if variety >= 3 {
                PasswordStrength::Moderate
            } else {
                PasswordStrength::Weak
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_derivation_deterministic() {
        let password = "SecretPassword123!";
        let salt = Salt([1u8; 16]);

        let key1 = KeyDerivation::derive_key(password, &salt).unwrap();
        let key2 = KeyDerivation::derive_key(password, &salt).unwrap();

        assert_eq!(key1.0, key2.0);
    }

    #[test]
    fn test_key_derivation_different_salts() {
        let password = "SecretPassword123!";
        let salt1 = Salt([1u8; 16]);
        let salt2 = Salt([2u8; 16]);

        let key1 = KeyDerivation::derive_key(password, &salt1).unwrap();
        let key2 = KeyDerivation::derive_key(password, &salt2).unwrap();

        assert_ne!(key1.0, key2.0);
    }

    #[test]
    fn test_key_derivation_different_passwords() {
        let password1 = "SecretPassword123!";
        let password2 = "SecretPassword123?";
        let salt = Salt([1u8; 16]);

        let key1 = KeyDerivation::derive_key(password1, &salt).unwrap();
        let key2 = KeyDerivation::derive_key(password2, &salt).unwrap();

        assert_ne!(key1.0, key2.0);
    }

    #[test]
    fn test_salt_is_random() {
        let salt1 = KeyDerivation::generate_salt();
        let salt2 = KeyDerivation::generate_salt();

        assert_ne!(salt1.0, salt2.0);
    }

    #[test]
    fn test_key_length_correct() {
        let password = "pass";
        let salt = Salt([1u8; 16]);
        let key = KeyDerivation::derive_key(password, &salt).unwrap();
        assert_eq!(key.0.len(), 32);
    }

    #[test]
    fn test_verify_key_strength() {
        assert_eq!(KeyDerivation::verify_key_strength("short"), PasswordStrength::Weak);
        assert_eq!(KeyDerivation::verify_key_strength("predictable"), PasswordStrength::Weak);
        assert_eq!(KeyDerivation::verify_key_strength("Predict1"), PasswordStrength::Moderate);
        assert_eq!(KeyDerivation::verify_key_strength("PredictableSecret1!"), PasswordStrength::Strong);
        assert_eq!(KeyDerivation::verify_key_strength("VeryLongPassphraseThatIsExtremelySafe"), PasswordStrength::VeryStrong);
    }
}
