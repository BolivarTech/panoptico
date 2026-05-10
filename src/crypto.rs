// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-12

//! Self-contained cryptographic module — key derivation, authenticated
//! encryption, and forward error correction.
//!
//! Provides trait-based abstractions for each cryptographic layer and a
//! [`CryptoVault`] compositor that wires them together. Default algorithms:
//! Argon2 (KDF) + AES-256-GCM-SIV (AEAD, nonce-misuse resistant) +
//! Reed-Solomon (FEC).
//!
//! # Architecture
//!
//! ```text
//! CryptoVault (compositor)
//! ├── Box<dyn KeyDerivation>       → Argon2Kdf (default)
//! ├── Box<dyn AuthenticatedCipher> → Aes256GcmSivCipher (default)
//! └── Box<dyn ErrorCorrection>     → ReedSolomonCodec (default)
//! ```
//!
//! # Encrypted blob format
//!
//! [`CryptoVault::encrypt`] produces a Base64 string whose decoded bytes have
//! the following layout:
//!
//! ```text
//! Base64( original_len₄ ‖ RS-encoded( salt₁₆ ‖ ciphertext_N ‖ tag₁₆ ) )
//! ```
//!
//! The nonce is **not stored** in the blob — it is derived from the KDF
//! output alongside the key: `Argon2(password, salt) → key(32) ‖ nonce(12)`.
//!
//! ## Field breakdown
//!
//! ```text
//! ┌───────────────┬─────────────────────────────────────────────────────────────┐
//! │  Length header │                     RS-encoded data                        │
//! │   (4 bytes)   │                                                             │
//! ├───────────────┼──────────┬──────────┬────────────────────┬──────────────────┤
//! │ original_len  │  Block 1 │  Block 2 │        ...         │  Block K         │
//! │ LE u32        │ data+ecc │ data+ecc │                    │  data+ecc        │
//! └───────────────┴──────────┴──────────┴────────────────────┴──────────────────┘
//! ```
//!
//! | Field | Size | Description |
//! |-------|------|-------------|
//! | `original_len` | 4 bytes | Little-endian `u32` — byte length of the plaindata before RS encoding |
//! | RS-encoded data | variable | Reed-Solomon encoded blocks (see below) |
//!
//! ## Plaindata (before RS encoding)
//!
//! The plaindata protected by Reed-Solomon is:
//!
//! ```text
//! ┌──────────┬──────────────────────────────────┐
//! │   Salt   │   Ciphertext + GCM-SIV Tag       │
//! │ 16 bytes │   N + 16 bytes                   │
//! └──────────┴──────────────────────────────────┘
//! ```
//!
//! | Field | Size | Description |
//! |-------|------|-------------|
//! | Salt | 16 bytes | Random salt for Argon2 key derivation |
//! | Ciphertext | N bytes | AES-256-GCM-SIV encrypted plaintext (`N = plaintext.len()`) |
//! | SIV Tag | 16 bytes | GCM-SIV authentication tag (appended by the cipher) |
//!
//! ## Reed-Solomon encoding
//!
//! The plaindata is split into chunks of up to 223 bytes (RS data length)
//! and each chunk is encoded as an RS(255, 223) block — 223 data bytes plus
//! 32 parity bytes. This allows correcting up to 16 corrupted bytes per
//! block. For a typical API key (~50 chars), the plaindata fits in a single
//! RS block.
//!
//! ```text
//! ┌─────────────────────┬─────────────────┐
//! │      Data           │    Parity       │
//! │  ≤ 223 bytes        │   32 bytes      │
//! └─────────────────────┴─────────────────┘
//!        one RS block (≤ 255 bytes)
//! ```
//!
//! ## Security properties
//!
//! | Property | Guarantee |
//! |----------|-----------|
//! | Confidentiality | AES-256-GCM-SIV (256-bit key) |
//! | Integrity | GCM-SIV authentication tag (128-bit) |
//! | Nonce-misuse resistance | SIV construction — confidentiality preserved even if salt collides |
//! | Anti brute-force | Argon2id key derivation (memory-hard, CPU-intensive) |
//! | Derived nonce | Nonce derived from KDF output — collision impossible with unique salt |
//! | Error resilience | Reed-Solomon corrects up to 16 bytes per 255-byte block |
//! | Portability | Base64 output — safe for TOML, environment variables, etc. |
//!
//! ## Large input walkthrough (10,000 characters)
//!
//! AES-256-GCM encrypts the entire plaintext in a single operation regardless
//! of size. The only component that splits data into blocks is Reed-Solomon,
//! which operates transparently inside [`ReedSolomonCodec`]. The output is
//! always a single contiguous Base64 string.
//!
//! **Step 1 — KDF derives key + nonce from password and random salt:**
//!
//! ```text
//! Argon2(password, random_salt) → 44 bytes
//!                                  ├── key   (0..32)  = 256-bit AES key
//!                                  └── nonce (32..44) = 96-bit GCM-SIV nonce
//! ```
//!
//! **Step 2 — Encryption (one AES-GCM-SIV operation):**
//!
//! ```text
//! plaindata = salt(16) + AES-GCM-SIV(10,000 bytes) + tag(16)
//!           = 10,032 bytes  (12 bytes smaller — nonce not stored)
//! ```
//!
//! **Step 3 — Reed-Solomon encoding (multiple blocks, concatenated):**
//!
//! ```text
//! 10,032 bytes ÷ 223 bytes/block = 45 blocks (44 full + 1 partial)
//!
//! ┌───────────┬───────────┬───────────┬─────┬────────────┐
//! │  Block 1  │  Block 2  │  Block 3  │ ... │  Block 45  │
//! │ 223+32 B  │ 223+32 B  │ 223+32 B  │     │  20+32 B   │
//! │  = 255 B  │  = 255 B  │  = 255 B  │     │  = 52 B    │
//! └───────────┴───────────┴───────────┴─────┴────────────┘
//! RS total: 44 × 255 + 52 = 11,272 bytes
//! ```
//!
//! **Step 4 — Final blob:**
//!
//! ```text
//! length header(4) + RS data(11,272) = 11,276 bytes → Base64 → ~15,036 chars
//! ```
//!
//! **Decryption** reverses the process: the complete Base64 string is passed
//! to [`CryptoVault::decrypt`], which reads the length header, RS-decodes
//! block by block (correcting errors if any), extracts the salt, re-derives
//! key + nonce from Argon2, and AES-GCM-SIV decrypts back to the original
//! 10,000 characters.
//!
//! # Example
//!
//! ```
//! use panoptico::crypto::CryptoVault;
//!
//! let vault = CryptoVault::default();
//! let encrypted = vault.encrypt("my-password", "secret-data").unwrap();
//! let decrypted = vault.decrypt("my-password", &encrypted).unwrap();
//! assert_eq!(decrypted, "secret-data");
//! ```

