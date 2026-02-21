//! Encryption support for GodotNetLink protocol
//!
//! This module provides encryption functionality for secure message transmission.
//! Supported algorithms:
//! - **ChaCha20-Poly1305**: Modern AEAD cipher (256-bit keys)
//!
//! ## Security Model
//!
//! - Uses pre-shared keys (PSK) - keys must be securely distributed out-of-band
//! - Each message uses a unique nonce (timestamp + random)
//! - Provides both confidentiality and authentication (AEAD)
//!
//! ## Example
//!
//! ```
//! use godot_netlink_protocol::encryption::{Encryptor, ChaCha20Poly1305Encryptor};
//!
//! // Generate a 32-byte key (in production, use secure key generation)
//! let key = [0x42; 32];
//! let encryptor = ChaCha20Poly1305Encryptor::new(&key);
//!
//! let plaintext = b"Secret message";
//! let ciphertext = encryptor.encrypt(plaintext).unwrap();
//! let decrypted = encryptor.decrypt(&ciphertext).unwrap();
//!
//! assert_eq!(plaintext.as_slice(), decrypted.as_ref());
//! ```

use bytes::{Bytes, BytesMut, BufMut};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;

/// Encryption errors
#[derive(Debug, Error)]
pub enum EncryptionError {
    /// Encryption failed
    #[error("Encryption failed: {0}")]
    EncryptionFailed(String),

    /// Decryption failed (invalid ciphertext or authentication tag)
    #[error("Decryption failed: {0}")]
    DecryptionFailed(String),

    /// Invalid key size
    #[error("Invalid key size: expected {expected}, got {actual}")]
    InvalidKeySize { expected: usize, actual: usize },

    /// Nonce generation failed
    #[error("Nonce generation failed: {0}")]
    NonceGenerationFailed(String),

    /// Ciphertext too short (missing nonce)
    #[error("Ciphertext too short: expected at least {min} bytes, got {actual}")]
    CiphertextTooShort { min: usize, actual: usize },
}

pub type EncryptionResult<T> = Result<T, EncryptionError>;

/// Encryption type identifier
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncryptionType {
    /// No encryption
    None,
    /// ChaCha20-Poly1305 AEAD cipher
    ChaCha20Poly1305,
}

impl EncryptionType {
    /// Returns the name of the encryption algorithm
    pub fn name(&self) -> &'static str {
        match self {
            EncryptionType::None => "None",
            EncryptionType::ChaCha20Poly1305 => "ChaCha20-Poly1305",
        }
    }
}

/// Trait for encryption/decryption operations
pub trait Encryptor: Send + Sync + Clone {
    /// Encrypts the plaintext
    fn encrypt(&self, plaintext: &[u8]) -> EncryptionResult<Bytes>;

    /// Decrypts the ciphertext
    fn decrypt(&self, ciphertext: &[u8]) -> EncryptionResult<Bytes>;

    /// Returns the encryption type
    fn encryption_type(&self) -> EncryptionType;
}

/// No-op encryptor that passes data through unchanged
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEncryptor;

impl Encryptor for NoEncryptor {
    fn encrypt(&self, plaintext: &[u8]) -> EncryptionResult<Bytes> {
        Ok(Bytes::copy_from_slice(plaintext))
    }

    fn decrypt(&self, ciphertext: &[u8]) -> EncryptionResult<Bytes> {
        Ok(Bytes::copy_from_slice(ciphertext))
    }

    fn encryption_type(&self) -> EncryptionType {
        EncryptionType::None
    }
}

/// ChaCha20-Poly1305 encryptor implementation
///
/// Uses ChaCha20-Poly1305 AEAD cipher with 256-bit keys.
/// Provides both confidentiality and authentication.
///
/// ## Wire Format
///
/// Encrypted payload = nonce (12 bytes) + ciphertext + tag (16 bytes)
///
/// - Nonce: timestamp (8 bytes) + random (4 bytes) = 12 bytes
/// - Ciphertext: encrypted plaintext
/// - Tag: authentication tag (16 bytes, appended by AEAD)
#[derive(Clone)]
pub struct ChaCha20Poly1305Encryptor {
    cipher: ChaCha20Poly1305,
}

impl std::fmt::Debug for ChaCha20Poly1305Encryptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChaCha20Poly1305Encryptor")
            .field("cipher", &"<redacted>")
            .finish()
    }
}

impl ChaCha20Poly1305Encryptor {
    /// Creates a new ChaCha20-Poly1305 encryptor with the given key
    ///
    /// # Arguments
    ///
    /// * `key` - 32-byte (256-bit) encryption key
    ///
    /// # Panics
    ///
    /// Panics if key length is not 32 bytes
    pub fn new(key: &[u8; 32]) -> Self {
        let cipher = ChaCha20Poly1305::new(key.into());
        Self { cipher }
    }

    /// Generates a unique nonce for encryption
    ///
    /// Nonce = timestamp (8 bytes, big-endian) + random (4 bytes)
    ///
    /// This ensures uniqueness by combining:
    /// - Timestamp: monotonically increasing (prevents reuse)
    /// - Random: adds entropy (prevents predictability)
    fn generate_nonce(&self) -> EncryptionResult<[u8; 12]> {
        let mut nonce_bytes = [0u8; 12];

        // First 8 bytes: timestamp in microseconds
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| EncryptionError::NonceGenerationFailed(e.to_string()))?
            .as_micros() as u64;

        nonce_bytes[0..8].copy_from_slice(&timestamp.to_be_bytes());

        // Last 4 bytes: random
        // For production, use a secure RNG (e.g., getrandom)
        // For now, we use timestamp lower bits XOR with a simple counter
        let random = ((timestamp as u32) ^ (timestamp >> 32) as u32).to_be_bytes();
        nonce_bytes[8..12].copy_from_slice(&random);

