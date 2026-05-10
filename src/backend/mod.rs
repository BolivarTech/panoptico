// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Backend trait and shared types for AI code review.
//!
//! Defines the [`ReviewBackend`] trait implemented by concrete backends
//! ([`azure_foundry`], [`anthropic`], [`bedrock`], [`claude_code`]),
//! along with shared request/response types used across the review pipeline.

use serde::{Deserialize, Serialize};

use crate::error::ReviewError;
use crate::finding_id::{Category, CATEGORY_SLUGS};

/// Deserialize a value treating JSON `null` as `T::default()`.
///
/// Use with `#[serde(default)]` to handle both missing keys and
/// explicit `null` values from LLM or API responses.
pub(crate) fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// Deserialize `Vec<Finding>` from a JSON array, `null`, or a string-wrapped JSON array.
///
/// LLMs occasionally return `findings` as a JSON **string** (`"[{...}]"`)
/// instead of a proper array (`[{...}]`). This deserializer handles all three
/// representations transparently:
/// - `[{...}]` → normal array deserialization
/// - `null` → empty `Vec`
/// - `"[{...}]"` → re-parse the string as `Vec<Finding>`
fn string_or_vec_findings<'de, D>(deserializer: D) -> Result<Vec<Finding>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de;

    struct FindingsVisitor;

    impl<'de> de::Visitor<'de> for FindingsVisitor {
        type Value = Vec<Finding>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter
                .write_str("a JSON array of findings, null, or a string containing a JSON array")
        }

        fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_none<E: de::Error>(self) -> Result<Self::Value, E> {
            Ok(Vec::new())
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
            serde_json::from_str(value).map_err(de::Error::custom)
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
            Deserialize::deserialize(de::value::SeqAccessDeserializer::new(seq))
        }
    }

    deserializer.deserialize_any(FindingsVisitor)
}

/// Severity level of a code review finding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Security vulnerability, crash, data loss.
    Critical,
    /// Potential bug, race condition, resource leak.
    #[default]
    Warning,
    /// Code style, naming, readability improvement.
    Suggestion,
    /// Good pattern worth highlighting.
    Positive,
}

impl std::fmt::Display for Severity {
    /// Render a human-readable severity tag.
    ///
    /// # Examples
    ///
    /// ```
    /// use panoptico::backend::Severity;
    ///
    /// assert_eq!(format!("{}", Severity::Critical), "CRITICAL");
    /// assert_eq!(format!("{}", Severity::Warning), "WARNING");
    /// assert_eq!(format!("{}", Severity::Suggestion), "SUGGESTION");
    /// assert_eq!(format!("{}", Severity::Positive), "POSITIVE");
    /// ```
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Critical => write!(f, "CRITICAL"),
            Severity::Warning => write!(f, "WARNING"),
            Severity::Suggestion => write!(f, "SUGGESTION"),
            Severity::Positive => write!(f, "POSITIVE"),
        }
    }
}

/// A single code review finding tied to a file and line.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Finding {
    /// Severity level.
    #[serde(default, deserialize_with = "null_as_default")]
    pub severity: Severity,
    /// Source file path.
    #[serde(default, deserialize_with = "null_as_default")]
    pub file: String,
    /// Line number in the file.
    #[serde(default, deserialize_with = "null_as_default")]
    pub line: u32,
    /// Short title summarizing the finding.
    #[serde(default, deserialize_with = "null_as_default")]
    pub title: String,
    /// Detailed description of the issue.
    #[serde(default, deserialize_with = "null_as_default")]
    pub description: String,
    /// Suggested fix or improvement (empty for positive findings).
    #[serde(default, deserialize_with = "null_as_default")]
    pub suggestion: String,
    /// Controlled-vocabulary category for deterministic ID generation.
    #[serde(default, deserialize_with = "null_as_default")]
    pub category: Category,
    /// Deterministic finding ID (`SHA256(file:line:category)[:16]`).
    #[serde(default, rename = "findingId")]
    pub finding_id: String,
    /// Step-by-step reasoning justifying the severity assignment.
    ///
    /// Populated by the chain-of-thought instructions in the review
    /// system prompt. Excluded from human-readable output (see
    /// `format_human_readable` in `reviewer.rs`) but included in
    /// JSON output for debugging and audit.
    #[serde(default, deserialize_with = "null_as_default")]
    pub reasoning: String,
}

