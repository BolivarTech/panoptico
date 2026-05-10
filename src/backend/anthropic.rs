// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Direct Anthropic API backend for Claude.
//!
//! Implements [`ReviewBackend`] using HTTP POST
//! to `api.anthropic.com`. Uses `x-api-key` auth header and versioned
//! model names (e.g., `claude-sonnet-4-5-20250929`).
//!
//! # Examples
//!
//! ```ignore
//! use panoptico::backend::anthropic::AnthropicBackend;
//! use panoptico::backend::ReviewBackend;
//!
//! let backend = AnthropicBackend::new("my-api-key");
//! let response = backend.review(&request).await?;
//! ```

use crate::backend::{
    build_messages_body, build_user_content, handle_api_response, ReviewBackend, ReviewRequest,
    ReviewResponse,
};
use crate::error::ReviewError;

/// Direct Anthropic API backend client.
///
/// Sends review requests to `api.anthropic.com` using the Messages API
/// with `x-api-key` header authentication.
pub struct AnthropicBackend {
    /// HTTP client for making API requests.
    client: reqwest::Client,
    /// API key for `x-api-key` header authentication.
    api_key: String,
    /// Base URL, defaults to `https://api.anthropic.com`.
    base_url: String,
}

impl AnthropicBackend {
    /// Create a new Anthropic backend with the default base URL.
    ///
    /// # Arguments
    ///
    /// * `api_key` - API key for `x-api-key` header authentication.
    ///
    /// # Returns
    ///
    /// A configured backend pointing to `https://api.anthropic.com`.
    pub fn new(api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    /// Create a new Anthropic backend with a custom base URL.
    ///
    /// Used in tests to point at a wiremock server instead of
    /// the real Anthropic API.
    ///
    /// # Arguments
    ///
    /// * `api_key` - API key for `x-api-key` header authentication.
    /// * `base_url` - Custom base URL (e.g., wiremock server URI).
    ///
    /// # Returns
    ///
    /// A configured backend pointing to the given base URL.
    #[cfg(test)]
    fn with_base_url(api_key: &str, base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key: api_key.to_string(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ReviewBackend for AnthropicBackend {
    /// Send a review request to the Anthropic API and parse the response.
    ///
    /// Posts to `{base_url}/v1/messages` with `x-api-key` header auth.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] on HTTP or authentication errors.
    /// Returns [`ReviewError::Parse`] if the response body is malformed.
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError> {
        let url = format!("{}/v1/messages", self.base_url);
        let user_content = build_user_content(request);
        let body = build_messages_body(request, &user_content);

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header(
                crate::backend::ANTHROPIC_VERSION_HEADER,
                crate::backend::ANTHROPIC_API_VERSION,
            )
            .json(&body)
            .send()
            .await?;

        handle_api_response(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::make_review_request;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Fixture: successful API response with 2 findings.
    const API_SUCCESS: &str = include_str!("../../tests/fixtures/api_success.json");
    /// Fixture: successful API response with cache tokens populated.
    const API_SUCCESS_CACHED: &str = include_str!("../../tests/fixtures/api_success_cached.json");
    /// Fixture: authentication error response.
    const API_ERROR_AUTH: &str = include_str!("../../tests/fixtures/api_error_auth.json");
    /// Fixture: rate limit error response.
    const API_ERROR_RATE_LIMIT: &str =
        include_str!("../../tests/fixtures/api_error_rate_limit.json");

    #[test]
    fn new_creates_backend() {
        let _backend = AnthropicBackend::new("test-key");
    }

    #[tokio::test]
    async fn sends_post_to_v1_messages() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let _ = backend.review(&make_review_request()).await;
    }

    #[tokio::test]
    async fn sends_x_api_key_header() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(header("x-api-key", "secret-key-456"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("secret-key-456", &server.uri());
        let _ = backend.review(&make_review_request()).await;
    }

    #[tokio::test]
    async fn sends_anthropic_version_header() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(header(
                crate::backend::ANTHROPIC_VERSION_HEADER,
                crate::backend::ANTHROPIC_API_VERSION,
            ))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let _ = backend.review(&make_review_request()).await;
    }

    #[tokio::test]
    async fn sends_content_type_json() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let _ = backend.review(&make_review_request()).await;
    }

    #[tokio::test]
    async fn request_body_includes_model_and_max_tokens() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let mut request = make_review_request();
        request.model = "claude-sonnet-4-5-20250929".to_string();
        request.max_tokens = 4096;

        let _ = backend.review(&request).await;

        let received = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();

        assert_eq!(body["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(body["max_tokens"], 4096);
        assert!(body["system"].is_string(), "body must include 'system'");
        assert!(body["messages"].is_array(), "body must include 'messages'");
    }

    #[tokio::test]
    async fn parses_tool_use_success_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let response = backend.review(&make_review_request()).await.unwrap();

        assert_eq!(
            response.review.summary,
            "Buffer overflow vulnerability found in main.c"
        );
        assert_eq!(response.review.findings.len(), 2);
        assert_eq!(response.review.findings[0].file, "src/main.c");
        assert_eq!(response.review.findings[0].line, 42);
    }

    #[tokio::test]
    async fn parses_cached_response_token_usage() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(API_SUCCESS_CACHED, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let response = backend.review(&make_review_request()).await.unwrap();

        assert_eq!(response.usage.cache_read_input_tokens, 1400);
        assert_eq!(response.usage.cache_creation_input_tokens, 0);
        assert_eq!(response.usage.input_tokens, 1800);
        assert_eq!(response.usage.output_tokens, 245);
    }

    #[tokio::test]
    async fn returns_api_error_on_auth_failure() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401).set_body_raw(API_ERROR_AUTH, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("bad-key", &server.uri());
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 401"
        );
    }

    #[tokio::test]
    async fn returns_api_error_on_rate_limit() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429).set_body_raw(API_ERROR_RATE_LIMIT, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = AnthropicBackend::with_base_url("test-key", &server.uri());
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 429"
        );
    }
}