use std::fmt;

use aes_gcm_siv::aead::generic_array::GenericArray;
use aes_gcm_siv::aead::{Aead, KeyInit};
use aes_gcm_siv::Aes256GcmSiv;
use argon2::Argon2;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use rand::RngCore;
use zeroize::Zeroizing;

// ── Public constants ────────────────────────────────────────────────

/// Salt length in bytes for Argon2 key derivation.
pub const SALT_LEN: usize = 16;

/// Derived key length in bytes (AES-256).
pub const KEY_LEN: usize = 32;

/// Default Reed-Solomon parity bytes per block.
pub const RS_DEFAULT_PARITY_LEN: usize = 32;

/// Default Reed-Solomon data bytes per block.
pub const RS_DEFAULT_DATA_LEN: usize = 223;

/// Maximum Reed-Solomon block size — GF(2^8) field constraint.
const RS_MAX_BLOCK_SIZE: usize = 255;

// ── CryptoError ─────────────────────────────────────────────────────

/// Error type for cryptographic operations.
///
/// Each variant maps to a specific stage in the encrypt/decrypt pipeline.
#[derive(Debug)]
pub enum CryptoError {
    /// Key derivation failure (e.g., invalid Argon2 parameters).
    KeyDerivation(String),
    /// Cipher failure (e.g., wrong key, corrupted ciphertext).
    Cipher(String),
    /// Forward error correction failure (e.g., corruption beyond capacity).
    ErrorCorrection(String),
    /// Base64 encoding/decoding failure.
    Encoding(String),
    /// Invalid input (e.g., empty password).
    InvalidInput(String),
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyDerivation(msg) => write!(f, "Key derivation error: {}", msg),
            Self::Cipher(msg) => write!(f, "Cipher error: {}", msg),
            Self::ErrorCorrection(msg) => write!(f, "Error correction error: {}", msg),
            Self::Encoding(msg) => write!(f, "Encoding error: {}", msg),
            Self::InvalidInput(msg) => write!(f, "Invalid input: {}", msg),
        }
    }
}

impl std::error::Error for CryptoError {}

// ── Traits ──────────────────────────────────────────────────────────

