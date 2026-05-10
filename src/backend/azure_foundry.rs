// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Azure AI Foundry HTTP backend for Claude API.
//!
//! Implements [`ReviewBackend`] using HTTP POST
//! to an Azure AI Foundry endpoint. Uses `x-api-key` auth header and
//! deployment names as model identifiers.
//!
//! # Examples
//!
//! ```ignore
//! use panoptico::backend::azure_foundry::AzureFoundryBackend;
//! use panoptico::backend::ReviewBackend;
//!
//! let backend = AzureFoundryBackend::new(
//!     "https://my-endpoint.services.ai.azure.com",
//!     "my-api-key",
//! );
//! let response = backend.review(&request).await?;
//! ```

use crate::backend::{
    build_messages_body, build_user_content, handle_api_response, ReviewBackend, ReviewRequest,
    ReviewResponse,
};
use crate::error::ReviewError;

/// Azure AI Foundry backend client.
///
/// Sends review requests to an Azure AI Foundry deployment using
/// the Anthropic Messages API format with `x-api-key` authentication.
pub struct AzureFoundryBackend {
    /// HTTP client for making API requests.
    client: reqwest::Client,
    /// Azure AI Foundry endpoint URL.
    endpoint: String,
    /// API key for authentication.
    api_key: String,
}

impl AzureFoundryBackend {
    /// Create a new Azure AI Foundry backend.
    ///
    /// # Arguments
    ///
    /// * `endpoint` - Base URL of the Azure AI Foundry endpoint.
    /// * `api_key` - API key for `x-api-key` header authentication.
    ///
    /// # Returns
    ///
    /// A configured backend ready to send review requests.
    pub fn new(endpoint: &str, api_key: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ReviewBackend for AzureFoundryBackend {
    /// Send a review request to Azure AI Foundry and parse the response.
    ///
    /// Posts to `{endpoint}/v1/messages` with `x-api-key` header auth.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] on HTTP or authentication errors.
    /// Returns [`ReviewError::Parse`] if the response body is malformed.
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError> {
        let url = format!("{}/v1/messages", self.endpoint);
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
    /// Fixture: server error response.
    const API_ERROR_SERVER: &str = include_str!("../../tests/fixtures/api_error_server.json");

    #[test]
    fn new_creates_backend() {
        let _backend =
            AzureFoundryBackend::new("https://my-endpoint.services.ai.azure.com", "test-key");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
        let _ = backend.review(&make_review_request()).await;

        // wiremock verifies the expectation on drop
    }

    #[tokio::test]
    async fn sends_api_key_header() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(header("x-api-key", "secret-key-123"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(API_SUCCESS, "application/json"))
            .expect(1)
            .mount(&server)
            .await;

        let backend = AzureFoundryBackend::new(&server.uri(), "secret-key-123");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
        let mut request = make_review_request();
        request.model = "claude-sonnet-4-5".to_string();
        request.max_tokens = 4096;

        let _ = backend.review(&request).await;

        let received = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();

        assert_eq!(body["model"], "claude-sonnet-4-5");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
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

        let backend = AzureFoundryBackend::new(&server.uri(), "bad-key");
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 401"
        );
    }

    #[tokio::test]
    async fn returns_api_error_on_server_error() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500).set_body_raw(API_ERROR_SERVER, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = AzureFoundryBackend::new(&server.uri(), "test-key");
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 500"
        );
    }
}