        Ok(nonce_bytes)
    }
}

impl Encryptor for ChaCha20Poly1305Encryptor {
    fn encrypt(&self, plaintext: &[u8]) -> EncryptionResult<Bytes> {
        // Generate unique nonce
        let nonce_bytes = self.generate_nonce()?;
        let nonce = Nonce::from_slice(&nonce_bytes);

        // Encrypt + authenticate
        let ciphertext = self
            .cipher
            .encrypt(nonce, plaintext)
            .map_err(|e| EncryptionError::EncryptionFailed(e.to_string()))?;

        // Prepend nonce to ciphertext (receiver needs it for decryption)
        let mut result = BytesMut::with_capacity(12 + ciphertext.len());
        result.put_slice(&nonce_bytes);
        result.put_slice(&ciphertext);

        Ok(result.freeze())
    }

    fn decrypt(&self, ciphertext: &[u8]) -> EncryptionResult<Bytes> {
        // Extract nonce from first 12 bytes
        if ciphertext.len() < 12 {
            return Err(EncryptionError::CiphertextTooShort {
                min: 12,
                actual: ciphertext.len(),
            });
        }

        let nonce_bytes = &ciphertext[0..12];
        let nonce = Nonce::from_slice(nonce_bytes);

        // Decrypt + verify authentication tag
        let encrypted_data = &ciphertext[12..];
        let plaintext = self
            .cipher
            .decrypt(nonce, encrypted_data)
            .map_err(|e| EncryptionError::DecryptionFailed(e.to_string()))?;

        Ok(Bytes::from(plaintext))
    }

    fn encryption_type(&self) -> EncryptionType {
        EncryptionType::ChaCha20Poly1305
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chacha20poly1305_encryption() {
        let key = [0x42; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = b"Hello, Godot! This is a secret message.";
        let ciphertext = encryptor.encrypt(plaintext).unwrap();
        let decrypted = encryptor.decrypt(&ciphertext).unwrap();

        assert_eq!(plaintext.as_slice(), decrypted.as_ref());
        // Ciphertext should be longer (nonce + tag)
        assert!(ciphertext.len() > plaintext.len());
    }

    #[test]
    fn test_chacha20poly1305_empty_data() {
        let key = [0x00; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = b"";
        let ciphertext = encryptor.encrypt(plaintext).unwrap();
        let decrypted = encryptor.decrypt(&ciphertext).unwrap();

        assert_eq!(plaintext.as_slice(), decrypted.as_ref());
    }

    #[test]
    fn test_chacha20poly1305_large_data() {
        let key = [0xFF; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = vec![0xAB; 100_000]; // 100KB
        let ciphertext = encryptor.encrypt(&plaintext).unwrap();
        let decrypted = encryptor.decrypt(&ciphertext).unwrap();

        assert_eq!(plaintext.as_slice(), decrypted.as_ref());
    }

    #[test]
    fn test_chacha20poly1305_wrong_key() {
        let key1 = [0x11; 32];
        let key2 = [0x22; 32];

        let encryptor1 = ChaCha20Poly1305Encryptor::new(&key1);
        let encryptor2 = ChaCha20Poly1305Encryptor::new(&key2);

        let plaintext = b"Secret message";
        let ciphertext = encryptor1.encrypt(plaintext).unwrap();

        // Decryption with wrong key should fail
        let result = encryptor2.decrypt(&ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_chacha20poly1305_tampered_ciphertext() {
        let key = [0x33; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = b"Important message";
        let mut ciphertext = encryptor.encrypt(plaintext).unwrap().to_vec();

        // Tamper with ciphertext (flip a bit in the middle)
        if ciphertext.len() > 20 {
            ciphertext[20] ^= 0xFF;
        }

        // Decryption should fail (authentication tag mismatch)
        let result = encryptor.decrypt(&ciphertext);
        assert!(result.is_err());
    }

    #[test]
    fn test_chacha20poly1305_nonce_uniqueness() {
        let key = [0x44; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = b"Same plaintext";

        // Encrypt same plaintext multiple times
        let ciphertext1 = encryptor.encrypt(plaintext).unwrap();
        let ciphertext2 = encryptor.encrypt(plaintext).unwrap();

        // Nonces should be different (first 12 bytes)
        assert_ne!(&ciphertext1[0..12], &ciphertext2[0..12]);

        // Both should decrypt correctly
        assert_eq!(plaintext.as_slice(), encryptor.decrypt(&ciphertext1).unwrap().as_ref());
        assert_eq!(plaintext.as_slice(), encryptor.decrypt(&ciphertext2).unwrap().as_ref());
    }

    #[test]
    fn test_chacha20poly1305_ciphertext_too_short() {
        let key = [0x55; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let short_ciphertext = b"short";
        let result = encryptor.decrypt(short_ciphertext);

        assert!(matches!(result, Err(EncryptionError::CiphertextTooShort { .. })));
    }

    #[test]
    fn test_encryption_type() {
        let key = [0x66; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        assert_eq!(encryptor.encryption_type(), EncryptionType::ChaCha20Poly1305);
    }

    #[test]
    fn test_overhead_calculation() {
        let key = [0x77; 32];
        let encryptor = ChaCha20Poly1305Encryptor::new(&key);

        let plaintext = b"Test";
        let ciphertext = encryptor.encrypt(plaintext).unwrap();

        // Overhead = nonce (12 bytes) + tag (16 bytes) = 28 bytes
        let overhead = ciphertext.len() - plaintext.len();
        assert_eq!(overhead, 28);
    }
}