/// Key derivation function.
///
/// Derives a fixed-length cryptographic key from a password and salt.
pub trait KeyDerivation: Send + Sync {
    /// Derive a key of `output_len` bytes from `password` and `salt`.
    ///
    /// The returned key is wrapped in [`Zeroizing`] to ensure it is erased
    /// from memory when dropped, even if the caller does not handle it
    /// explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::KeyDerivation`] on failure.
    fn derive_key(
        &self,
        password: &[u8],
        salt: &[u8],
        output_len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError>;
}

/// Authenticated encryption with associated data (AEAD) cipher.
///
/// Provides confidentiality and integrity in a single operation.
pub trait AuthenticatedCipher: Send + Sync {
    /// Encrypt `data` with the given `key` and `nonce`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Cipher`] on failure.
    fn encrypt(&self, key: &[u8], nonce: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError>;

    /// Decrypt `data` with the given `key` and `nonce`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Cipher`] on failure.
    fn decrypt(&self, key: &[u8], nonce: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError>;

    /// Returns the required nonce length in bytes for this cipher.
    fn nonce_len(&self) -> usize;
}

/// Forward error correction codec.
///
/// Adds redundancy to detect and correct bit errors.
pub trait ErrorCorrection: Send + Sync {
    /// Encode `data` with error correction redundancy.
    fn encode(&self, data: &[u8]) -> Vec<u8>;

    /// Decode and correct errors, truncating to `original_len`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::ErrorCorrection`] if corruption exceeds capacity.
    fn decode(&self, encoded: &[u8], original_len: usize) -> Result<Vec<u8>, CryptoError>;
}

// ── Argon2Kdf ───────────────────────────────────────────────────────

/// Argon2id key derivation with default parameters.
pub struct Argon2Kdf;

impl KeyDerivation for Argon2Kdf {
    /// Derive a key using Argon2id with default parameters.
    ///
    /// # Arguments
    ///
    /// * `password` — user-provided password bytes.
    /// * `salt` — random salt (must be at least 8 bytes for Argon2).
    /// * `output_len` — desired key length in bytes.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::KeyDerivation`] if Argon2 fails.
    fn derive_key(
        &self,
        password: &[u8],
        salt: &[u8],
        output_len: usize,
    ) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
        let mut key = Zeroizing::new(vec![0u8; output_len]);
        Argon2::default()
            .hash_password_into(password, salt, &mut key)
            .map_err(|e| CryptoError::KeyDerivation(format!("Argon2 failed: {}", e)))?;
        Ok(key)
    }
}

// ── Aes256GcmSivCipher ──────────────────────────────────────────────

/// AES-256-GCM-SIV nonce-misuse resistant authenticated cipher.
///
/// Unlike standard AES-GCM, GCM-SIV derives a synthetic IV from the
/// nonce, key, and plaintext. If a nonce is reused with a different
/// plaintext, confidentiality is still preserved (only equality of
/// plaintexts is leaked when both nonce AND plaintext match).
pub struct Aes256GcmSivCipher;

/// Nonce length in bytes for AES-256-GCM-SIV.
const AES_GCM_SIV_NONCE_LEN: usize = 12;

impl AuthenticatedCipher for Aes256GcmSivCipher {
    /// Encrypt `data` with AES-256-GCM-SIV.
    ///
    /// # Arguments
    ///
    /// * `key` — 32-byte AES-256 key.
    /// * `nonce` — 12-byte nonce (misuse-resistant: safe even if reused).
    /// * `data` — plaintext to encrypt.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Cipher`] on init or encryption failure.
    fn encrypt(&self, key: &[u8], nonce: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let cipher = Aes256GcmSiv::new_from_slice(key)
            .map_err(|e| CryptoError::Cipher(format!("Cipher init failed: {}", e)))?;
        let nonce = GenericArray::from_slice(nonce);
        cipher
            .encrypt(nonce, data)
            .map_err(|e| CryptoError::Cipher(format!("Encryption failed: {}", e)))
    }

    /// Decrypt `data` with AES-256-GCM-SIV.
    ///
    /// # Arguments
    ///
    /// * `key` — 32-byte AES-256 key.
    /// * `nonce` — 12-byte nonce used during encryption.
    /// * `data` — ciphertext to decrypt (includes SIV tag).
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::Cipher`] on init, decryption, or auth failure.
    fn decrypt(&self, key: &[u8], nonce: &[u8], data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let cipher = Aes256GcmSiv::new_from_slice(key)
            .map_err(|e| CryptoError::Cipher(format!("Cipher init failed: {}", e)))?;
        let nonce = GenericArray::from_slice(nonce);
        cipher
            .decrypt(nonce, data)
            .map_err(|e| CryptoError::Cipher(format!("Decryption failed: {}", e)))
    }

    /// Returns the required nonce length in bytes (12 for AES-256-GCM-SIV).
    fn nonce_len(&self) -> usize {
        AES_GCM_SIV_NONCE_LEN
    }
}

// ── ReedSolomonCodec ────────────────────────────────────────────────

/// Reed-Solomon forward error correction codec.
///
/// Splits data into blocks and appends parity bytes for error detection
/// and correction. Default parameters: RS(255, 223) — 32 parity bytes
/// per 223-byte data block, correcting up to 16 corrupted bytes.
#[derive(Debug)]
pub struct ReedSolomonCodec {
    /// Parity bytes per block.
    parity_len: usize,
    /// Maximum data bytes per block.
    data_len: usize,
}

impl Default for ReedSolomonCodec {
    fn default() -> Self {
        Self {
            parity_len: RS_DEFAULT_PARITY_LEN,
            data_len: RS_DEFAULT_DATA_LEN,
        }
    }
}

impl ReedSolomonCodec {
    /// Create a codec with custom RS parameters.
    ///
    /// # Arguments
    ///
    /// * `parity_len` — parity bytes per block (corrects up to `parity_len / 2` errors).
    /// * `data_len` — maximum data bytes per block.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::InvalidInput`] if parameters are zero or
    /// exceed the GF(2^8) field limit of 255.
    pub fn new(parity_len: usize, data_len: usize) -> Result<Self, CryptoError> {
        if parity_len == 0 || data_len == 0 {
            return Err(CryptoError::InvalidInput(
                "Parity and data length must be greater than zero".to_string(),
            ));
        }
        if parity_len + data_len > RS_MAX_BLOCK_SIZE {
            return Err(CryptoError::InvalidInput(format!(
                "parity_len ({}) + data_len ({}) exceeds GF(2^8) limit of {}",
                parity_len, data_len, RS_MAX_BLOCK_SIZE
            )));
        }
        Ok(Self {
            parity_len,
            data_len,
        })
    }
}

impl ErrorCorrection for ReedSolomonCodec {
    /// Encode data with Reed-Solomon error correction.
    ///
    /// Splits `data` into chunks of `data_len` and appends
    /// `parity_len` parity bytes per chunk.
    fn encode(&self, data: &[u8]) -> Vec<u8> {
        let enc = reed_solomon::Encoder::new(self.parity_len);
        let mut result = Vec::new();
        for chunk in data.chunks(self.data_len) {
            let encoded = enc.encode(chunk);
            result.extend_from_slice(&encoded);
        }
        result
    }

