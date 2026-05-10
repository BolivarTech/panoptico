// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Claude Code CLI subprocess backend.
//!
//! Implements [`ReviewBackend`] by invoking the
//! `claude` CLI as a subprocess with `--print` and `--output-format json`.
//! Parses the JSON output and estimates token usage from content length.
//!
//! # Examples
//!
//! ```ignore
//! use panoptico::backend::claude_code::ClaudeCodeBackend;
//! use panoptico::backend::ReviewBackend;
//!
//! let backend = ClaudeCodeBackend::new();
//! let response = backend.review(&request).await?;
//! ```

use serde::Deserialize;

use crate::backend::{CodeReview, ReviewBackend, ReviewRequest, ReviewResponse, TokenUsage};
use crate::error::ReviewError;
use crate::finding_id::CATEGORY_SLUGS;

/// Approximate characters per token used for CLI token estimation.
const CHARS_PER_TOKEN_ESTIMATE: usize = 4;

/// Claude Code CLI subprocess backend.
///
/// Invokes the `claude` CLI with `--print` mode to perform code reviews.
/// Parses structured JSON output and estimates token usage since the CLI
/// does not provide exact token counts.
#[derive(Default)]
pub struct ClaudeCodeBackend;

impl ClaudeCodeBackend {
    /// Create a new Claude Code backend.
    ///
    /// # Returns
    ///
    /// A configured backend ready to invoke the CLI.
    pub fn new() -> Self {
        Self
    }

    /// Parse Claude Code CLI JSON output into a review response.
    ///
    /// Handles the nested `result` field which contains an escaped JSON
    /// string with the actual review data.
    ///
    /// # Arguments
    ///
    /// * `output` - Raw JSON output from the CLI subprocess.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] if the CLI reported an error.
    /// Returns [`ReviewError::Parse`] if the JSON is malformed.
    fn parse_output(output: &str) -> Result<ReviewResponse, ReviewError> {
        let cli_output: CliOutput =
            serde_json::from_str(output).map_err(|e| ReviewError::Parse(e.to_string()))?;

        if cli_output.is_error {
            return Err(ReviewError::Api(cli_output.result));
        }

        let json_str = extract_json(&cli_output.result);
        let review: CodeReview =
            serde_json::from_str(json_str).map_err(|e| ReviewError::Parse(e.to_string()))?;

        // Use real token counts from CLI when available, estimate otherwise.
        let usage = cli_output
            .usage
            .map(|u| TokenUsage {
                input_tokens: u.input_tokens.unwrap_or(0),
                output_tokens: u.output_tokens.unwrap_or(0),
                cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0),
                cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0),
            })
            .unwrap_or_else(|| TokenUsage {
                input_tokens: (output.len() / CHARS_PER_TOKEN_ESTIMATE).min(u32::MAX as usize)
                    as u32,
                output_tokens: (cli_output.result.len() / CHARS_PER_TOKEN_ESTIMATE)
                    .min(u32::MAX as usize) as u32,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            });

        Ok(ReviewResponse { review, usage })
    }

    /// Build command-line flags for the `claude` CLI invocation.
    ///
    /// The prompt is **not** included — it is piped via stdin to avoid
    /// hitting the OS command-line length limit on Windows (~32 KB).
    ///
    /// # Arguments
    ///
    /// * `request` - The review request containing model and system prompt.
    ///
    /// # Returns
    ///
    /// A vector of CLI flag strings (no positional prompt argument).
    fn build_command_args(request: &ReviewRequest) -> Vec<String> {
        vec![
            "--print".to_string(),
            "--output-format".to_string(),
            "json".to_string(),
            "--model".to_string(),
            request.model.clone(),
            "--system-prompt".to_string(),
            request.system_prompt.clone(),
        ]
    }

    /// Build the user prompt string from a review request.
    ///
    /// Appends a JSON format instruction so the model responds with
    /// structured output matching the [`CodeReview`] schema.
    ///
    /// # Arguments
    ///
    /// * `request` - The review request containing diff content.
    ///
    /// # Returns
    ///
    /// The complete prompt string to pipe via stdin.
    fn build_prompt(request: &ReviewRequest) -> String {
        let category_list = CATEGORY_SLUGS.join("|");
        let json_format = format!(
            "\n\n\
            Respond ONLY with a JSON object in this exact format (no markdown fences):\n\
            {{\"summary\": \"<brief summary>\", \"findings\": [\
            {{\"severity\": \"critical|warning|suggestion|positive\", \
            \"file\": \"<path>\", \"line\": <number>, \
            \"title\": \"<short title>\", \
            \"description\": \"<detail>\", \
            \"suggestion\": \"<fix>\", \
            \"category\": \"{}\", \
            \"reasoning\": \"<step-by-step reasoning>\"}}]}}",
            category_list
        );

        if request.file_info.is_empty() {
            // Synthesis request — content is pre-built batch reviews.
            format!("{}{}", request.diff_content, json_format)
        } else {
            // Review request — format batch header + diff.
            let mut body = format!(
                "Review batch {}/{}\n\nFiles: {}\n\n{}",
                request.batch_number,
                request.total_batches,
                request.file_info,
                request.diff_content,
            );
            if let Some(ref custom) = request.custom_instructions {
                body.push_str(&format!("\n\nAdditional instructions:\n{}", custom));
            }
            format!("{}{}", body, json_format)
        }
    }
}

