// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-11

//! Secure credential storage — resolves API keys from multiple sources.
//!
//! Supports four credential sources: environment variables, OS keyring,
//! AES-256-GCM encrypted blobs with Argon2 key derivation and
//! Reed-Solomon error correction, and Azure Key Vault.
//!
//! Cryptographic operations are delegated to [`crate::crypto::CryptoVault`].
//!
//! # Credential Sources
//!
//! | Source | Use Case |
//! |--------|----------|
//! | `Env` | CI/CD pipelines (default); decrypts when `--key-password` provided |
//! | `Keyring` | Developer local (interactive) |
//! | `Encrypted` | Portable (password-protected TOML blob) |
//! | `Vault` | Enterprise (Azure managed identity) |

use crate::crypto::CryptoVault;
use crate::error::ReviewError;

/// Source from which the API key is resolved at runtime.
///
/// Configured via `credential_source` in the `[azure]` TOML section.
///
/// # Variants
///
/// * `Env` — reads `AZURE_AI_API_KEY` from environment.
/// * `Keyring` — reads from OS credential store.
/// * `Encrypted` — decrypts a base64 blob stored in TOML.
/// * `Vault` — fetches from Azure Key Vault via managed identity.
#[derive(Debug, Clone)]
pub enum CredentialSource {
    /// Read API key from `AZURE_AI_API_KEY` environment variable.
    ///
    /// When a password is provided, the env var value is treated as an
    /// encrypted base64 blob and decrypted (same as [`Encrypted`](Self::Encrypted)).
    Env,
    /// Read API key from OS credential store (keyring).
    Keyring,
    /// Decrypt API key from an AES-256-GCM + Reed-Solomon base64 blob.
    Encrypted {
        /// Base64-encoded encrypted blob.
        api_key_encrypted: String,
    },
    /// Fetch API key from Azure Key Vault.
    Vault {
        /// Key Vault URL (e.g., `https://myvault.vault.azure.net`).
        vault_url: String,
        /// Secret name in the vault.
        vault_secret_name: String,
    },
}

impl CredentialSource {
    /// Resolve the API key from this credential source.
    ///
    /// # Arguments
    ///
    /// * `password` — required for `Encrypted`; optional for `Env`
    ///   (triggers decryption of the env var value); ignored otherwise.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Config`] if the key cannot be resolved
    /// (missing env var, wrong password, vault unreachable, etc.).
    pub async fn resolve(&self, password: Option<&str>) -> Result<String, ReviewError> {
        match self {
            CredentialSource::Env => {
                let raw = std::env::var("AZURE_AI_API_KEY").map_err(|_| {
                    ReviewError::Config("AZURE_AI_API_KEY environment variable not set".to_string())
                })?;
                match password {
                    Some(pwd) => decrypt_api_key(pwd, &raw),
                    None => Ok(raw),
                }
            }
            CredentialSource::Keyring => Err(ReviewError::Config(
                "Keyring credential source not yet implemented".to_string(),
            )),
            CredentialSource::Encrypted { api_key_encrypted } => {
                let password = password.ok_or_else(|| {
                    ReviewError::Config(
                        "Password required for encrypted credential source".to_string(),
                    )
                })?;
                decrypt_api_key(password, api_key_encrypted)
            }
            CredentialSource::Vault {
                vault_url,
                vault_secret_name,
            } => Err(ReviewError::Config(format!(
                "Vault credential source not yet implemented (vault: {}, secret: {})",
                vault_url, vault_secret_name
            ))),
        }
    }
}

/// Encrypt an API key with a password.
///
/// Delegates to [`CryptoVault::encrypt`] with default algorithms
/// (Argon2 + AES-256-GCM + Reed-Solomon).
///
/// # Arguments
///
/// * `password` — user-provided password for key derivation.
/// * `api_key` — the plaintext API key to encrypt.
///
/// # Returns
///
/// Base64-encoded blob suitable for TOML `api_key_encrypted` field.
///
/// # Errors
///
/// Returns [`ReviewError::Config`] on cryptographic failures.
pub fn encrypt_api_key(password: &str, api_key: &str) -> Result<String, ReviewError> {
    let vault = CryptoVault::default();
    Ok(vault.encrypt(password, api_key)?)
}