    /// Decode and correct Reed-Solomon encoded data.
    ///
    /// Corrects up to `parity_len / 2` corrupted bytes per block
    /// and truncates the result to `original_len`.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError::ErrorCorrection`] if corruption exceeds capacity.
    fn decode(&self, encoded: &[u8], original_len: usize) -> Result<Vec<u8>, CryptoError> {
        let dec = reed_solomon::Decoder::new(self.parity_len);
        let block_size = self.data_len + self.parity_len;
        let mut result = Vec::new();

        for chunk in encoded.chunks(block_size) {
            // A valid RS block must contain at least 1 data byte + parity.
            if chunk.len() <= self.parity_len {
                return Err(CryptoError::ErrorCorrection(
                    "Encoded block too short for Reed-Solomon parity".to_string(),
                ));
            }
            let recovered = dec.correct(chunk, None).map_err(|_| {
                CryptoError::ErrorCorrection("Reed-Solomon error correction failed".to_string())
            })?;
            result.extend_from_slice(recovered.data());
        }

        result.truncate(original_len);
        Ok(result)
    }
}

// ── CryptoVault ─────────────────────────────────────────────────────

/// Compositor that wires key derivation, authenticated encryption,
/// and error correction into a complete encrypt/decrypt pipeline.
///
/// See the [module-level documentation](self) for the full blob format
/// specification, field breakdown, and security properties.
///
/// # Example
///
/// ```
/// use panoptico::crypto::CryptoVault;
///
/// let vault = CryptoVault::default();
/// let blob = vault.encrypt("password", "my-secret").unwrap();
/// let secret = vault.decrypt("password", &blob).unwrap();
/// assert_eq!(secret, "my-secret");
/// ```
pub struct CryptoVault {
    kdf: Box<dyn KeyDerivation>,
    cipher: Box<dyn AuthenticatedCipher>,
    fec: Box<dyn ErrorCorrection>,
}

impl Default for CryptoVault {
    fn default() -> Self {
        Self {
            kdf: Box::new(Argon2Kdf),
            cipher: Box::new(Aes256GcmSivCipher),
            fec: Box::new(ReedSolomonCodec::default()),
        }
    }
}

impl CryptoVault {
    /// Create a vault with custom algorithm implementations.
    ///
    /// # Arguments
    ///
    /// * `kdf` — key derivation function.
    /// * `cipher` — authenticated encryption cipher.
    /// * `fec` — forward error correction codec.
    pub fn new(
        kdf: Box<dyn KeyDerivation>,
        cipher: Box<dyn AuthenticatedCipher>,
        fec: Box<dyn ErrorCorrection>,
    ) -> Self {
        Self { kdf, cipher, fec }
    }

    /// Encrypt plaintext with a password.
    ///
    /// Applies key derivation, authenticated encryption, error correction,
    /// and base64 encoding. The derived key is zeroed from memory after use.
    ///
    /// # Arguments
    ///
    /// * `password` — user-provided password for key derivation.
    /// * `plaintext` — the data to encrypt.
    ///
    /// # Returns
    ///
    /// Base64-encoded blob.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] on any cryptographic failure or invalid input.
    pub fn encrypt(&self, password: &str, plaintext: &str) -> Result<String, CryptoError> {
        if password.is_empty() {
            return Err(CryptoError::InvalidInput(
                "Password must not be empty".to_string(),
            ));
        }

        let nonce_len = self.cipher.nonce_len();

        // Generate random salt only — the nonce is derived from the KDF.
        //
        // Defense in depth: the nonce is deterministically derived from
        // (password, salt) via Argon2, so nonce uniqueness is guaranteed as
        // long as the salt is unique (128-bit random, birthday bound ~2^64).
        // Even if the salt collides, AES-GCM-SIV is nonce-misuse resistant:
        // different plaintexts still produce different synthetic IVs,
        // preserving confidentiality.
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);

        // Derive key + nonce from password + salt (zeroed on drop).
        let kdf_output = self
            .kdf
            .derive_key(password.as_bytes(), &salt, KEY_LEN + nonce_len)?;

