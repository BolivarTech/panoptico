// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Error types for the AI code reviewer.
//!
//! Provides [`ReviewError`], the unified error type used across all
//! modules in the review pipeline.

use std::fmt;

/// Unified error type for all reviewer operations.
#[derive(Debug)]
pub enum ReviewError {
    /// Git diff extraction failure.
    GitDiff(String),
    /// API communication failure.
    Api(String),
    /// JSON parsing failure.
    Parse(String),
    /// Configuration error.
    Config(String),
    /// Azure DevOps API failure.
    AzureDevOps(String),
    /// IO operation failure.
    Io(std::io::Error),
}

impl fmt::Display for ReviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GitDiff(msg) => write!(f, "Git diff error: {}", msg),
            Self::Api(msg) => write!(f, "API error: {}", msg),
            Self::Parse(msg) => write!(f, "Parse error: {}", msg),
            Self::Config(msg) => write!(f, "Config error: {}", msg),
            Self::AzureDevOps(msg) => write!(f, "Azure DevOps error: {}", msg),
            Self::Io(err) => write!(f, "IO error: {}", err),
        }
    }
}

impl std::error::Error for ReviewError {}

impl From<std::io::Error> for ReviewError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for ReviewError {
    fn from(err: serde_json::Error) -> Self {
        Self::Parse(err.to_string())
    }
}

impl From<reqwest::Error> for ReviewError {
    fn from(err: reqwest::Error) -> Self {
        Self::Api(err.to_string())
    }
}

impl From<crate::crypto::CryptoError> for ReviewError {
    fn from(err: crate::crypto::CryptoError) -> Self {
        use crate::crypto::CryptoError;
        match err {
            CryptoError::Encoding(ref msg) => Self::Parse(format!("Crypto encoding: {}", msg)),
            CryptoError::Cipher(ref msg) => Self::Config(format!("Decryption failed: {}", msg)),
            CryptoError::KeyDerivation(ref msg) => {
                Self::Config(format!("Key derivation failed: {}", msg))
            }
            CryptoError::ErrorCorrection(ref msg) => {
                Self::Config(format!("Error correction failed: {}", msg))
            }
            CryptoError::InvalidInput(ref msg) => {
                Self::Config(format!("Invalid crypto input: {}", msg))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_git_diff_error() {
        let err = ReviewError::GitDiff("branch not found".to_string());
        let msg = format!("{}", err);
        assert!(
            msg.contains("branch not found"),
            "GitDiff display should contain the inner message"
        );
    }

    #[test]
    fn display_api_error() {
        let err = ReviewError::Api("timeout".to_string());
        let msg = format!("{}", err);
        assert!(
            msg.contains("timeout"),
            "Api display should contain the inner message"
        );
    }

    #[test]
    fn display_parse_error() {
        let err = ReviewError::Parse("invalid JSON".to_string());
        let msg = format!("{}", err);
        assert!(
            msg.contains("invalid JSON"),
            "Parse display should contain the inner message"
        );
    }

    #[test]
    fn display_config_error() {
        let err = ReviewError::Config("missing field".to_string());
        let msg = format!("{}", err);
        assert!(
            msg.contains("missing field"),
            "Config display should contain the inner message"
        );
    }

    #[test]
    fn display_azure_devops_error() {
        let err = ReviewError::AzureDevOps("401 unauthorized".to_string());
        let msg = format!("{}", err);
        assert!(
            msg.contains("401 unauthorized"),
            "AzureDevOps display should contain the inner message"
        );
    }

    #[test]
    fn display_io_error() {
        let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = ReviewError::Io(inner);
        let msg = format!("{}", err);
        assert!(
            msg.contains("file missing"),
            "Io display should contain the inner message"
        );
    }

    #[test]
    fn from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let err: ReviewError = io_err.into();
        assert!(
            matches!(err, ReviewError::Io(_)),
            "io::Error should convert to ReviewError::Io"
        );
    }

    #[test]
    fn from_serde_json_error() {
        let json_err = serde_json::from_str::<serde_json::Value>("{{bad}}").unwrap_err();
        let err: ReviewError = json_err.into();
        assert!(
            matches!(err, ReviewError::Parse(_)),
            "serde_json::Error should convert to ReviewError::Parse"
        );
    }

    #[test]
    fn error_is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ReviewError>();
        assert_sync::<ReviewError>();
    }

    #[test]
    fn from_crypto_cipher_error_maps_to_config() {
        let crypto_err = crate::crypto::CryptoError::Cipher("test failure".to_string());
        let err: ReviewError = crypto_err.into();
        assert!(
            matches!(err, ReviewError::Config(ref msg) if msg.contains("Decryption failed")),
            "Cipher error should map to Config: {:?}",
            err
        );
        let msg = format!("{}", err);
        assert!(msg.contains("test failure"));
    }

    #[test]
    fn from_crypto_encoding_error_maps_to_parse() {
        let crypto_err = crate::crypto::CryptoError::Encoding("bad base64".to_string());
        let err: ReviewError = crypto_err.into();
        assert!(
            matches!(err, ReviewError::Parse(ref msg) if msg.contains("Crypto encoding")),
            "Encoding error should map to Parse: {:?}",
            err
        );
        let msg = format!("{}", err);
        assert!(msg.contains("bad base64"));
    }

    #[test]
    fn from_crypto_key_derivation_error_maps_to_config() {
        let crypto_err = crate::crypto::CryptoError::KeyDerivation("bad params".to_string());
        let err: ReviewError = crypto_err.into();
        assert!(
            matches!(err, ReviewError::Config(ref msg) if msg.contains("Key derivation failed")),
            "KeyDerivation error should map to Config: {:?}",
            err
        );
    }

    #[test]
    fn from_crypto_error_correction_maps_to_config() {
        let crypto_err = crate::crypto::CryptoError::ErrorCorrection("beyond capacity".to_string());
        let err: ReviewError = crypto_err.into();
        assert!(
            matches!(err, ReviewError::Config(ref msg) if msg.contains("Error correction failed")),
            "ErrorCorrection should map to Config: {:?}",
            err
        );
    }

    #[test]
    fn from_crypto_invalid_input_maps_to_config() {
        let crypto_err = crate::crypto::CryptoError::InvalidInput("empty password".to_string());
        let err: ReviewError = crypto_err.into();
        assert!(
            matches!(err, ReviewError::Config(ref msg) if msg.contains("Invalid crypto input")),
            "InvalidInput should map to Config: {:?}",
            err
        );
    }

    #[test]
    fn error_implements_std_error() {
        let err = ReviewError::Api("test".to_string());
        let _std_err: &dyn std::error::Error = &err;
    }
}