/// Aggregated code review result for one or more batches.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeReview {
    /// Human-readable summary of the review.
    #[serde(default, deserialize_with = "null_as_default")]
    pub summary: String,
    /// Individual findings.
    #[serde(default, deserialize_with = "string_or_vec_findings")]
    pub findings: Vec<Finding>,
}

/// Token usage statistics from a single API call.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input tokens consumed.
    pub input_tokens: u32,
    /// Output tokens generated.
    pub output_tokens: u32,
    /// Tokens read from prompt cache.
    pub cache_read_input_tokens: u32,
    /// Tokens written to prompt cache.
    pub cache_creation_input_tokens: u32,
}

/// Response from a review backend call.
#[derive(Debug, Clone)]
pub struct ReviewResponse {
    /// The code review result.
    pub review: CodeReview,
    /// Token usage for cost tracking.
    pub usage: TokenUsage,
}

/// Request payload sent to a review backend.
#[derive(Debug, Clone)]
pub struct ReviewRequest {
    /// System-level prompt defining the reviewer role.
    pub system_prompt: String,
    /// Optional project-specific review instructions.
    pub custom_instructions: Option<String>,
    /// The diff content to review.
    pub diff_content: String,
    /// Current batch number (1-indexed).
    pub batch_number: u32,
    /// Total number of batches.
    pub total_batches: u32,
    /// Comma-separated list of files in this batch.
    pub file_info: String,
    /// Model deployment name.
    pub model: String,
    /// Maximum output tokens.
    pub max_tokens: u32,
}

/// Trait for AI review backends.
///
/// Implementors handle sending diff content to an LLM and parsing
/// the structured review response. Concrete implementations:
/// [`azure_foundry`], [`anthropic`], [`bedrock`], [`claude_code`].
#[async_trait::async_trait]
pub trait ReviewBackend: Send + Sync {
    /// Send a review request and receive structured findings.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] on communication failure, or
    /// [`ReviewError::Parse`] if the response cannot be deserialized.
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError>;
}

// ── Shared Anthropic Messages API utilities ──────────────────────

/// Anthropic Messages API version string.
pub(crate) const ANTHROPIC_API_VERSION: &str = "2023-06-01";

/// HTTP header name for the Anthropic API version.
pub(crate) const ANTHROPIC_VERSION_HEADER: &str = "anthropic-version";

/// Build the user content string from a review request.
///
/// For review requests (non-empty `file_info`), formats batch info,
/// file list, diff content, and optional custom instructions.
/// For synthesis requests (empty `file_info`), passes through the
/// pre-built content directly.
pub(crate) fn build_user_content(request: &ReviewRequest) -> String {
    if request.file_info.is_empty() {
        return request.diff_content.clone();
    }
    let mut content = format!(
        "Review batch {}/{}\n\nFiles: {}\n\n{}",
        request.batch_number, request.total_batches, request.file_info, request.diff_content
    );
    if let Some(ref instructions) = request.custom_instructions {
        content.push_str(&format!("\n\nAdditional instructions:\n{}", instructions));
    }
    content
}

/// Request body for the Anthropic Messages API.
///
/// Used by Azure AI Foundry and Anthropic backends. Bedrock uses
/// a specialized variant with `anthropic_version` instead of `model`.
#[derive(Serialize)]
pub(crate) struct MessagesRequestBody<'a> {
    pub model: &'a str,
    pub max_tokens: u32,
    pub system: &'a str,
    pub messages: Vec<Message<'a>>,
    pub tools: Vec<serde_json::Value>,
    pub tool_choice: ToolChoice<'a>,
}

