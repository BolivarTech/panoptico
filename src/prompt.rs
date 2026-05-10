// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Prompt construction for review and synthesis requests.

use std::collections::HashSet;

use crate::backend::{CodeReview, ReviewRequest};
use crate::batch::Batch;
use crate::context::FileContext;
use crate::languages::SemanticBatch;

/// Maximum tokens for a single batch review response.
const DEFAULT_REVIEW_MAX_TOKENS: u32 = 4096;

/// Maximum tokens for the synthesis (reduce) response.
const DEFAULT_SYNTHESIS_MAX_TOKENS: u32 = 8192;

/// Default system prompt defining the reviewer role and focus areas.
pub const DEFAULT_SYSTEM_PROMPT: &str = r#"You are an expert code reviewer.

Review the provided code diff and classify each finding by severity:
- Critical: security vulnerabilities, data loss, crashes, hardcoded secrets
- Warning: potential bugs, race conditions, resource leaks, silent failures
- Suggestion: style, naming, readability, documentation improvements
- Positive: good patterns, clean code worth highlighting

Focus areas:
1. Security — validate inputs, avoid injection/overflow, no hardcoded credentials, fail securely
2. Error handling — handle all error cases explicitly, no silent failures, use language-appropriate patterns
3. Resource management — proper cleanup, avoid leaks, prefer stack over heap when possible
4. Quality — low coupling, high cohesion, DRY, Single Responsibility Principle, no magic numbers
5. Concurrency — thread safety, race condition prevention, correct synchronization
6. Style — language-specific conventions, consistent naming, organized imports, reasonable line length
7. Documentation — missing or incorrect docstrings, undocumented contracts, unclear logic without comments"#;

/// System prompt for the synthesis (reduce) phase.
const DEFAULT_SYNTHESIS_PROMPT: &str = "You are an expert code reviewer performing the synthesis \
    phase.\n\n\
    You will receive multiple batch reviews of the same Pull Request. \
    Consolidate them into a single unified code review.\n\n\
    CRITICAL: Every individual finding MUST appear in the `findings` array with all fields \
    (severity, file, line, title, description, suggestion). The `summary` field is ONLY for a \
    brief overall assessment — do NOT list individual findings in the summary.\n\n\
    Rules:\n\
    1. Merge duplicate or overlapping findings into one entry in the `findings` array\n\
    2. Keep the highest severity when duplicates conflict\n\
    3. Preserve all unique findings across batches — each as a separate entry in `findings`\n\
    4. Write a concise summary (2-3 sentences) covering the overall PR quality\n\
    5. Use the same severity levels: critical, warning, suggestion, positive\n\
    6. Never put finding details in the summary — use the `findings` array for that";

/// Chain-of-thought instructions appended to every **review** system prompt.
///
/// Forces the model to reason before assigning severity, reducing
/// over-classification and speculative findings. NOT applied to the
/// synthesis prompt because synthesis consolidates existing findings
/// rather than generating new ones from code.
const CHAIN_OF_THOUGHT_INSTRUCTIONS: &str = r#"

For EACH finding, you MUST include a `reasoning` field with your analysis:
1. What does this code do? (1 sentence)
2. What could go wrong? (specific failure mode, not generic)
3. How likely is this in practice? (certain / likely / possible / unlikely)
4. Why does this warrant the assigned severity? (1 sentence)

Rules:
- If your reasoning concludes "unlikely" and severity is critical or warning,
  downgrade the severity to suggestion or omit the finding entirely.
- Do not flag speculative issues as high severity.
- "Positive" findings do not need likelihood analysis."#;

/// Few-shot examples of findings the model should NOT produce.
/// Injected into every review system prompt to reduce false positives.
const FALSE_POSITIVE_EXAMPLES: &str = r#"

## What NOT to flag — false positive examples

### 1. Pre-existing issues in unchanged code
```diff
  fn process(data: &[u8]) -> Result<(), Error> {
+     let parsed = parse_header(data)?;
      let result = unsafe { transmute(data) };  // Do NOT flag
+     validate(parsed)?;
  }
```
The `unsafe` block is pre-existing (no `+` prefix). Only report issues
on lines with `+` prefix, unless a new change makes an existing line
dangerous.

