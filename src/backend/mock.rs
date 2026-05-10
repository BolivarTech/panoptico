// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-10

//! Mock backend for testing the review pipeline without real API calls.
//!
//! Provides [`MockBackend`], a configurable [`ReviewBackend`] implementation
//! that returns pre-programmed responses, tracks calls, and supports
//! assertions for verifying pipeline behavior.
//!
//! # Examples
//!
//! ```ignore
//! use crate::backend::mock::{MockBackend, make_review_response};
//!
//! let mock = MockBackend::builder()
//!     .with_success(make_review_response("No issues", 0))
//!     .build();
//!
//! // Use mock as `dyn ReviewBackend` in pipeline tests...
//!
//! mock.assert_call_count(1);
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{
    CodeReview, Finding, ReviewBackend, ReviewRequest, ReviewResponse, Severity, TokenUsage,
};
use crate::error::ReviewError;
use crate::finding_id::Category;

/// Clonable error type for mock configuration.
///
/// Wraps error variants that map to [`ReviewError`], but supports `Clone`
/// (unlike `ReviewError` which contains `std::io::Error`).
#[derive(Debug, Clone)]
pub enum MockError {
    /// Maps to [`ReviewError::Api`].
    Api(String),
    /// Maps to [`ReviewError::Parse`].
    Parse(String),
    /// Maps to [`ReviewError::Config`].
    Config(String),
}

impl From<MockError> for ReviewError {
    fn from(err: MockError) -> Self {
        match err {
            MockError::Api(msg) => ReviewError::Api(msg),
            MockError::Parse(msg) => ReviewError::Parse(msg),
            MockError::Config(msg) => ReviewError::Config(msg),
        }
    }
}

/// A single programmed behavior for the mock backend.
#[derive(Debug, Clone)]
pub enum MockBehavior {
    /// Return a successful response.
    Success(ReviewResponse),
    /// Return an error.
    Error(MockError),
    /// Return a successful response after a delay.
    DelayedSuccess {
        /// The response to return.
        response: ReviewResponse,
        /// Duration to sleep before returning.
        delay: Duration,
    },
}

/// Records a single call made to the mock backend.
#[derive(Debug, Clone)]
pub struct RecordedCall {
    /// The review request that was sent.
    pub request: ReviewRequest,
}

/// Thread-safe internal state for call tracking.
#[derive(Debug, Default)]
struct MockState {
    /// All recorded calls, in order.
    calls: Vec<RecordedCall>,
}

/// A configurable mock implementation of [`ReviewBackend`].
///
/// Cycles through configured behaviors on each `review()` call and
/// records all requests for later assertion. Thread-safe for use in
/// concurrent pipeline tests.
#[derive(Debug)]
pub struct MockBackend {
    /// Immutable list of behaviors to cycle through.
    behaviors: Vec<MockBehavior>,
    /// Thread-safe mutable state for call tracking.
    state: Arc<Mutex<MockState>>,
}

impl MockBackend {
    /// Create a new builder for configuring mock behaviors.
    pub fn builder() -> MockBackendBuilder {
        MockBackendBuilder {
            behaviors: Vec::new(),
        }
    }

    /// Return the number of times `review()` has been called.
    pub fn call_count(&self) -> usize {
        self.state.lock().unwrap().calls.len()
    }

    /// Return a clone of all recorded calls.
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.state.lock().unwrap().calls.clone()
    }

    /// Return a clone of the recorded call at `index`.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of bounds.
    pub fn call(&self, index: usize) -> RecordedCall {
        let state = self.state.lock().unwrap();
        state.calls[index].clone()
    }

    /// Assert that `review()` was called exactly `expected` times.
    ///
    /// # Panics
    ///
    /// Panics if the actual call count differs from `expected`.
    pub fn assert_call_count(&self, expected: usize) {
        let actual = self.call_count();
        assert_eq!(
            actual, expected,
            "Expected {} calls to MockBackend::review(), got {}",
            expected, actual
        );
    }
}

#[async_trait::async_trait]
impl ReviewBackend for MockBackend {
    /// Execute a mock review, returning the next configured behavior.
    ///
    /// Records the request and selects a behavior by cycling through
    /// the configured list: `call_index % behaviors.len()`.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError`] when the selected behavior is [`MockBehavior::Error`].
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError> {
        let call_index = {
            let mut state = self.state.lock().unwrap();
            let index = state.calls.len();
            state.calls.push(RecordedCall {
                request: request.clone(),
            });
            index
        };

        let behavior = &self.behaviors[call_index % self.behaviors.len()];

        match behavior {
            MockBehavior::Success(response) => Ok(response.clone()),
            MockBehavior::Error(err) => Err(err.clone().into()),
            MockBehavior::DelayedSuccess { response, delay } => {
                tokio::time::sleep(*delay).await;
                Ok(response.clone())
            }
        }
    }
}