        // Encrypt with authenticated cipher.
        let ciphertext = self.cipher.encrypt(
            &kdf_output[..KEY_LEN],
            &kdf_output[KEY_LEN..],
            plaintext.as_bytes(),
        )?;

        // Assemble plaindata: [salt | ciphertext] — nonce is NOT stored.
        let mut plaindata = Vec::with_capacity(SALT_LEN + ciphertext.len());
        plaindata.extend_from_slice(&salt);
        plaindata.extend_from_slice(&ciphertext);

        // FEC encode.
        let original_len = plaindata.len();
        let rs_encoded = self.fec.encode(&plaindata);

        // Prepend original length (4 bytes LE) and base64 encode.
        let original_len_u32 = u32::try_from(original_len)
            .map_err(|_| CryptoError::Encoding("Data too large for length header".to_string()))?;
        let mut blob = Vec::with_capacity(4 + rs_encoded.len());
        blob.extend_from_slice(&original_len_u32.to_le_bytes());
        blob.extend_from_slice(&rs_encoded);

        Ok(STANDARD.encode(&blob))
    }

    /// Decrypt a base64-encoded blob with a password.
    ///
    /// Reverses the process of [`encrypt`](Self::encrypt): base64 decode,
    /// error correction, authenticated decryption. The derived key is
    /// zeroed from memory after use.
    ///
    /// # Arguments
    ///
    /// * `password` — the password used during encryption.
    /// * `encrypted_base64` — the base64-encoded blob.
    ///
    /// # Returns
    ///
    /// The original plaintext.
    ///
    /// # Errors
    ///
    /// Returns [`CryptoError`] on invalid base64, wrong password,
    /// corrupted data beyond FEC recovery, or invalid UTF-8.
    pub fn decrypt(&self, password: &str, encrypted_base64: &str) -> Result<String, CryptoError> {
        if password.is_empty() {
            return Err(CryptoError::InvalidInput(
                "Password must not be empty".to_string(),
            ));
        }

        let nonce_len = self.cipher.nonce_len();

        // Base64 decode.
        let blob = STANDARD
            .decode(encrypted_base64)
            .map_err(|e| CryptoError::Encoding(format!("Invalid base64: {}", e)))?;

        // Need at least 4 bytes for the length header.
        if blob.len() < 4 {
            return Err(CryptoError::Encoding(
                "Encrypted blob too short".to_string(),
            ));
        }

        // Read original plaindata length and validate.
        let len_bytes: [u8; 4] = blob[..4]
            .try_into()
            .map_err(|_| CryptoError::Encoding("Invalid length header".to_string()))?;
        let original_len = u32::from_le_bytes(len_bytes) as usize;

        // Minimum plaindata: salt + at least one byte of ciphertext.
        if original_len < SALT_LEN + 1 {
            return Err(CryptoError::InvalidInput(
                "Encrypted data too short for salt and ciphertext".to_string(),
            ));
        }

        // Validate length header against blob size — FEC encoding always adds
        // parity bytes, so original_len must be strictly less than encoded size.
        // This prevents a crafted header from causing excessive memory allocation.
        let rs_encoded_len = blob.len() - 4;
        if original_len > rs_encoded_len {
            return Err(CryptoError::InvalidInput(
                "Length header exceeds encoded data size".to_string(),
            ));
        }

        // FEC decode.
        let plaindata = self.fec.decode(&blob[4..], original_len)?;

        if plaindata.len() < SALT_LEN + 1 {
            return Err(CryptoError::InvalidInput(
                "Decrypted data too short".to_string(),
            ));
        }

        // Extract salt and ciphertext — nonce is derived, not stored.
        let salt = &plaindata[..SALT_LEN];
        let ciphertext = &plaindata[SALT_LEN..];

        // Derive key + nonce from password + salt (zeroed on drop).
        let kdf_output = self
            .kdf
            .derive_key(password.as_bytes(), salt, KEY_LEN + nonce_len)?;

        // Decrypt with authenticated cipher.
        let plaintext =
            self.cipher
                .decrypt(&kdf_output[..KEY_LEN], &kdf_output[KEY_LEN..], ciphertext)?;

        String::from_utf8(plaintext)
            .map_err(|e| CryptoError::Encoding(format!("Invalid UTF-8: {}", e)))
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── CryptoError Display ────────────────────────────────────────

    #[test]
    fn display_key_derivation_error() {
        let err = CryptoError::KeyDerivation("bad params".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("Key derivation error"));
        assert!(msg.contains("bad params"));
    }

    #[test]
    fn display_cipher_error() {
        let err = CryptoError::Cipher("wrong key".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("Cipher error"));
        assert!(msg.contains("wrong key"));
    }

    #[test]
    fn display_error_correction_error() {
        let err = CryptoError::ErrorCorrection("too corrupt".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("Error correction error"));
        assert!(msg.contains("too corrupt"));
    }

    #[test]
    fn display_encoding_error() {
        let err = CryptoError::Encoding("bad base64".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("Encoding error"));
        assert!(msg.contains("bad base64"));
    }

    #[test]
    fn display_invalid_input_error() {
        let err = CryptoError::InvalidInput("empty password".to_string());
        let msg = format!("{}", err);
        assert!(msg.contains("Invalid input"));
        assert!(msg.contains("empty password"));
    }

    #[test]
    fn crypto_error_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<CryptoError>();
        assert_sync::<CryptoError>();
    }