/// A single message in the conversation.
#[derive(Serialize)]
pub(crate) struct Message<'a> {
    pub role: &'a str,
    pub content: &'a str,
}

/// Tool choice constraint for enforced structured output.
#[derive(Serialize)]
pub(crate) struct ToolChoice<'a> {
    #[serde(rename = "type")]
    pub choice_type: &'a str,
    pub name: &'a str,
}

/// Build a [`MessagesRequestBody`] from a review request and pre-built user content.
///
/// Shared by [`anthropic`] and [`azure_foundry`] backends. Bedrock uses a
/// specialized body with `anthropic_version` instead of `model`.
pub(crate) fn build_messages_body<'a>(
    request: &'a ReviewRequest,
    user_content: &'a str,
) -> MessagesRequestBody<'a> {
    MessagesRequestBody {
        model: &request.model,
        max_tokens: request.max_tokens,
        system: &request.system_prompt,
        messages: vec![Message {
            role: "user",
            content: user_content,
        }],
        tools: vec![review_tool()],
        tool_choice: ToolChoice {
            choice_type: "tool",
            name: "record_code_review",
        },
    }
}

/// Build the `record_code_review` tool definition with JSON schema.
pub(crate) fn review_tool() -> serde_json::Value {
    let category_enum: Vec<serde_json::Value> = CATEGORY_SLUGS
        .iter()
        .map(|s| serde_json::json!(s))
        .collect();

    serde_json::json!({
        "name": "record_code_review",
        "description": "Record structured code review findings",
        "input_schema": {
            "type": "object",
            "required": ["summary", "findings"],
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Human-readable summary of the review"
                },
                "findings": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": ["severity", "file", "line", "title", "description", "suggestion", "category", "reasoning"],
                        "properties": {
                            "severity": {
                                "type": "string",
                                "enum": ["critical", "warning", "suggestion", "positive"]
                            },
                            "file": { "type": "string" },
                            "line": { "type": "integer" },
                            "title": { "type": "string" },
                            "description": { "type": "string" },
                            "suggestion": { "type": "string" },
                            "category": {
                                "type": "string",
                                "enum": category_enum
                            },
                            "reasoning": {
                                "type": "string",
                                "description": "Step-by-step reasoning: (1) what the code does, (2) what could go wrong, (3) how likely is the issue, (4) why this severity. If the issue is unlikely, downgrade severity or omit."
                            }
                        }
                    }
                }
            }
        }
    })
}

/// Anthropic Messages API response envelope.
#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    usage: ApiUsage,
}

/// A content block in the response (text, tool_use, or unknown).
#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "tool_use")]
    ToolUse { input: CodeReview },
    #[serde(rename = "text")]
    Text {},
    /// Catch-all for unknown content block types from future API versions.
    #[serde(other)]
    Other,
}

/// Token usage from the API response.
#[derive(Deserialize)]
struct ApiUsage {
    #[serde(default, deserialize_with = "null_as_default")]
    input_tokens: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    output_tokens: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    cache_creation_input_tokens: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    cache_read_input_tokens: u32,
}

/// Parse an Anthropic Messages API response into a [`ReviewResponse`].
///
/// Extracts the [`CodeReview`] from the first `tool_use` content block
/// and maps usage statistics.
///
/// # Errors
///
/// Returns [`ReviewError::Parse`] if the response is malformed or
/// missing a `tool_use` content block.
pub(crate) fn parse_messages_response(body: &str) -> Result<ReviewResponse, ReviewError> {
    let api_response: ApiResponse =
        serde_json::from_str(body).map_err(|e| ReviewError::Parse(e.to_string()))?;

    let review = api_response
        .content
        .into_iter()
        .find_map(|block| match block {
            ContentBlock::ToolUse { input } => Some(input),
            _ => None,
        })
        .ok_or_else(|| ReviewError::Parse("no tool_use block in response".to_string()))?;

    let usage = TokenUsage {
        input_tokens: api_response.usage.input_tokens,
        output_tokens: api_response.usage.output_tokens,
        cache_read_input_tokens: api_response.usage.cache_read_input_tokens,
        cache_creation_input_tokens: api_response.usage.cache_creation_input_tokens,
    };

    Ok(ReviewResponse { review, usage })
}

