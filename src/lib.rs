// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! AI Code Reviewer — automated code review using Claude via Azure AI Foundry.
//!
//! This crate provides the core pipeline for AI-powered PR code review:
//! diff extraction, hunk parsing, batch grouping, prompt construction,
//! hallucination validation, and cost tracking.
//!
//! # Architecture
//!
//! The main entry point is [`PrAiReviewer`], which orchestrates the full
//! review pipeline. Create an instance with a [`config::ReviewConfig`]
//! and call [`PrAiReviewer::run`] with a [`Command`].
//!
//! ```no_run
//! use panoptico::{PrAiReviewer, Command};
//! use panoptico::config::ReviewConfig;
//!
//! # async fn example() -> Result<(), panoptico::error::ReviewError> {
//! let config = ReviewConfig::default();
//! let reviewer = PrAiReviewer::new(config);
//! reviewer.run(Command::Test).await?;
//! # Ok(())
//! # }
//! ```

pub mod backend;
pub mod batch;
pub mod config;
pub mod context;
pub mod credential;
pub mod crypto;
pub mod error;
pub mod finding_id;
pub mod git;
pub mod hunk;
pub mod languages;
pub mod metrics;
pub mod prompt;
pub mod reviewer;
pub mod validator;

// Re-export the primary public API at crate root.
pub use reviewer::{Command, PrAiReviewer};

/// Shared synchronization primitives for tests that mutate process-global state.
///
/// `std::env::set_current_dir` and `std::env::set_var` are process-global.
/// Tests that modify these must acquire the corresponding mutex to prevent
/// race conditions when `cargo test` runs tests in parallel.
///
/// # Lock ordering
///
/// `CWD_MUTEX` and `ENV_MUTEX` are currently independent — no test
/// acquires both. If a future test needs both locks, acquire
/// `CWD_MUTEX` first, then `ENV_MUTEX`, to prevent deadlocks.
#[cfg(test)]
pub(crate) mod test_sync {
    /// Mutex to serialize tests that call [`std::env::set_current_dir`].
    ///
    /// Acquire **before** [`ENV_MUTEX`] if both are needed (see module docs).
    pub static CWD_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Mutex to serialize tests that call [`std::env::set_var`] / [`std::env::remove_var`].
    ///
    /// Acquire **after** [`CWD_MUTEX`] if both are needed (see module docs).
    pub static ENV_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// RAII guard that restores the working directory on drop (including panics).
    pub struct CwdGuard {
        original: std::path::PathBuf,
    }

    impl CwdGuard {
        /// Change to `new_dir` and remember the previous directory.
        pub fn new(new_dir: &std::path::Path) -> Self {
            let original = std::env::current_dir().unwrap();
            std::env::set_current_dir(new_dir).unwrap();
            Self { original }
        }
    }

    impl Drop for CwdGuard {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }
}