    #[test]
    fn crypto_error_implements_std_error() {
        let err = CryptoError::Cipher("test".to_string());
        let _std_err: &dyn std::error::Error = &err;
    }

    // ── Argon2Kdf ──────────────────────────────────────────────────

    #[test]
    fn argon2_derives_correct_key_length() {
        let kdf = Argon2Kdf;
        let key = kdf
            .derive_key(b"password", &[0u8; SALT_LEN], KEY_LEN)
            .unwrap();
        assert_eq!(key.len(), KEY_LEN);
    }

    #[test]
    fn argon2_is_deterministic_same_salt() {
        let kdf = Argon2Kdf;
        let salt = [42u8; SALT_LEN];
        let k1 = kdf.derive_key(b"password", &salt, KEY_LEN).unwrap();
        let k2 = kdf.derive_key(b"password", &salt, KEY_LEN).unwrap();
        assert_eq!(k1, k2, "Same password + salt should produce same key");
    }

    #[test]
    fn argon2_different_salt_produces_different_key() {
        let kdf = Argon2Kdf;
        let k1 = kdf
            .derive_key(b"password", &[0u8; SALT_LEN], KEY_LEN)
            .unwrap();
        let k2 = kdf
            .derive_key(b"password", &[1u8; SALT_LEN], KEY_LEN)
            .unwrap();
        assert_ne!(k1, k2, "Different salts should produce different keys");
    }

    // ── Aes256GcmSivCipher ────────────────────────────────────────────

    #[test]
    fn aes_gcm_nonce_len_is_12() {
        let cipher = Aes256GcmSivCipher;
        assert_eq!(cipher.nonce_len(), AES_GCM_SIV_NONCE_LEN);
    }

    #[test]
    fn aes_gcm_roundtrip() {
        let cipher = Aes256GcmSivCipher;
        let key = [0xABu8; KEY_LEN];
        let nonce = [0xCDu8; AES_GCM_SIV_NONCE_LEN];
        let plaintext = b"hello, AEAD!";
        let ciphertext = cipher.encrypt(&key, &nonce, plaintext).unwrap();
        let decrypted = cipher.decrypt(&key, &nonce, &ciphertext).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes_gcm_wrong_key_fails() {
        let cipher = Aes256GcmSivCipher;
        let nonce = [0xCDu8; AES_GCM_SIV_NONCE_LEN];
        let ciphertext = cipher.encrypt(&[0xABu8; KEY_LEN], &nonce, b"data").unwrap();
        let result = cipher.decrypt(&[0xFFu8; KEY_LEN], &nonce, &ciphertext);
        assert!(result.is_err(), "Wrong key should fail decryption");
    }

    #[test]
    fn aes_gcm_wrong_nonce_fails() {
        let cipher = Aes256GcmSivCipher;
        let key = [0xABu8; KEY_LEN];
        let ciphertext = cipher
            .encrypt(&key, &[0xCDu8; AES_GCM_SIV_NONCE_LEN], b"data")
            .unwrap();
        let result = cipher.decrypt(&key, &[0xFFu8; AES_GCM_SIV_NONCE_LEN], &ciphertext);
        assert!(result.is_err(), "Wrong nonce should fail decryption");
    }

    // ── ReedSolomonCodec ───────────────────────────────────────────

    #[test]
    fn rs_new_creates_codec_with_custom_params() {
        let rs = ReedSolomonCodec::new(16, 100).unwrap();
        assert_eq!(rs.parity_len, 16);
        assert_eq!(rs.data_len, 100);
    }

    #[test]
    fn rs_new_rejects_zero_parity() {
        let result = ReedSolomonCodec::new(0, 100);
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "Zero parity should be rejected"
        );
    }