/// Builder for constructing a [`MockBackend`] with configured behaviors.
#[derive(Debug)]
pub struct MockBackendBuilder {
    behaviors: Vec<MockBehavior>,
}

impl MockBackendBuilder {
    /// Add a success behavior that returns the given response.
    ///
    /// # Arguments
    ///
    /// * `response` - The response to return on this call.
    ///
    /// # Returns
    ///
    /// The builder for chaining.
    pub fn with_success(mut self, response: ReviewResponse) -> Self {
        self.behaviors.push(MockBehavior::Success(response));
        self
    }

    /// Add an error behavior that returns the given error.
    ///
    /// # Arguments
    ///
    /// * `error` - The error to return on this call.
    ///
    /// # Returns
    ///
    /// The builder for chaining.
    pub fn with_error(mut self, error: MockError) -> Self {
        self.behaviors.push(MockBehavior::Error(error));
        self
    }

    /// Add a delayed success behavior that sleeps before returning.
    ///
    /// # Arguments
    ///
    /// * `response` - The response to return after the delay.
    /// * `delay` - Duration to sleep before returning the response.
    ///
    /// # Returns
    ///
    /// The builder for chaining.
    pub fn with_delayed_success(mut self, response: ReviewResponse, delay: Duration) -> Self {
        self.behaviors
            .push(MockBehavior::DelayedSuccess { response, delay });
        self
    }

    /// Build the [`MockBackend`].
    ///
    /// # Panics
    ///
    /// Panics if no behaviors were configured.
    pub fn build(self) -> MockBackend {
        assert!(
            !self.behaviors.is_empty(),
            "MockBackend requires at least one behavior"
        );
        MockBackend {
            behaviors: self.behaviors,
            state: Arc::new(Mutex::new(MockState::default())),
        }
    }
}

/// Create a [`ReviewResponse`] with generated findings and default token usage.
///
/// # Arguments
///
/// * `summary` - Summary text for the review.
/// * `finding_count` - Number of generic findings to generate.
///
/// # Returns
///
/// A [`ReviewResponse`] with zeroed [`TokenUsage`].
pub fn make_review_response(summary: &str, finding_count: usize) -> ReviewResponse {
    make_review_response_with_usage(summary, finding_count, TokenUsage::default())
}

/// Create a [`ReviewResponse`] with generated findings and custom token usage.
///
/// # Arguments
///
/// * `summary` - Summary text for the review.
/// * `finding_count` - Number of generic findings to generate.
/// * `usage` - Token usage statistics.
///
/// # Returns
///
/// A [`ReviewResponse`] with `finding_count` generic [`Finding`] values.
pub fn make_review_response_with_usage(
    summary: &str,
    finding_count: usize,
    usage: TokenUsage,
) -> ReviewResponse {
    let findings = (0..finding_count)
        .map(|i| Finding {
            severity: Severity::Warning,
            file: format!("file{}.c", i),
            line: (i + 1) as u32,
            title: format!("Finding {}", i),
            description: format!("Description for finding {}", i),
            suggestion: format!("Fix for finding {}", i),
            category: Category::default(),
            finding_id: String::new(),
            reasoning: String::new(),
        })
        .collect();

    ReviewResponse {
        review: CodeReview {
            summary: summary.to_string(),
            findings,
        },
        usage,
    }
}

/// Create a single [`Finding`] with sensible defaults for tests.
///
/// Sets severity to `Warning`, category to `Other`, and uses
/// empty strings for description, suggestion, finding_id, and reasoning.
///
/// # Arguments
///
/// * `file` - Source file path.
/// * `line` - Line number.
/// * `title` - Short title for the finding.
///
/// # Returns
///
/// A [`Finding`] suitable for test assertions and pipeline exercises.
pub fn make_test_finding(file: &str, line: u32, title: &str) -> Finding {
    Finding {
        severity: Severity::Warning,
        file: file.to_string(),
        line,
        title: title.to_string(),
        description: "Test description".to_string(),
        suggestion: "Test suggestion".to_string(),
        category: Category::default(),
        finding_id: String::new(),
        reasoning: String::new(),
    }
}