/// Decrypt an API key from a base64-encoded encrypted blob.
///
/// Delegates to [`CryptoVault::decrypt`] with default algorithms
/// (Argon2 + AES-256-GCM + Reed-Solomon).
///
/// # Arguments
///
/// * `password` — the password used during encryption.
/// * `encrypted_base64` — the base64 blob from TOML config.
///
/// # Returns
///
/// The original plaintext API key.
///
/// # Errors
///
/// Returns [`ReviewError::Config`] on invalid base64, wrong password,
/// corrupted data beyond RS recovery, or invalid UTF-8.
pub fn decrypt_api_key(password: &str, encrypted_base64: &str) -> Result<String, ReviewError> {
    let vault = CryptoVault::default();
    Ok(vault.decrypt(password, encrypted_base64)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_sync::ENV_MUTEX;

    // ── CredentialSource construction ────────────────────────────────

    #[test]
    fn env_variant_is_constructible() {
        let source = CredentialSource::Env;
        // Debug is derived — format should not panic.
        let debug = format!("{:?}", source);
        assert!(debug.contains("Env"), "Debug output should contain 'Env'");
    }

    #[test]
    fn keyring_variant_is_constructible() {
        let source = CredentialSource::Keyring;
        let debug = format!("{:?}", source);
        assert!(
            debug.contains("Keyring"),
            "Debug output should contain 'Keyring'"
        );
    }

    #[test]
    fn encrypted_variant_holds_blob() {
        let blob = "c2FsdC4uLm5vbmNlLi4u".to_string();
        let source = CredentialSource::Encrypted {
            api_key_encrypted: blob.clone(),
        };
        if let CredentialSource::Encrypted { api_key_encrypted } = &source {
            assert_eq!(api_key_encrypted, &blob);
        } else {
            panic!("Expected Encrypted variant");
        }
    }

    #[test]
    fn vault_variant_holds_url_and_name() {
        let url = "https://myvault.vault.azure.net".to_string();
        let name = "panoptico-api-key".to_string();
        let source = CredentialSource::Vault {
            vault_url: url.clone(),
            vault_secret_name: name.clone(),
        };
        if let CredentialSource::Vault {
            vault_url,
            vault_secret_name,
        } = &source
        {
            assert_eq!(vault_url, &url);
            assert_eq!(vault_secret_name, &name);
        } else {
            panic!("Expected Vault variant");
        }
    }

    // ── CredentialSource::resolve ────────────────────────────────────

    #[tokio::test]
    async fn resolve_env_reads_environment_variable() {
        let key = "test-api-key-12345";
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::set_var("AZURE_AI_API_KEY", key);
        }
        let source = CredentialSource::Env;
        let result = source.resolve(None).await;
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::remove_var("AZURE_AI_API_KEY");
        }
        assert_eq!(result.unwrap(), key);
    }

    #[tokio::test]
    async fn resolve_env_missing_var_returns_config_error() {
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::remove_var("AZURE_AI_API_KEY");
        }
        let source = CredentialSource::Env;
        let result = source.resolve(None).await;
        assert!(
            matches!(result, Err(ReviewError::Config(_))),
            "Missing env var should return ReviewError::Config"
        );
    }

    #[tokio::test]
    async fn resolve_encrypted_without_password_returns_error() {
        let source = CredentialSource::Encrypted {
            api_key_encrypted: "dW51c2Vk".to_string(),
        };
        let result = source.resolve(None).await;
        assert!(
            matches!(result, Err(ReviewError::Config(_))),
            "Encrypted source without password should return ReviewError::Config"
        );
    }

    #[tokio::test]
    async fn resolve_encrypted_with_password_decrypts() {
        let api_key = "sk-ant-api03-test-key";
        let password = "strong-password-123";
        let encrypted = encrypt_api_key(password, api_key).unwrap();
        let source = CredentialSource::Encrypted {
            api_key_encrypted: encrypted,
        };
        let result = source.resolve(Some(password)).await.unwrap();
        assert_eq!(result, api_key);
    }

    // ── Env + password (encrypted blob in env var) ───────────────────

    #[tokio::test]
    async fn resolve_env_with_password_decrypts_blob() {
        let api_key = "sk-ant-api03-env-encrypted";
        let password = "env-password-456";
        let blob = encrypt_api_key(password, api_key).unwrap();
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::set_var("AZURE_AI_API_KEY", &blob);
        }
        let source = CredentialSource::Env;
        let result = source.resolve(Some(password)).await;
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::remove_var("AZURE_AI_API_KEY");
        }
        assert_eq!(result.unwrap(), api_key);
    }

    #[tokio::test]
    async fn resolve_env_with_password_bad_blob_returns_error() {
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::set_var("AZURE_AI_API_KEY", "not-a-valid-encrypted-blob");
        }
        let source = CredentialSource::Env;
        let result = source.resolve(Some("any-password")).await;
        {
            let _guard = ENV_MUTEX.lock().unwrap();
            std::env::remove_var("AZURE_AI_API_KEY");
        }
        assert!(
            matches!(result, Err(ReviewError::Config(_) | ReviewError::Parse(_))),
            "Bad encrypted blob in env var should return a crypto error: {:?}",
            result
        );
    }
}