    #[test]
    fn rs_new_rejects_zero_data_len() {
        let result = ReedSolomonCodec::new(32, 0);
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "Zero data_len should be rejected"
        );
    }

    #[test]
    fn rs_new_rejects_exceeding_field_size() {
        let result = ReedSolomonCodec::new(200, 200);
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "parity + data > 255 should be rejected"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("GF(2^8)"));
    }

    #[test]
    fn rs_new_accepts_max_field_size() {
        let rs = ReedSolomonCodec::new(32, 223).unwrap();
        assert_eq!(rs.parity_len + rs.data_len, RS_MAX_BLOCK_SIZE);
    }

    #[test]
    fn rs_roundtrip_preserves_data() {
        let rs = ReedSolomonCodec::default();
        let data = b"Hello, Reed-Solomon! This is a test of FEC encoding.";
        let encoded = rs.encode(data);
        let decoded = rs.decode(&encoded, data.len()).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn rs_corrects_corrupted_data() {
        let rs = ReedSolomonCodec::default();
        let data = b"FEC correction test payload for Reed-Solomon codec.";
        let mut encoded = rs.encode(data);

        // Corrupt 10 bytes in the encoded block.
        for i in 0..10 {
            encoded[i * 7] ^= 0xAA;
        }

        let decoded = rs.decode(&encoded, data.len()).unwrap();
        assert_eq!(decoded, data, "RS decode should correct 10 corrupted bytes");
    }

    #[test]
    fn rs_encoded_size_includes_parity() {
        let rs = ReedSolomonCodec::default();
        let data = vec![0u8; 100];
        let encoded = rs.encode(&data);
        // 100 bytes fits in one RS block → output = 100 + parity.
        assert_eq!(encoded.len(), 100 + RS_DEFAULT_PARITY_LEN);
    }

    #[test]
    fn rs_multi_block_encode_decode() {
        let rs = ReedSolomonCodec::default();
        // Data larger than RS_DEFAULT_DATA_LEN forces multiple blocks.
        let data = vec![42u8; RS_DEFAULT_DATA_LEN + 50];
        let encoded = rs.encode(&data);

        let block_size = RS_DEFAULT_DATA_LEN + RS_DEFAULT_PARITY_LEN;
        let expected_len = block_size + (50 + RS_DEFAULT_PARITY_LEN);
        assert_eq!(encoded.len(), expected_len);

        let decoded = rs.decode(&encoded, data.len()).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn rs_default_params() {
        let rs = ReedSolomonCodec::default();
        assert_eq!(rs.parity_len, RS_DEFAULT_PARITY_LEN);
        assert_eq!(rs.data_len, RS_DEFAULT_DATA_LEN);
    }

    #[test]
    fn rs_decode_rejects_chunk_not_larger_than_parity() {
        let rs = ReedSolomonCodec::default();
        // 32 bytes = exactly parity_len, no room for data.
        let short_data = vec![0u8; RS_DEFAULT_PARITY_LEN];
        let result = rs.decode(&short_data, 1);
        assert!(
            matches!(result, Err(CryptoError::ErrorCorrection(_))),
            "Chunk not larger than parity should fail"
        );
    }

    // ── CryptoVault ────────────────────────────────────────────────

    #[test]
    fn vault_new_with_custom_algorithms() {
        let vault = CryptoVault::new(
            Box::new(Argon2Kdf),
            Box::new(Aes256GcmSivCipher),
            Box::new(ReedSolomonCodec::default()),
        );
        let encrypted = vault.encrypt("password", "test-data").unwrap();
        let decrypted = vault.decrypt("password", &encrypted).unwrap();
        assert_eq!(decrypted, "test-data");
    }

    #[test]
    fn vault_encrypt_returns_nonempty_string() {
        let vault = CryptoVault::default();
        let result = vault.encrypt("password", "sk-ant-api03-xxxxx").unwrap();
        assert!(!result.is_empty());
    }

    #[test]
    fn vault_encrypt_produces_valid_base64() {
        let vault = CryptoVault::default();
        let result = vault.encrypt("password", "sk-ant-api03-xxxxx").unwrap();
        let decoded = STANDARD.decode(&result);
        assert!(decoded.is_ok(), "Output should be valid base64");
    }

    #[test]
    fn vault_encrypt_different_calls_produce_different_output() {
        let vault = CryptoVault::default();
        let a = vault.encrypt("password", "sk-ant-api03-xxxxx").unwrap();
        let b = vault.encrypt("password", "sk-ant-api03-xxxxx").unwrap();
        assert_ne!(a, b, "Random salt/nonce should produce different blobs");
    }

    #[test]
    fn vault_decrypt_roundtrip() {
        let vault = CryptoVault::default();
        let api_key = "sk-ant-api03-real-key-here";
        let password = "my-secure-password";
        let encrypted = vault.encrypt(password, api_key).unwrap();
        let decrypted = vault.decrypt(password, &encrypted).unwrap();
        assert_eq!(decrypted, api_key);
    }

    #[test]
    fn vault_decrypt_wrong_password_fails() {
        let vault = CryptoVault::default();
        let encrypted = vault
            .encrypt("correct-password", "sk-ant-api03-key")
            .unwrap();
        let result = vault.decrypt("wrong-password", &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn vault_decrypt_invalid_base64_fails() {
        let vault = CryptoVault::default();
        let result = vault.decrypt("password", "!!!not-base64!!!");
        assert!(
            matches!(result, Err(CryptoError::Encoding(_))),
            "Invalid base64 should return CryptoError::Encoding"
        );
    }

    #[test]
    fn vault_decrypt_short_blob_fails() {
        let vault = CryptoVault::default();
        let short_blob = STANDARD.encode([0u8; 5]);
        let result = vault.decrypt("password", &short_blob);
        assert!(result.is_err());
    }

    #[test]
    fn vault_decrypt_empty_input_fails() {
        let vault = CryptoVault::default();
        let result = vault.decrypt("password", "");
        assert!(result.is_err());
    }

    #[test]
    fn vault_decrypt_roundtrip_empty_plaintext() {
        let vault = CryptoVault::default();
        let encrypted = vault.encrypt("password", "").unwrap();
        let decrypted = vault.decrypt("password", &encrypted).unwrap();
        assert_eq!(decrypted, "");
    }

    #[test]
    fn vault_decrypt_empty_password_fails() {
        let vault = CryptoVault::default();
        let encrypted = vault.encrypt("password", "test-data").unwrap();
        let result = vault.decrypt("", &encrypted);
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "Empty password on decrypt should return CryptoError::InvalidInput"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("Password must not be empty"));
    }

    #[test]
    fn vault_decrypt_tampered_length_header_fails() {
        let vault = CryptoVault::default();
        let encrypted = vault.encrypt("password", "test-data").unwrap();
        let mut blob = STANDARD.decode(&encrypted).unwrap();
        // Set original_len to a value larger than the RS payload.
        let huge_len = (blob.len() as u32) + 1000;
        blob[..4].copy_from_slice(&huge_len.to_le_bytes());
        let tampered = STANDARD.encode(&blob);
        let result = vault.decrypt("password", &tampered);
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "Tampered length header should fail"
        );
    }

    #[test]
    fn vault_encrypt_empty_password_fails() {
        let vault = CryptoVault::default();
        let result = vault.encrypt("", "sk-ant-api03-key");
        assert!(
            matches!(result, Err(CryptoError::InvalidInput(_))),
            "Empty password should return CryptoError::InvalidInput"
        );
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("Password must not be empty"));
    }

    #[test]
    fn vault_rs_corrects_single_corrupted_byte() {
        let vault = CryptoVault::default();
        let password = "rs-test-password";
        let api_key = "sk-ant-api03-rs-single";
        let encrypted = vault.encrypt(password, api_key).unwrap();

        let corrupted = corrupt_blob(&encrypted, &[0]);
        let decrypted = vault.decrypt(password, &corrupted).unwrap();
        assert_eq!(decrypted, api_key);
    }

    #[test]
    fn vault_rs_corrects_up_to_16_bytes() {
        let vault = CryptoVault::default();
        let password = "rs-test-password";
        let api_key = "sk-ant-api03-rs-max";
        let encrypted = vault.encrypt(password, api_key).unwrap();

        let offsets: Vec<usize> = (0..16).map(|i| i * 5).collect();
        let corrupted = corrupt_blob(&encrypted, &offsets);
        let decrypted = vault.decrypt(password, &corrupted).unwrap();
        assert_eq!(decrypted, api_key);
    }

    #[test]
    fn vault_rs_fails_when_corruption_exceeds_capacity() {
        let vault = CryptoVault::default();
        let password = "rs-test-password";
        let api_key = "sk-ant-api03-rs-overflow";
        let encrypted = vault.encrypt(password, api_key).unwrap();

        let offsets: Vec<usize> = (0..17).map(|i| i * 5).collect();
        let corrupted = corrupt_blob(&encrypted, &offsets);
        let result = vault.decrypt(password, &corrupted);
        assert!(result.is_err());
    }

    #[test]
    fn vault_blob_structure_has_length_header_and_rs_blocks() {
        let vault = CryptoVault::default();
        let password = "structure-test";
        let api_key = "sk-ant-api03-structure";
        let encrypted = vault.encrypt(password, api_key).unwrap();

        let blob = STANDARD.decode(&encrypted).unwrap();

        // First 4 bytes: original plaindata length (LE u32).
        let len_bytes: [u8; 4] = blob[..4].try_into().unwrap();
        let original_len = u32::from_le_bytes(len_bytes) as usize;

        // Original plaindata = salt(16) + ciphertext.
        // Ciphertext = api_key.len() + GCM-SIV tag (16 bytes).
        // Nonce is derived from KDF, not stored in the blob.
        let expected_plaindata_len = SALT_LEN + api_key.len() + 16;
        assert_eq!(original_len, expected_plaindata_len);

        // RS-encoded data follows the 4-byte header.
        let rs_data_len = blob.len() - 4;
        let block_size = RS_DEFAULT_DATA_LEN + RS_DEFAULT_PARITY_LEN;
        let num_full_blocks = original_len / RS_DEFAULT_DATA_LEN;
        let remainder = original_len % RS_DEFAULT_DATA_LEN;
        let expected_rs_len = num_full_blocks * block_size
            + if remainder > 0 {
                remainder + RS_DEFAULT_PARITY_LEN
            } else {
                0
            };
        assert_eq!(rs_data_len, expected_rs_len);
    }

    #[test]
    fn vault_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<CryptoVault>();
        assert_sync::<CryptoVault>();
    }

    // ── Helper ─────────────────────────────────────────────────────

    /// Corrupt bytes at given offsets within the RS-encoded portion of an
    /// encrypted blob (after the 4-byte length header).
    fn corrupt_blob(encrypted_base64: &str, byte_offsets: &[usize]) -> String {
        let mut blob = STANDARD.decode(encrypted_base64).unwrap();
        for &offset in byte_offsets {
            let idx = 4 + offset;
            assert!(idx < blob.len(), "offset {} out of bounds", offset);
            blob[idx] ^= 0xFF;
        }
        STANDARD.encode(&blob)
    }
}