/// Handle an HTTP response from the Anthropic Messages API.
///
/// Checks the status code and delegates to [`parse_messages_response`]
/// on success.
///
/// # Errors
///
/// Returns [`ReviewError::Api`] on non-success HTTP status.
/// Returns [`ReviewError::Parse`] if the response body is malformed.
pub(crate) async fn handle_api_response(
    response: reqwest::Response,
) -> Result<ReviewResponse, ReviewError> {
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(ReviewError::Api(format!("HTTP {}: {}", status, body)));
    }
    parse_messages_response(&body)
}

pub mod anthropic;
pub mod azure_foundry;
pub mod bedrock;
pub mod claude_code;

#[cfg(test)]
pub mod mock;

#[cfg(test)]
mod tests {
    use super::*;

    fn review_request() -> ReviewRequest {
        ReviewRequest {
            system_prompt: "You are a reviewer.".to_string(),
            custom_instructions: None,
            diff_content: "+// added line".to_string(),
            batch_number: 2,
            total_batches: 5,
            file_info: "main.rs, lib.rs".to_string(),
            model: "claude-sonnet-4-5".to_string(),
            max_tokens: 4096,
        }
    }

    #[test]
    fn severity_display_critical() {
        assert_eq!(format!("{}", Severity::Critical), "CRITICAL");
    }

    #[test]
    fn severity_display_warning() {
        assert_eq!(format!("{}", Severity::Warning), "WARNING");
    }

    #[test]
    fn severity_display_suggestion() {
        assert_eq!(format!("{}", Severity::Suggestion), "SUGGESTION");
    }

    #[test]
    fn severity_display_positive() {
        assert_eq!(format!("{}", Severity::Positive), "POSITIVE");
    }

    #[test]
    fn build_user_content_includes_batch_header_for_review() {
        let req = review_request();
        let content = build_user_content(&req);
        assert!(
            content.contains("Review batch 2/5"),
            "Should include batch header"
        );
        assert!(content.contains("Files: main.rs, lib.rs"));
        assert!(content.contains("+// added line"));
    }

    #[test]
    fn build_user_content_appends_custom_instructions() {
        let mut req = review_request();
        req.custom_instructions = Some("Check for MISRA compliance".to_string());
        let content = build_user_content(&req);
        assert!(content.contains("Additional instructions:"));
        assert!(content.contains("Check for MISRA compliance"));
    }

    #[test]
    fn build_user_content_skips_batch_header_for_synthesis() {
        let req = ReviewRequest {
            file_info: String::new(),
            diff_content: "=== BATCH 1 ===\npre-built synthesis content".to_string(),
            ..review_request()
        };
        let content = build_user_content(&req);
        assert!(
            !content.contains("Review batch"),
            "Synthesis should not have batch header"
        );
        assert_eq!(
            content, req.diff_content,
            "Synthesis should pass content through directly"
        );
    }

