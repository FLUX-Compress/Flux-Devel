//! # Cryptography Group
//!
//! ## Purpose
//! The cryptography group provides best-in-class security for FLUX archives. It protects data in
//! transit and at rest against unauthorized access, eavesdropping, and tampering.
//!
//! ## Architectural Decisions
//!
//! ### AES-256-GCM for Streaming Encryption
//! We use AES-256 in Galois/Counter Mode (GCM) for data encryption. Unlike unauthenticated modes
//! (like CBC or CTR) which are vulnerable to padding oracle attacks or ciphertext manipulation
//! (bit-flipping), GCM is an Authenticated Encryption with Associated Data (AEAD) mode. It
//! simultaneously encrypts the data (confidentiality) and computes an authentication tag
//! (authenticity and integrity). If an attacker tampers with even a single bit of the archive,
//! decryption fails immediately and securely. GCM is also hardware-accelerated on modern CPUs via
//! AES-NI instructions, yielding near-zero overhead.
//!
//! ### Argon2id for Key Derivation
//! We use Argon2id to derive 256-bit encryption keys from user passwords. Argon2id is the winner
//! of the Password Hashing Competition (PHC) and is the current industry standard (recommended by
//! OWASP). It is a hybrid memory-hard algorithm that resists GPU- and ASIC-based brute force
//! attacks by requiring configurable, substantial memory block sizes for hashing (64MB in FLUX).
//! This prevents attackers from testing millions of passwords in parallel on graphics hardware.
//!
//! ### Mandatory Compress-Then-Encrypt Ordering
//! Encryption MUST happen after compression. Cryptographically secure ciphertext is indistinguishable
//! from random noise and has maximum Shannon entropy, which makes it mathematically incompressible.
//! Attempting to compress encrypted data yields zero compression ratio and wastes CPU cycles.
//! Compressing first maximizes coding density, and encrypting second ensures complete confidentiality.
//!
//! ### Security Properties
//! - **Confidentiality**: Protected by AES-256, which has no known practical attacks.
//! - **Authenticity**: Guaranteed via GCM authentication tags. Only key holders can produce valid tags.
//! - **Integrity**: Any byte manipulation, truncation, or insertion is detected at chunk granularity.
//!
//! ## Archive Encryption Pipeline Diagram
//!
//! ```text
//!   Compressed Data (from rANS)
//!       │
//!       ▼
//!   [Argon2id Key Derivation] ← password + random salt
//!       │
//!       ├── Encryption Key (256-bit)
//!       └── (key is derived fresh each session, never reused)
//!           │
//!           ▼
//!       [AES-256-GCM] ← random IV per solid block
//!           │
//!           ├── Encrypted ciphertext
//!           └── Authentication tag (per chunk, every 1MB)
//!               │
//!               ▼
//!           [Archive]
//! ```

pub mod keys;
pub mod stream;
pub mod sentinel;
pub mod header;

use std::fmt;
use zeroize::Zeroize;

/// Cryptographically random 128-bit salt wrapper used for key derivation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Salt(pub [u8; 16]);

/// 256-bit encryption key derived from password hashing.
///
/// Implements `Zeroize` on drop to wipe key bytes from system memory, preventing
/// key leakage through memory dumps or cold-boot attacks.
#[derive(Zeroize)]
#[zeroize(drop)]
pub struct EncryptionKey(pub [u8; 32]);

impl EncryptionKey {
    /// Creates a new EncryptionKey from raw bytes.
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl Clone for EncryptionKey {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

/// 96-bit (12-byte) initialization vector (nonce) standard for AES-GCM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Iv(pub [u8; 12]);

/// 128-bit (16-byte) Galois authentication tag for AES-GCM verification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AuthTag(pub [u8; 16]);

/// Errors generated during cryptographic operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    /// Decryption sentinel check failed, indicating an incorrect password.
    WrongPassword,
    /// Data structure or format is corrupted or truncated.
    CorruptData,
    /// Authenticated tag verification failed, indicating ciphertext tampering.
    AuthenticationFailed,
    /// Key derivation engine failed to compute the key.
    KeyDerivationFailed(String),
    /// Parameter inputs are invalid or out of acceptable bounds.
    InvalidParameter(String),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "Incorrect password provided"),
            Self::CorruptData => write!(f, "Archive data is corrupted or truncated"),
            Self::AuthenticationFailed => write!(f, "Authentication tag verification failed (tampering detected)"),
            Self::KeyDerivationFailed(err) => write!(f, "Key derivation failed: {}", err),
            Self::InvalidParameter(err) => write!(f, "Invalid parameter: {}", err),
        }
    }
}

impl std::error::Error for CryptoError {}