/// Create a default [`ReviewRequest`] suitable for tests.
///
/// # Returns
///
/// A [`ReviewRequest`] with placeholder values for all fields.
pub fn make_review_request() -> ReviewRequest {
    ReviewRequest {
        system_prompt: "You are a code reviewer.".to_string(),
        custom_instructions: None,
        diff_content: "--- a/main.c\n+++ b/main.c\n@@ -1,3 +1,4 @@\n+#include <stdio.h>"
            .to_string(),
        batch_number: 1,
        total_batches: 1,
        file_info: "main.c".to_string(),
        model: "claude-sonnet-4-5".to_string(),
        max_tokens: 4096,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn returns_configured_success() {
        let response = make_review_response("All clear", 1);
        let mock = MockBackend::builder().with_success(response).build();

        let result = mock.review(&make_review_request()).await.unwrap();

        assert_eq!(result.review.summary, "All clear");
        assert_eq!(result.review.findings.len(), 1);
    }

    #[tokio::test]
    async fn returns_configured_error() {
        let mock = MockBackend::builder()
            .with_error(MockError::Api("connection refused".to_string()))
            .build();

        let result = mock.review(&make_review_request()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, ReviewError::Api(ref msg) if msg.contains("connection refused")),
            "Expected Api error with 'connection refused', got: {:?}",
            err
        );
    }

    #[tokio::test]
    async fn cycles_through_behaviors() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("First", 0))
            .with_success(make_review_response("Second", 0))
            .build();

        let r1 = mock.review(&make_review_request()).await.unwrap();
        let r2 = mock.review(&make_review_request()).await.unwrap();
        let r3 = mock.review(&make_review_request()).await.unwrap();
        let r4 = mock.review(&make_review_request()).await.unwrap();

        assert_eq!(r1.review.summary, "First");
        assert_eq!(r2.review.summary, "Second");
        assert_eq!(r3.review.summary, "First", "Should cycle back to first");
        assert_eq!(r4.review.summary, "Second", "Should cycle back to second");
    }

    #[tokio::test]
    async fn tracks_call_count() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("OK", 0))
            .build();

        assert_eq!(mock.call_count(), 0);
        mock.review(&make_review_request()).await.unwrap();
        assert_eq!(mock.call_count(), 1);
        mock.review(&make_review_request()).await.unwrap();
        assert_eq!(mock.call_count(), 2);
    }

    #[tokio::test]
    async fn records_request_details() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("OK", 0))
            .build();

        let mut request = make_review_request();
        request.model = "custom-model".to_string();
        request.batch_number = 3;
        request.total_batches = 5;

        mock.review(&request).await.unwrap();

        let recorded = mock.call(0);
        assert_eq!(recorded.request.model, "custom-model");
        assert_eq!(recorded.request.batch_number, 3);
        assert_eq!(recorded.request.total_batches, 5);
    }

    #[tokio::test]
    async fn assert_call_count_passes_on_match() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("OK", 0))
            .build();

        mock.review(&make_review_request()).await.unwrap();
        mock.review(&make_review_request()).await.unwrap();

        mock.assert_call_count(2);
    }

    #[tokio::test]
    #[should_panic(expected = "Expected 5 calls")]
    async fn assert_call_count_panics_on_mismatch() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("OK", 0))
            .build();

        mock.review(&make_review_request()).await.unwrap();

        mock.assert_call_count(5);
    }

    #[tokio::test]
    async fn delayed_success_adds_latency() {
        let delay = Duration::from_millis(100);
        let mock = MockBackend::builder()
            .with_delayed_success(make_review_response("Delayed", 0), delay)
            .build();

        let start = tokio::time::Instant::now();
        let result = mock.review(&make_review_request()).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(result.review.summary, "Delayed");
        assert!(
            elapsed >= delay,
            "Expected at least {:?} delay, got {:?}",
            delay,
            elapsed
        );
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<MockBackend>();
        assert_sync::<MockBackend>();
    }

    #[tokio::test]
    async fn works_with_arc_dyn_backend() {
        let mock = MockBackend::builder()
            .with_success(make_review_response("Dynamic", 1))
            .build();

        let backend: Arc<dyn ReviewBackend> = Arc::new(mock);

        let result = backend.review(&make_review_request()).await.unwrap();
        assert_eq!(result.review.summary, "Dynamic");
        assert_eq!(result.review.findings.len(), 1);
    }
}