### 2. Idiomatic unwrap/expect in test code
```rust
#[test]
fn parses_valid_config() {
    let config = Config::from_str(SAMPLE).unwrap();  // Do NOT flag
    assert_eq!(config.model, "claude-sonnet-4-5");
}
```
In test code (`#[test]`, `#[cfg(test)]`, `tests/` directory), `unwrap()`
and `expect()` are idiomatic. Panicking IS the correct behavior in tests.

### 3. Mutex::lock().unwrap() — standard Rust pattern
```rust
let guard = self.state.lock().unwrap();
```
`Mutex::lock()` returns `Err` only if poisoned (another thread panicked).
Unwrapping a poisoned mutex is standard practice. Do not flag unless the
codebase implements cross-thread panic recovery.

### 4. Framework conventions
```python
@app.route("/api/data")
def get_data():
    return jsonify(data)  // Do NOT flag "no error handling"
```
Web framework route handlers (Flask, Django, FastAPI, Express, Actix)
are wrapped by error middleware. The framework catches exceptions. Do not
flag "missing try/except" on route handlers.

### 5. Infallible type conversions
```rust
let value = u64::from(some_u32);  // Do NOT flag
```
Widening conversions (`u32` → `u64`, `i16` → `i32`) are infallible.
Do not flag as "potential overflow" or suggest `try_from`."#;

/// Build a review request for a single batch.
///
/// # Arguments
///
/// * `batch` - The batch of hunks to review.
/// * `batch_number` - Current batch number (1-indexed).
/// * `total_batches` - Total number of batches.
/// * `model` - Model deployment name.
/// * `system_prompt` - System prompt text.
/// * `custom_instructions` - Optional project-specific instructions.
///
/// # Returns
///
/// A `ReviewRequest` ready to send to a backend.
pub fn build_review_request(
    batch: &Batch,
    batch_number: u32,
    total_batches: u32,
    model: &str,
    system_prompt: &str,
    custom_instructions: Option<&str>,
) -> ReviewRequest {
    let enriched_prompt = format!(
        "{}{}{}",
        system_prompt, CHAIN_OF_THOUGHT_INSTRUCTIONS, FALSE_POSITIVE_EXAMPLES,
    );

    ReviewRequest {
        system_prompt: enriched_prompt,
        custom_instructions: custom_instructions.map(String::from),
        diff_content: batch.content(),
        batch_number,
        total_batches,
        file_info: batch.file_list().join(", "),
        model: model.to_string(),
        max_tokens: DEFAULT_REVIEW_MAX_TOKENS,
    }
}

/// Build a synthesis request to merge multiple batch reviews.
///
/// # Arguments
///
/// * `batch_reviews` - Reviews from individual batches.
/// * `model` - Model deployment name.
///
/// # Returns
///
/// A `ReviewRequest` with all batch reviews as content for synthesis.
pub fn build_synthesis_request(batch_reviews: &[CodeReview], model: &str) -> ReviewRequest {
    let mut content = String::new();
    for (i, review) in batch_reviews.iter().enumerate() {
        content.push_str(&format!("=== BATCH {} ===\n", i + 1));
        let json = serde_json::to_string_pretty(review).unwrap_or_else(|_| format!("{:?}", review));
        content.push_str(&json);
        content.push_str("\n\n");
    }

    ReviewRequest {
        system_prompt: DEFAULT_SYNTHESIS_PROMPT.to_string(),
        custom_instructions: None,
        diff_content: content,
        batch_number: 1,
        total_batches: 1,
        file_info: String::new(),
        model: model.to_string(),
        max_tokens: DEFAULT_SYNTHESIS_MAX_TOKENS,
    }
}