/// Extract JSON content from a string that may contain markdown fences.
///
/// Strips leading/trailing whitespace, and removes `` ```json `` / `` ``` ``
/// fences if present. Returns a slice pointing to the inner JSON.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    }
}

/// Claude Code CLI JSON output envelope.
#[derive(Deserialize)]
struct CliOutput {
    /// Whether the CLI reported an error.
    #[serde(default, deserialize_with = "super::null_as_default")]
    is_error: bool,
    /// The result string (review JSON or error message).
    #[serde(default, deserialize_with = "super::null_as_default")]
    result: String,
    /// Token usage reported by the CLI (available since v2.x).
    usage: Option<CliUsage>,
}

/// Token usage fields from the Claude Code CLI output.
#[derive(Deserialize)]
struct CliUsage {
    /// Input tokens consumed.
    input_tokens: Option<u32>,
    /// Output tokens generated.
    output_tokens: Option<u32>,
    /// Tokens read from prompt cache.
    cache_read_input_tokens: Option<u32>,
    /// Tokens written to prompt cache.
    cache_creation_input_tokens: Option<u32>,
}

#[async_trait::async_trait]
impl ReviewBackend for ClaudeCodeBackend {
    /// Execute a review by spawning the `claude` CLI subprocess.
    ///
    /// Builds CLI arguments, pipes the prompt via stdin (to avoid
    /// OS command-line length limits), captures stdout, and parses
    /// the JSON output.
    ///
    /// # Errors
    ///
    /// Returns [`ReviewError::Api`] if the CLI process fails or
    /// reports an error in its output.
    /// Returns [`ReviewError::Io`] if the subprocess cannot be spawned.
    /// Returns [`ReviewError::Parse`] if the output is malformed.
    async fn review(&self, request: &ReviewRequest) -> Result<ReviewResponse, ReviewError> {
        use std::process::Stdio;
        use tokio::io::AsyncWriteExt;

        let args = Self::build_command_args(request);
        let prompt = Self::build_prompt(request);

        let mut child = tokio::process::Command::new("claude")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(ReviewError::Io)?;

        // Pipe prompt via stdin to avoid OS command-line length limits.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(prompt.as_bytes())
                .await
                .map_err(ReviewError::Io)?;
        }