    #[test]
    fn finding_null_suggestion_defaults_to_empty() {
        let json = r#"{
            "severity": "warning",
            "file": "src/main.rs",
            "line": 42,
            "title": "Unused variable",
            "description": "Variable x is never read",
            "suggestion": null
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.suggestion, "");
    }

    #[test]
    fn finding_all_null_fields_default_gracefully() {
        let json = r#"{
            "severity": null,
            "file": null,
            "line": null,
            "title": null,
            "description": null,
            "suggestion": null
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.file, "");
        assert_eq!(finding.line, 0);
        assert_eq!(finding.title, "");
        assert_eq!(finding.description, "");
        assert_eq!(finding.suggestion, "");
    }

    #[test]
    fn finding_missing_fields_default_gracefully() {
        let json = r#"{}"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.file, "");
        assert_eq!(finding.line, 0);
        assert_eq!(finding.title, "");
        assert_eq!(finding.description, "");
        assert_eq!(finding.suggestion, "");
    }

    #[test]
    fn code_review_null_fields_default_gracefully() {
        let json = r#"{"summary": null, "findings": null}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.summary, "");
        assert!(review.findings.is_empty());
    }

    #[test]
    fn code_review_string_wrapped_findings_parsed() {
        let json = r#"{"summary": "Review done.", "findings": "[{\"severity\":\"warning\",\"file\":\"main.c\",\"line\":1,\"title\":\"T\",\"description\":\"D\",\"suggestion\":\"S\",\"category\":\"style\"}]"}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.findings.len(), 1);
        assert_eq!(review.findings[0].file, "main.c");
        assert_eq!(review.findings[0].category, Category::Style);
    }

    #[test]
    fn code_review_string_wrapped_empty_array_parsed() {
        let json = r#"{"summary": "No issues.", "findings": "[]"}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert!(review.findings.is_empty());
    }

    #[test]
    fn code_review_normal_array_findings_still_works() {
        let json = r#"{"summary": "OK", "findings": [{"severity":"warning","file":"a.rs","line":1,"title":"T","description":"D","suggestion":"S","category":"style"}]}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert_eq!(review.findings.len(), 1);
        assert_eq!(review.findings[0].file, "a.rs");
    }

    #[test]
    fn code_review_missing_findings_defaults_to_empty() {
        let json = r#"{"summary": "OK"}"#;
        let review: CodeReview = serde_json::from_str(json).unwrap();
        assert!(review.findings.is_empty());
    }

    #[test]
    fn severity_default_is_warning() {
        assert_eq!(Severity::default(), Severity::Warning);
    }

    // ── E. Finding serde with new fields (9 tests — all PASS) ──

    #[test]
    fn finding_serialize_includes_finding_id() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "main.c".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::default(),
            finding_id: "abc123def456ghij".to_string(),
            reasoning: String::new(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(
            json.contains("\"findingId\""),
            "Serialized JSON should contain findingId key"
        );
        assert!(
            json.contains("abc123def456ghij"),
            "Serialized JSON should contain the finding_id value"
        );
    }

    #[test]
    fn finding_serialize_includes_category() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "main.c".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::BufferOverflow,
            finding_id: String::new(),
            reasoning: String::new(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(
            json.contains("\"category\""),
            "Serialized JSON should contain category key"
        );
        assert!(
            json.contains("buffer-overflow"),
            "Serialized JSON should contain the category slug"
        );
    }

    #[test]
    fn finding_deserialize_without_finding_id_uses_default() {
        let json = r#"{
            "severity": "warning",
            "file": "main.c",
            "line": 1,
            "title": "T",
            "description": "D",
            "suggestion": "S",
            "category": "style"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.finding_id, "",
            "Missing findingId should default to empty string"
        );
    }

    #[test]
    fn finding_deserialize_without_category_uses_default() {
        let json = r#"{
            "severity": "warning",
            "file": "main.c",
            "line": 1,
            "title": "T",
            "description": "D",
            "suggestion": "S"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.category,
            Category::Other,
            "Missing category should default to Other"
        );
    }

    #[test]
    fn finding_deserialize_with_all_fields() {
        let json = r#"{
            "severity": "critical",
            "file": "src/main.c",
            "line": 42,
            "title": "Buffer overflow",
            "description": "Desc",
            "suggestion": "Fix",
            "category": "buffer-overflow",
            "findingId": "a1b2c3d4e5f6g7h8"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.severity, Severity::Critical);
        assert_eq!(finding.file, "src/main.c");
        assert_eq!(finding.line, 42);
        assert_eq!(finding.category, Category::BufferOverflow);
        assert_eq!(finding.finding_id, "a1b2c3d4e5f6g7h8");
    }

    #[test]
    fn finding_deserialize_camel_case_finding_id() {
        let json = r#"{
            "severity": "warning",
            "file": "main.c",
            "line": 1,
            "title": "T",
            "description": "D",
            "suggestion": "S",
            "findingId": "deadbeef12345678"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.finding_id, "deadbeef12345678",
            "Should deserialize camelCase findingId"
        );
    }

    #[test]
    fn finding_json_roundtrip_preserves_finding_id() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "main.c".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::default(),
            finding_id: "a1b2c3d4e5f67890".to_string(),
            reasoning: String::new(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let roundtrip: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.finding_id, "a1b2c3d4e5f67890");
    }

    #[test]
    fn finding_json_roundtrip_preserves_category() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "main.c".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::RaceCondition,
            finding_id: String::new(),
            reasoning: String::new(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let roundtrip: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.category, Category::RaceCondition);
    }

    #[test]
    fn finding_json_roundtrip_preserves_reasoning() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "main.c".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::default(),
            finding_id: String::new(),
            reasoning: "1. Does X. 2. Could fail. 3. Unlikely. 4. Suggestion.".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        let roundtrip: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(
            roundtrip.reasoning,
            "1. Does X. 2. Could fail. 3. Unlikely. 4. Suggestion."
        );
    }

    #[test]
    fn finding_backward_compat_old_json_without_new_fields() {
        let json = r#"{
            "severity": "warning",
            "file": "src/main.c",
            "line": 42,
            "title": "Unused variable",
            "description": "Variable x is never read",
            "suggestion": "Remove the variable"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.file, "src/main.c");
        assert_eq!(finding.line, 42);
        assert_eq!(
            finding.category,
            Category::Other,
            "Old JSON without category should default to Other"
        );
        assert_eq!(
            finding.finding_id, "",
            "Old JSON without findingId should default to empty"
        );
    }

    #[test]
    fn finding_deserialize_null_category_defaults_to_other() {
        let json = r#"{
            "severity": "warning",
            "file": "main.c",
            "line": 1,
            "title": "T",
            "description": "D",
            "suggestion": "S",
            "category": null
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.category,
            Category::Other,
            "Null category should default to Other"
        );
    }

    // ── F. Tool schema (4 tests — all PASS) ─────────────────────

    #[test]
    fn review_tool_schema_includes_category_property() {
        let tool = review_tool();
        let properties = &tool["input_schema"]["properties"]["findings"]["items"]["properties"];
        assert!(
            properties.get("category").is_some(),
            "Tool schema should include category property"
        );
    }

    #[test]
    fn review_tool_schema_category_has_enum_constraint() {
        let tool = review_tool();
        let category =
            &tool["input_schema"]["properties"]["findings"]["items"]["properties"]["category"];
        assert!(
            category.get("enum").is_some(),
            "Category property should have an enum constraint"
        );
    }

    #[test]
    fn review_tool_schema_category_enum_contains_all_slugs() {
        let tool = review_tool();
        let category_enum = &tool["input_schema"]["properties"]["findings"]["items"]["properties"]
            ["category"]["enum"];
        let expected = [
            "buffer-overflow",
            "null-deref",
            "resource-leak",
            "unvalidated-input",
            "race-condition",
            "error-handling",
            "hardcoded-secret",
            "integer-overflow",
            "injection",
            "logic-error",
            "type-mismatch",
            "deprecated-api",
            "performance",
            "style",
            "documentation",
            "other",
        ];
        let slugs: Vec<&str> = category_enum
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for slug in &expected {
            assert!(
                slugs.contains(slug),
                "Category enum should contain slug '{}'",
                slug
            );
        }
        assert_eq!(
            slugs.len(),
            expected.len(),
            "Category enum should have exactly {} slugs",
            expected.len()
        );
    }

    #[test]
    fn review_tool_schema_category_is_required() {
        let tool = review_tool();
        let required = &tool["input_schema"]["properties"]["findings"]["items"]["required"];
        let required_fields: Vec<&str> = required
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(
            required_fields.contains(&"category"),
            "category should be in the required array"
        );
    }

    #[test]
    fn api_usage_null_tokens_default_to_zero() {
        let json = r#"{
            "input_tokens": null,
            "output_tokens": null,
            "cache_creation_input_tokens": null,
            "cache_read_input_tokens": null
        }"#;
        let usage: ApiUsage = serde_json::from_str(json).unwrap();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    // --- Phase 1 (G2): Chain-of-thought reasoning tests ---

    #[test]
    fn finding_deserialize_with_reasoning() {
        let json = r#"{
            "severity": "warning",
            "file": "src/main.rs",
            "line": 10,
            "title": "Potential issue",
            "description": "Desc",
            "suggestion": "Fix it",
            "category": "logic-error",
            "reasoning": "1. Code does X. 2. Could fail if Y. 3. Unlikely. 4. Suggestion level."
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.reasoning,
            "1. Code does X. 2. Could fail if Y. 3. Unlikely. 4. Suggestion level."
        );
    }

    #[test]
    fn finding_deserialize_without_reasoning() {
        let json = r#"{
            "severity": "warning",
            "file": "src/main.rs",
            "line": 10,
            "title": "Title",
            "description": "Desc",
            "suggestion": "Fix",
            "category": "logic-error"
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.reasoning, "",
            "Missing reasoning should default to empty string"
        );
    }

    #[test]
    fn finding_deserialize_null_reasoning() {
        let json = r#"{
            "severity": "warning",
            "file": "src/main.rs",
            "line": 10,
            "title": "Title",
            "description": "Desc",
            "suggestion": "Fix",
            "category": "logic-error",
            "reasoning": null
        }"#;
        let finding: Finding = serde_json::from_str(json).unwrap();
        assert_eq!(
            finding.reasoning, "",
            "Null reasoning should default to empty string"
        );
    }

    #[test]
    fn finding_serialize_includes_reasoning() {
        let finding = Finding {
            severity: Severity::Warning,
            file: "test.rs".to_string(),
            line: 1,
            title: "T".to_string(),
            description: "D".to_string(),
            suggestion: "S".to_string(),
            category: Category::Other,
            finding_id: String::new(),
            reasoning: "Step-by-step analysis here.".to_string(),
        };
        let json = serde_json::to_string(&finding).unwrap();
        assert!(
            json.contains("\"reasoning\""),
            "Serialized JSON must contain reasoning field"
        );
        assert!(
            json.contains("Step-by-step analysis here."),
            "Serialized JSON must contain reasoning value"
        );
    }

    #[test]
    fn finding_default_has_empty_reasoning() {
        let finding = Finding::default();
        assert_eq!(
            finding.reasoning, "",
            "Default reasoning must be empty string"
        );
    }

    #[test]
    fn tool_schema_includes_reasoning() {
        let schema = review_tool();
        let properties = &schema["input_schema"]["properties"]["findings"]["items"]["properties"];
        assert!(
            properties.get("reasoning").is_some(),
            "Tool schema must include 'reasoning' in finding properties"
        );
    }

    #[test]
    fn tool_schema_reasoning_is_required() {
        let schema = review_tool();
        let required = schema["input_schema"]["properties"]["findings"]["items"]["required"]
            .as_array()
            .expect("required should be an array");
        let has_reasoning = required.iter().any(|v| v.as_str() == Some("reasoning"));
        assert!(
            has_reasoning,
            "Tool schema must list 'reasoning' as required"
        );
    }
}
