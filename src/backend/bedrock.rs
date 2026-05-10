// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! AWS Bedrock backend for Claude.
//!
//! Implements [`ReviewBackend`] using HTTP POST
//! to AWS Bedrock runtime endpoint. Uses AWS Signature V4 authentication
//! and Bedrock-specific model identifiers.
//!
//! # Examples
//!
//! ```ignore
//! use panoptico::backend::bedrock::BedrockBackend;
//! use panoptico::backend::ReviewBackend;
//!
//! let backend = BedrockBackend::new("us-east-1");
//! let response = backend.review(&request).await?;
//! ```

use serde::Serialize;

use crate::backend::{
    build_user_content, handle_api_response, review_tool, Message, ReviewBackend, ReviewRequest,
    ReviewResponse, ToolChoice,
};
use crate::error::ReviewError;

/// Bedrock-specific Anthropic API version string.
const BEDROCK_API_VERSION: &str = "bedrock-2023-05-31";

/// AWS Bedrock backend client.
///
/// Sends review requests to AWS Bedrock runtime using the Anthropic
/// Messages API format with AWS Signature V4 authentication.
pub struct BedrockBackend {
    /// HTTP client for making API requests.
    client: reqwest::Client,
    /// Base URL, defaults to `https://bedrock-runtime.{region}.amazonaws.com`.
    base_url: String,
}

impl BedrockBackend {
    /// Create a new Bedrock backend for the given AWS region.
    ///
    /// # Arguments
    ///
    /// * `region` - AWS region (e.g., `us-east-1`).
    ///
    /// # Returns
    ///
    /// A configured backend pointing to the Bedrock runtime endpoint.
    pub fn new(region: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: format!("https://bedrock-runtime.{}.amazonaws.com", region),
        }
    }

    /// Create a new Bedrock backend with a custom base URL.
    ///
    /// Used in tests to point at a wiremock server instead of
    /// the real AWS Bedrock endpoint.
    ///
    /// # Arguments
    ///
    /// * `base_url` - Custom base URL (e.g., wiremock server URI).
    ///
    /// # Returns
    ///
    /// A configured backend pointing to the given base URL.
    #[cfg(test)]
    fn with_base_url(base_url: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }
}

/// Request body for the Bedrock invoke endpoint.
///
/// Uses the Anthropic Messages API format with a Bedrock-specific
/// `anthropic_version` field instead of the `model` HTTP header.
#[derive(Serialize)]
struct BedrockRequestBody<'a> {
    anthropic_version: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: Vec<Message<'a>>,
    tools: Vec<serde_json::Value>,
    tool_choice: ToolChoice<'a>,
}

#[async_trait::async_trait]
impl ReviewBackend for BedrockBackend {
    /// Send a review request to AWS Bedrock and parse the response.
    ///
    /// Posts to `{base_url}/model/{model}/invoke` with AWS Sig V4 auth.
    /// The request body includes `anthropic_version: "bedrock-2023-05-31"`.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] on HTTP or authentication errors.
    /// Returns [`ReviewError::Parse`] if the response body is malformed.
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError> {
        let url = format!("{}/model/{}/invoke", self.base_url, request.model);
        let user_content = build_user_content(request);

        let body = BedrockRequestBody {
            anthropic_version: BEDROCK_API_VERSION,
            max_tokens: request.max_tokens,
            system: &request.system_prompt,
            messages: vec![Message {
                role: "user",
                content: &user_content,
            }],
            tools: vec![review_tool()],
            tool_choice: ToolChoice {
                choice_type: "tool",
                name: "record_code_review",
            },
        };

        let response = self.client.post(&url).json(&body).send().await?;

        handle_api_response(response).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::make_review_request;
    use wiremock::matchers::{header, method};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Fixture: successful Bedrock response with 2 findings (no cache fields).
    const BEDROCK_SUCCESS: &str = include_str!("../../tests/fixtures/bedrock_success.json");
    /// Fixture: server error response.
    const API_ERROR_SERVER: &str = include_str!("../../tests/fixtures/api_error_server.json");

    #[test]
    fn new_creates_backend() {
        let _backend = BedrockBackend::new("us-east-1");
    }

    #[tokio::test]
    async fn sends_post_to_model_invoke_endpoint() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(BEDROCK_SUCCESS, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let mut request = make_review_request();
        request.model = "anthropic.claude-sonnet-4-5-v2".to_string();

        let _ = backend.review(&request).await;

        let received = &server.received_requests().await.unwrap()[0];
        let request_path = &received.url.path();
        assert!(
            request_path.contains("/model/") && request_path.contains("/invoke"),
            "Expected path to contain '/model/.../invoke', got: {}",
            request_path
        );
    }

    #[tokio::test]
    async fn sends_content_type_json() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(header("content-type", "application/json"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(BEDROCK_SUCCESS, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let _ = backend.review(&make_review_request()).await;
    }

    #[tokio::test]
    async fn request_body_includes_anthropic_version() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(BEDROCK_SUCCESS, "application/json"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let _ = backend.review(&make_review_request()).await;

        let received = &server.received_requests().await.unwrap()[0];
        let body: serde_json::Value = serde_json::from_slice(&received.body).unwrap();

        assert_eq!(
            body["anthropic_version"], BEDROCK_API_VERSION,
            "Bedrock requests must include anthropic_version"
        );
    }

    #[tokio::test]
    async fn parses_tool_use_success_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(BEDROCK_SUCCESS, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let response = backend.review(&make_review_request()).await.unwrap();

        assert_eq!(
            response.review.summary,
            "Potential null dereference in parser module"
        );
        assert_eq!(response.review.findings.len(), 2);
        assert_eq!(response.review.findings[0].file, "src/parser.c");
        assert_eq!(response.review.findings[0].line, 73);
    }

    #[tokio::test]
    async fn defaults_cache_tokens_to_zero() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(BEDROCK_SUCCESS, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let response = backend.review(&make_review_request()).await.unwrap();

        assert_eq!(
            response.usage.cache_read_input_tokens, 0,
            "Bedrock does not support cache; should default to 0"
        );
        assert_eq!(
            response.usage.cache_creation_input_tokens, 0,
            "Bedrock does not support cache; should default to 0"
        );
        assert_eq!(response.usage.input_tokens, 2100);
        assert_eq!(response.usage.output_tokens, 312);
    }

    #[tokio::test]
    async fn returns_api_error_on_auth_failure() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(403)
                    .set_body_raw(r#"{"message":"Access denied"}"#, "application/json"),
            )
            .mount(&server)
            .await;

        let backend = BedrockBackend::with_base_url(&server.uri());
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 403"
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

        let backend = BedrockBackend::with_base_url(&server.uri());
        let result = backend.review(&make_review_request()).await;

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "Expected ReviewError::Api on 500"
        );
    }
}