/// Format a markdown table of file context metadata for injection
/// into the user prompt before the diff content.
///
/// # Arguments
///
/// * `contexts` - Slice of (file_path, context) pairs.
///
/// # Returns
///
/// A markdown table string. Returns empty string if input is empty.
pub fn format_context_table(contexts: &[(&str, &FileContext)]) -> String {
    if contexts.is_empty() {
        return String::new();
    }
    let mut table = String::from(
        "## File Context\n\
         | File | Language | New | Tests | Lines | Frameworks |\n\
         |------|----------|-----|-------|-------|------------|\n",
    );
    for (file, ctx) in contexts {
        let new_flag = if ctx.is_new_file { "yes" } else { "no" };
        let test_flag = if ctx.has_test_file { "yes" } else { "no" };
        let frameworks = if ctx.framework_hints.is_empty() {
            "\u{2014}".to_string()
        } else {
            ctx.framework_hints.join(", ")
        };
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            file, ctx.language, new_flag, test_flag, ctx.lines_changed, frameworks
        ));
    }
    table.push('\n');
    table
}

/// System prompt for semantic context review mode.
///
/// Used when `--semantic` is enabled. Instructs the model to analyze
/// complete code units rather than raw diff fragments.
pub const SEMANTIC_SYSTEM_PROMPT: &str = r#"You are an expert code reviewer.

For each code unit shown below, you receive:
1. The COMPLETE source code (not just the diff)
2. Which specific lines were MODIFIED in this PR (marked with line numbers)
3. File context metadata (language, new/existing, test coverage, frameworks)

Analyze each unit as a whole. Focus on the modified lines and how they
interact with the surrounding code.

Rules:
- Only report issues on modified lines, unless a modification creates or
  reveals an issue in surrounding code.
- Consider the file context: new files deserve more scrutiny than stable ones.
- Adjust severity based on framework conventions (see false positive examples).

Classify each finding by severity:
- Critical: security vulnerabilities, data loss, crashes, hardcoded secrets
- Warning: potential bugs, race conditions, resource leaks, silent failures
- Suggestion: style, naming, readability, documentation improvements
- Positive: good patterns, clean code worth highlighting"#;