        let output = child.wait_with_output().await.map_err(ReviewError::Io)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ReviewError::Api(format!(
                "claude CLI exited with {}: {}",
                output.status, stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Self::parse_output(&stdout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::make_review_request;

    /// Fixture: successful Claude Code CLI output with nested result string.
    const CLAUDE_CODE_SUCCESS: &str = include_str!("../../tests/fixtures/claude_code_success.json");
    /// Fixture: error output from Claude Code CLI.
    const CLAUDE_CODE_ERROR: &str = include_str!("../../tests/fixtures/claude_code_error.json");

    #[test]
    fn new_creates_backend() {
        let _backend = ClaudeCodeBackend::new();
    }

    #[test]
    fn parses_success_json_output() {
        let response = ClaudeCodeBackend::parse_output(CLAUDE_CODE_SUCCESS).unwrap();

        assert_eq!(
            response.review.summary,
            "Race condition detected in thread pool."
        );
        assert_eq!(response.review.findings.len(), 1);
        assert_eq!(response.review.findings[0].file, "src/pool.c");
        assert_eq!(response.review.findings[0].line, 88);
    }

    #[test]
    fn parses_nested_result_string() {
        let response = ClaudeCodeBackend::parse_output(CLAUDE_CODE_SUCCESS).unwrap();

        // The `result` field in the fixture is an escaped JSON string,
        // not an inline object — verify it was correctly deserialized.
        assert_eq!(
            response.review.findings[0].title,
            "Data race on shared counter"
        );
        assert_eq!(
            response.review.findings[0].suggestion,
            "Add pthread_mutex_lock before counter access."
        );
    }

    #[test]
    fn estimates_token_usage() {
        let response = ClaudeCodeBackend::parse_output(CLAUDE_CODE_SUCCESS).unwrap();

        // Token estimation: ~4 chars per token.
        // The result string is ~250 chars, so output_tokens should be ~62.
        assert!(
            response.usage.output_tokens > 0,
            "Token usage should be estimated from content length"
        );
        // Cache tokens should be zero — CLI has no prompt caching.
        assert_eq!(response.usage.cache_read_input_tokens, 0);
        assert_eq!(response.usage.cache_creation_input_tokens, 0);
    }

    #[test]
    fn returns_error_on_cli_failure() {
        let result = ClaudeCodeBackend::parse_output(CLAUDE_CODE_ERROR);

        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), ReviewError::Api(_)),
            "CLI error output should produce ReviewError::Api"
        );
    }

    #[test]
    fn build_args_includes_model_and_flags() {
        let mut request = make_review_request();
        request.model = "claude-sonnet-4-5-20250929".to_string();

        let args = ClaudeCodeBackend::build_command_args(&request);

        assert!(
            args.contains(&"--print".to_string()),
            "Args must include --print"
        );
        assert!(
            args.contains(&"--output-format".to_string()),
            "Args must include --output-format"
        );
        assert!(
            args.contains(&"json".to_string()),
            "Args must include json as output format"
        );
        assert!(
            args.contains(&"--model".to_string()),
            "Args must include --model"
        );
        assert!(
            args.contains(&"claude-sonnet-4-5-20250929".to_string()),
            "Args must include the model name"
        );
    }

    #[test]
    fn build_args_excludes_prompt() {
        let request = make_review_request();
        let args = ClaudeCodeBackend::build_command_args(&request);

        assert!(
            !args.iter().any(|a| a.contains("Review batch")),
            "Prompt must not be in args (piped via stdin instead)"
        );
    }

    #[test]
    fn build_prompt_includes_diff_and_json_format() {
        let request = make_review_request();
        let prompt = ClaudeCodeBackend::build_prompt(&request);

        assert!(
            prompt.contains("Review batch"),
            "Prompt should include batch header"
        );
        assert!(
            prompt.contains("Respond ONLY with a JSON object"),
            "Prompt should include JSON format instructions"
        );
    }

    #[test]
    fn build_prompt_includes_custom_instructions() {
        let mut request = make_review_request();
        request.custom_instructions = Some("Check MISRA compliance".to_string());
        let prompt = ClaudeCodeBackend::build_prompt(&request);

        assert!(
            prompt.contains("Check MISRA compliance"),
            "Prompt should include custom instructions"
        );
    }

    #[test]
    fn build_prompt_synthesis_skips_batch_header() {
        let mut request = make_review_request();
        request.file_info = String::new();
        request.diff_content = "=== BATCH 1 ===\npre-built content".to_string();
        let prompt = ClaudeCodeBackend::build_prompt(&request);

        assert!(
            !prompt.contains("Review batch"),
            "Synthesis prompt should not have batch header"
        );
        assert!(
            prompt.contains("=== BATCH 1 ==="),
            "Synthesis prompt should pass through content"
        );
    }

    #[test]
    fn is_send_and_sync() {
        fn assert_send<T: Send>() {}
        fn assert_sync<T: Sync>() {}
        assert_send::<ClaudeCodeBackend>();
        assert_sync::<ClaudeCodeBackend>();
    }

    #[test]
    fn parse_output_handles_null_suggestion() {
        let json = r#"{
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": "{\"summary\":\"Review complete.\",\"findings\":[{\"severity\":\"suggestion\",\"file\":\"src/main.rs\",\"line\":10,\"title\":\"Naming\",\"description\":\"Use snake_case.\",\"suggestion\":null}]}"
        }"#;
        let response = ClaudeCodeBackend::parse_output(json).unwrap();
        assert_eq!(response.review.findings.len(), 1);
        assert_eq!(response.review.findings[0].suggestion, "");
    }
}