/// Build a review request from a semantic batch.
///
/// Formats each unit with its context metadata, changed lines annotation,
/// and full source code in a fenced code block.
///
/// # Arguments
///
/// * `batch` - The semantic batch of code units.
/// * `batch_number` - Current batch number (1-indexed).
/// * `total_batches` - Total number of batches.
/// * `model` - Model deployment name.
/// * `system_prompt` - System prompt text.
/// * `custom_instructions` - Optional project-specific instructions.
///
/// # Returns
///
/// A [`ReviewRequest`] ready to send to a backend.
pub fn build_semantic_review_request(
    batch: &SemanticBatch,
    batch_number: u32,
    total_batches: u32,
    model: &str,
    system_prompt: &str,
    custom_instructions: Option<&str>,
) -> ReviewRequest {
    let enriched_prompt = format!(
        "{}{}{}",
        system_prompt, CHAIN_OF_THOUGHT_INSTRUCTIONS, FALSE_POSITIVE_EXAMPLES,
    );

    let mut content = String::new();
    for unit in &batch.units {
        let ctx = &unit.context;
        let frameworks = if ctx.framework_hints.is_empty() {
            "none".to_string()
        } else {
            ctx.framework_hints.join(", ")
        };
        let new_flag = if ctx.is_new_file {
            "new file"
        } else {
            "existing"
        };
        let test_flag = if ctx.has_test_file {
            "has tests"
        } else {
            "no tests"
        };
        let changed: Vec<String> = unit.changed_lines.iter().map(|l| l.to_string()).collect();

        content.push_str(&format!(
            "## {:?} `{}` ({})\n\
             **Context**: {} | {} | {} | {} lines changed | {}\n\
             **Lines**: {}-{}\n\
             **Modified**: [{}]\n\
             ```{}\n{}\n```\n\n",
            unit.kind,
            unit.name,
            unit.file,
            ctx.language,
            new_flag,
            test_flag,
            ctx.lines_changed,
            frameworks,
            unit.start_line,
            unit.end_line,
            changed.join(", "),
            ctx.language,
            unit.content,
        ));
    }

    let file_info: Vec<String> = batch.units.iter().map(|u| u.file.clone()).collect();
    let unique_files: HashSet<&str> = file_info.iter().map(|s| s.as_str()).collect();

    ReviewRequest {
        system_prompt: enriched_prompt,
        custom_instructions: custom_instructions.map(String::from),
        diff_content: content,
        batch_number,
        total_batches,
        file_info: unique_files.into_iter().collect::<Vec<_>>().join(", "),
        model: model.to_string(),
        max_tokens: DEFAULT_REVIEW_MAX_TOKENS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::make_test_finding;
    use crate::backend::CodeReview;
    use crate::hunk::Hunk;

    /// Helper to create a batch with known content.
    fn make_batch(files: &[(&str, usize)]) -> Batch {
        let hunks: Vec<Hunk> = files
            .iter()
            .map(|(file, lines)| {
                let content = (0..*lines)
                    .map(|i| format!("line {}", i))
                    .collect::<Vec<_>>()
                    .join("\n");
                Hunk {
                    file: file.to_string(),
                    content,
                    lines: *lines,
                }
            })
            .collect();
        let total_lines = hunks.iter().map(|h| h.lines).sum();
        Batch { hunks, total_lines }
    }

    fn make_review(summary: &str, findings_count: usize) -> CodeReview {
        let findings = (0..findings_count)
            .map(|i| {
                let mut f = make_test_finding(
                    &format!("file{}.c", i),
                    (i + 1) as u32,
                    &format!("Finding {}", i),
                );
                f.description = format!("Description {}", i);
                f.suggestion = format!("Fix {}", i);
                f
            })
            .collect();
        CodeReview {
            summary: summary.to_string(),
            findings,
        }
    }

    // --- build_review_request ---

    #[test]
    fn review_request_uses_provided_system_prompt() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(&batch, 1, 1, "claude-sonnet-4-5", "custom prompt", None);
        assert!(
            req.system_prompt.starts_with("custom prompt"),
            "Enriched system prompt should start with the provided prompt"
        );
    }

    #[test]
    fn review_request_includes_custom_instructions() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            Some("Check for MISRA compliance"),
        );
        assert_eq!(
            req.custom_instructions.as_deref(),
            Some("Check for MISRA compliance"),
            "Custom instructions should be included"
        );
    }

    #[test]
    fn review_request_none_custom_instructions_when_absent() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.custom_instructions.is_none(),
            "Custom instructions should be None when not provided"
        );
    }

    #[test]
    fn review_request_batch_numbers_set_correctly() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            3,
            5,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert_eq!(req.batch_number, 3);
        assert_eq!(req.total_batches, 5);
    }

    #[test]
    fn review_request_model_set_correctly() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert_eq!(req.model, "claude-sonnet-4-5");
    }

    #[test]
    fn review_request_max_tokens_is_4096() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert_eq!(
            req.max_tokens, 4096,
            "Review requests should default to 4096 max tokens"
        );
    }

    #[test]
    fn review_request_diff_content_from_batch() {
        let batch = make_batch(&[("main.c", 3)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            !req.diff_content.is_empty(),
            "Diff content should come from the batch"
        );
    }

    #[test]
    fn review_request_file_info_lists_batch_files() {
        let batch = make_batch(&[("main.c", 5), ("utils.h", 5)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.file_info.contains("main.c"),
            "file_info should contain batch file names"
        );
        assert!(
            req.file_info.contains("utils.h"),
            "file_info should contain batch file names"
        );
    }

    // --- build_synthesis_request ---

    #[test]
    fn synthesis_request_contains_all_batch_reviews() {
        let reviews = vec![
            make_review("Batch 1 summary", 2),
            make_review("Batch 2 summary", 1),
        ];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert!(
            req.diff_content.contains("Batch 1 summary"),
            "Synthesis content should include batch 1 review"
        );
        assert!(
            req.diff_content.contains("Batch 2 summary"),
            "Synthesis content should include batch 2 review"
        );
    }

    #[test]
    fn synthesis_request_has_batch_labels() {
        let reviews = vec![make_review("First", 1), make_review("Second", 1)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert!(
            req.diff_content.contains("BATCH 1"),
            "Synthesis should label batch 1"
        );
        assert!(
            req.diff_content.contains("BATCH 2"),
            "Synthesis should label batch 2"
        );
    }

    #[test]
    fn synthesis_request_model_set_correctly() {
        let reviews = vec![make_review("Summary", 0)];
        let req = build_synthesis_request(&reviews, "claude-haiku-4-5");
        assert_eq!(req.model, "claude-haiku-4-5");
    }

    #[test]
    fn synthesis_request_max_tokens_is_8192() {
        let reviews = vec![make_review("Summary", 0)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert_eq!(
            req.max_tokens, 8192,
            "Synthesis requests should use 8192 max tokens"
        );
    }

    #[test]
    fn synthesis_request_no_custom_instructions() {
        let reviews = vec![make_review("Summary", 0)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert!(
            req.custom_instructions.is_none(),
            "Synthesis requests should not have custom instructions"
        );
    }

    #[test]
    fn synthesis_request_system_prompt_has_consolidation_rules() {
        let reviews = vec![make_review("Summary", 1)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        let rules = [
            "duplicate",
            "highest severity",
            "unique findings",
            "summary",
            "critical",
        ];
        for rule in &rules {
            assert!(
                req.system_prompt.to_lowercase().contains(rule),
                "Synthesis system prompt should mention '{}': {}",
                rule,
                req.system_prompt
            );
        }
    }

    #[test]
    fn synthesis_request_file_info_is_empty() {
        let reviews = vec![make_review("Summary", 1)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert!(
            req.file_info.is_empty(),
            "Synthesis file_info must be empty to signal skip of batch header"
        );
    }

    // --- DEFAULT_SYSTEM_PROMPT ---

    #[test]
    fn default_system_prompt_mentions_key_focus_areas() {
        let areas = [
            "Security",
            "Error handling",
            "Resource management",
            "Quality",
            "Concurrency",
            "Style",
            "Documentation",
        ];
        for area in &areas {
            assert!(
                DEFAULT_SYSTEM_PROMPT.contains(area),
                "Should mention focus area: {}",
                area
            );
        }
    }

    // --- Phase 1 (G2): Chain-of-thought reasoning tests ---

    #[test]
    fn system_prompt_contains_cot_instructions() {
        let batch = make_batch(&[("test.rs", 5)]);
        let request = build_review_request(&batch, 1, 1, "model", "Base prompt", None);
        assert!(
            request.system_prompt.contains("reasoning"),
            "Enriched system prompt must mention 'reasoning'"
        );
        assert!(
            request.system_prompt.contains("What could go wrong"),
            "Enriched system prompt must contain CoT step 'What could go wrong'"
        );
        assert!(
            request.system_prompt.starts_with("Base prompt"),
            "Enriched system prompt must start with the original prompt"
        );
    }

    // --- Phase 2 (G3): Few-shot false positive examples tests ---

    #[test]
    fn review_request_prompt_contains_fp_examples() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.system_prompt.contains("What NOT to flag"),
            "Review system prompt must contain false positive examples section header"
        );
    }

    #[test]
    fn review_request_prompt_contains_unchanged_code_example() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.system_prompt.contains("pre-existing"),
            "FP examples must include the unchanged/pre-existing code example"
        );
    }

    #[test]
    fn review_request_prompt_contains_test_unwrap_example() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.system_prompt.contains("test code")
                || req.system_prompt.contains("test") && req.system_prompt.contains("unwrap"),
            "FP examples must include the test code unwrap example"
        );
    }

    #[test]
    fn review_request_prompt_contains_mutex_example() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.system_prompt.contains("Mutex"),
            "FP examples must include the Mutex::lock().unwrap() example"
        );
    }

    #[test]
    fn review_request_prompt_contains_infallible_example() {
        let batch = make_batch(&[("main.c", 10)]);
        let req = build_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            DEFAULT_SYSTEM_PROMPT,
            None,
        );
        assert!(
            req.system_prompt.contains("infallible"),
            "FP examples must include the infallible type conversion example"
        );
    }

    #[test]
    fn review_request_custom_prompt_also_gets_fp_examples() {
        let batch = make_batch(&[("main.c", 10)]);
        let custom = "You are a security-focused reviewer.";
        let req = build_review_request(&batch, 1, 1, "claude-sonnet-4-5", custom, None);
        assert!(
            req.system_prompt.starts_with(custom),
            "Custom system prompt must be preserved at the start"
        );
        assert!(
            req.system_prompt.contains("What NOT to flag"),
            "FP examples must be appended even with a custom system prompt"
        );
    }

    #[test]
    fn synthesis_request_excludes_fp_examples() {
        let reviews = vec![make_review("Summary", 1)];
        let req = build_synthesis_request(&reviews, "claude-sonnet-4-5");
        assert!(
            !req.system_prompt.contains("What NOT to flag"),
            "Synthesis system prompt must NOT contain false positive examples"
        );
    }

    // --- Phase 3 (G1): Context enrichment tests ---

    #[test]
    fn format_context_table_generates_markdown() {
        let ctx = FileContext {
            language: "rust".to_string(),
            is_new_file: false,
            has_test_file: true,
            lines_changed: 42,
            framework_hints: vec!["tokio".to_string()],
        };
        let table = format_context_table(&[("src/main.rs", &ctx)]);
        assert!(
            table.contains("| File |"),
            "Table should contain header row: {}",
            table
        );
        assert!(
            table.contains("src/main.rs"),
            "Table should contain file path: {}",
            table
        );
        assert!(
            table.contains("rust"),
            "Table should contain language: {}",
            table
        );
    }

    #[test]
    fn format_context_table_empty_returns_empty() {
        let table = format_context_table(&[]);
        assert!(
            table.is_empty(),
            "Empty input should produce empty string, got: {}",
            table
        );
    }

    #[test]
    fn format_context_table_shows_framework_hints() {
        let ctx = FileContext {
            language: "rust".to_string(),
            is_new_file: true,
            has_test_file: true,
            lines_changed: 10,
            framework_hints: vec!["tokio".to_string(), "serde".to_string()],
        };
        let table = format_context_table(&[("src/lib.rs", &ctx)]);
        assert!(
            table.contains("tokio"),
            "Table should contain framework hint 'tokio': {}",
            table
        );
        assert!(
            table.contains("serde"),
            "Table should contain framework hint 'serde': {}",
            table
        );
    }

    // --- Phase 4 (G0): Semantic review request test ---

    #[test]
    fn build_semantic_review_request_includes_metadata() {
        use crate::languages::{SemanticBatch, SemanticUnit, UnitKind};

        let unit = SemanticUnit {
            kind: UnitKind::Function,
            name: "process_data".to_string(),
            file: "src/main.rs".to_string(),
            start_line: 10,
            end_line: 25,
            content: "fn process_data() {\n    // body\n}".to_string(),
            changed_lines: vec![12, 15],
            context: FileContext {
                language: "rust".to_string(),
                is_new_file: false,
                has_test_file: true,
                lines_changed: 2,
                framework_hints: vec!["tokio".to_string()],
            },
        };
        let batch = SemanticBatch {
            units: vec![unit],
            estimated_tokens: 100,
        };

        let req = build_semantic_review_request(
            &batch,
            1,
            1,
            "claude-sonnet-4-5",
            SEMANTIC_SYSTEM_PROMPT,
            None,
        );

        assert!(
            req.diff_content.contains("**Context**"),
            "Semantic request must contain '**Context**' metadata line"
        );
        assert!(
            req.diff_content.contains("**Modified**"),
            "Semantic request must contain '**Modified**' line listing changed lines"
        );
        assert!(
            req.diff_content.contains("12, 15"),
            "Modified line numbers must be listed"
        );
        assert!(
            req.diff_content.contains("process_data"),
            "Unit name must appear in output"
        );
        assert!(
            req.diff_content.contains("```rust"),
            "Content must be in a fenced code block with language tag"
        );
        assert!(
            req.file_info.contains("src/main.rs"),
            "File info must list the unit's file"
        );
    }
}
