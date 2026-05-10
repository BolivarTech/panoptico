// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-17

//! Deterministic finding ID generation for stable deduplication.
//!
//! Provides [`Category`] classification, path normalization, and SHA256-based
//! ID generation for code review findings. IDs are stable across runs even
//! when AI-generated titles change, enabling reliable deduplication in CI pipelines.
//!
//! # Algorithm
//!
//! ```text
//! findingId = SHA256(normalize(file) + ":" + line + ":" + category)[:16]
//! ```
//!
//! The 16-character hex prefix provides 64 bits of entropy, sufficient to
//! avoid collisions within a single PR's findings.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backend::CodeReview;

/// Length of the hex-encoded finding ID prefix.
pub const FINDING_ID_HEX_LENGTH: usize = 16;

/// Controlled-vocabulary classification for code review findings.
///
/// Used as part of the deterministic [`generate_finding_id`] hash input,
/// ensuring stable IDs even when AI-generated titles vary between runs.
///
/// # Serialization
///
/// Serializes to/from kebab-case slugs (e.g., `buffer-overflow`, `null-deref`).
///
/// # Examples
///
/// ```ignore
/// use panoptico::finding_id::Category;
///
/// let cat: Category = serde_json::from_str("\"buffer-overflow\"").unwrap();
/// assert_eq!(cat, Category::BufferOverflow);
/// assert_eq!(serde_json::to_string(&cat).unwrap(), "\"buffer-overflow\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Out-of-bounds read/write.
    BufferOverflow,
    /// Null/dangling pointer dereference.
    NullDeref,
    /// Unclosed file, socket, memory, handle.
    ResourceLeak,
    /// Missing input validation or sanitization.
    UnvalidatedInput,
    /// Data race or TOCTOU.
    RaceCondition,
    /// Missing or incorrect error handling.
    ErrorHandling,
    /// Credentials, keys, tokens in source.
    HardcodedSecret,
    /// Arithmetic overflow/underflow.
    IntegerOverflow,
    /// Command, SQL, or format-string injection.
    Injection,
    /// Incorrect control flow or business logic.
    LogicError,
    /// Type confusion or incorrect cast.
    TypeMismatch,
    /// Use of deprecated or removed API.
    DeprecatedApi,
    /// Unnecessary allocation, O(n^2) in hot path, etc.
    Performance,
    /// Naming, formatting, convention violations.
    Style,
    /// Missing, incorrect, or incomplete documentation.
    Documentation,
    /// Fallback for unclassified findings.
    ///
    /// `#[serde(other)]` causes any unrecognized slug to deserialize
    /// as `Other` instead of failing. This is essential because LLMs
    /// may invent category names not in the enum (e.g., `"memory-leak"`).
    #[serde(other)]
    Other,
}

impl Category {
    /// Return the kebab-case slug for this category variant.
    ///
    /// Matches the serde `rename_all = "kebab-case"` serialization,
    /// but avoids the round-trip through `serde_json`.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use panoptico::finding_id::Category;
    ///
    /// assert_eq!(Category::BufferOverflow.slug(), "buffer-overflow");
    /// assert_eq!(Category::Other.slug(), "other");
    /// ```
    pub fn slug(&self) -> &'static str {
        match self {
            Category::BufferOverflow => "buffer-overflow",
            Category::NullDeref => "null-deref",
            Category::ResourceLeak => "resource-leak",
            Category::UnvalidatedInput => "unvalidated-input",
            Category::RaceCondition => "race-condition",
            Category::ErrorHandling => "error-handling",
            Category::HardcodedSecret => "hardcoded-secret",
            Category::IntegerOverflow => "integer-overflow",
            Category::Injection => "injection",
            Category::LogicError => "logic-error",
            Category::TypeMismatch => "type-mismatch",
            Category::DeprecatedApi => "deprecated-api",
            Category::Performance => "performance",
            Category::Style => "style",
            Category::Documentation => "documentation",
            Category::Other => "other",
        }
    }
}

/// All category slugs in canonical order.
///
/// Single source of truth for the complete list of kebab-case
/// category identifiers. Used by `review_tool()` in `backend/mod.rs`
/// and `ClaudeCodeBackend::build_prompt()` in `backend/claude_code.rs`
/// to avoid duplicating the slug list.
pub const CATEGORY_SLUGS: &[&str] = &[
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

impl Default for Category {
    /// Returns [`Category::Other`] as the default.
    fn default() -> Self {
        Category::Other
    }
}

/// Normalize a file path for deterministic hashing.
///
/// Converts backslashes to forward slashes, strips leading `./`,
/// and collapses consecutive slashes.
///
/// # Arguments
///
/// * `path` - The file path to normalize.
///
/// # Returns
///
/// A normalized path string suitable for hash input.
///
/// # Examples
///
/// ```ignore
/// use panoptico::finding_id::normalize_path;
///
/// assert_eq!(normalize_path("src\\main.rs"), "src/main.rs");
/// assert_eq!(normalize_path("./src/lib.rs"), "src/lib.rs");
/// ```
pub fn normalize_path(path: &str) -> String {
    let mut result = String::with_capacity(path.len());
    let mut prev_slash = false;
    let mut chars = path.chars().peekable();

    // Strip leading "./" or ".\" (after implicit backslash→slash conversion).
    if chars.peek() == Some(&'.') {
        let mut probe = chars.clone();
        probe.next(); // consume '.'
        match probe.peek() {
            Some('/' | '\\') => {
                chars.next(); // skip '.'
                chars.next(); // skip separator
            }
            None => return String::new(), // bare "."
            _ => {}
        }
    }

    for ch in chars {
        let ch = if ch == '\\' { '/' } else { ch };
        if ch == '/' {
            if !prev_slash || result.is_empty() {
                result.push('/');
            }
            prev_slash = true;
        } else {
            result.push(ch);
            prev_slash = false;
        }
    }
    result
}

/// Generate a deterministic finding ID from file, line, and category.
///
/// Computes `SHA256(normalize(file) + ":" + line + ":" + category)` and
/// returns the first [`FINDING_ID_HEX_LENGTH`] hex characters.
///
/// # Arguments
///
/// * `file` - Source file path (will be normalized).
/// * `line` - Line number (use 0 for file-level findings).
/// * `category` - The finding category.
///
/// # Returns
///
/// A lowercase hex string of length [`FINDING_ID_HEX_LENGTH`].
pub fn generate_finding_id(file: &str, line: u32, category: &Category) -> String {
    let normalized = normalize_path(file);
    let input = format!("{}:{}:{}", normalized, line, category.slug());
    let hash = Sha256::digest(input.as_bytes());
    hex_encode(&hash[..FINDING_ID_HEX_LENGTH / 2])
}

/// Encode bytes as lowercase hexadecimal.
fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{:02x}", b);
    }
    s
}

/// Assign deterministic IDs to all findings in a code review.
///
/// Iterates over each finding and sets its `finding_id` field using
/// [`generate_finding_id`] with the finding's file, line, and category.
///
/// # Arguments
///
/// * `review` - The code review whose findings will receive IDs.
pub fn assign_finding_ids(review: &mut CodeReview) {
    for finding in &mut review.findings {
        finding.finding_id = generate_finding_id(&finding.file, finding.line, &finding.category);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{CodeReview, Finding, Severity};

    // ── A. Category serde (29 tests — all PASS) ──────────────────

    #[test]
    fn category_serialize_buffer_overflow() {
        assert_eq!(
            serde_json::to_string(&Category::BufferOverflow).unwrap(),
            "\"buffer-overflow\""
        );
    }

    #[test]
    fn category_serialize_null_deref() {
        assert_eq!(
            serde_json::to_string(&Category::NullDeref).unwrap(),
            "\"null-deref\""
        );
    }

    #[test]
    fn category_serialize_resource_leak() {
        assert_eq!(
            serde_json::to_string(&Category::ResourceLeak).unwrap(),
            "\"resource-leak\""
        );
    }

    #[test]
    fn category_serialize_unvalidated_input() {
        assert_eq!(
            serde_json::to_string(&Category::UnvalidatedInput).unwrap(),
            "\"unvalidated-input\""
        );
    }

    #[test]
    fn category_serialize_race_condition() {
        assert_eq!(
            serde_json::to_string(&Category::RaceCondition).unwrap(),
            "\"race-condition\""
        );
    }

    #[test]
    fn category_serialize_error_handling() {
        assert_eq!(
            serde_json::to_string(&Category::ErrorHandling).unwrap(),
            "\"error-handling\""
        );
    }

    #[test]
    fn category_serialize_hardcoded_secret() {
        assert_eq!(
            serde_json::to_string(&Category::HardcodedSecret).unwrap(),
            "\"hardcoded-secret\""
        );
    }

    #[test]
    fn category_serialize_integer_overflow() {
        assert_eq!(
            serde_json::to_string(&Category::IntegerOverflow).unwrap(),
            "\"integer-overflow\""
        );
    }

    #[test]
    fn category_serialize_injection() {
        assert_eq!(
            serde_json::to_string(&Category::Injection).unwrap(),
            "\"injection\""
        );
    }

    #[test]
    fn category_serialize_logic_error() {
        assert_eq!(
            serde_json::to_string(&Category::LogicError).unwrap(),
            "\"logic-error\""
        );
    }

    #[test]
    fn category_serialize_type_mismatch() {
        assert_eq!(
            serde_json::to_string(&Category::TypeMismatch).unwrap(),
            "\"type-mismatch\""
        );
    }

    #[test]
    fn category_serialize_deprecated_api() {
        assert_eq!(
            serde_json::to_string(&Category::DeprecatedApi).unwrap(),
            "\"deprecated-api\""
        );
    }

    #[test]
    fn category_serialize_performance() {
        assert_eq!(
            serde_json::to_string(&Category::Performance).unwrap(),
            "\"performance\""
        );
    }

    #[test]
    fn category_serialize_style() {
        assert_eq!(
            serde_json::to_string(&Category::Style).unwrap(),
            "\"style\""
        );
    }

    #[test]
    fn category_serialize_documentation() {
        assert_eq!(
            serde_json::to_string(&Category::Documentation).unwrap(),
            "\"documentation\""
        );
    }

    #[test]
    fn category_serialize_other() {
        assert_eq!(
            serde_json::to_string(&Category::Other).unwrap(),
            "\"other\""
        );
    }

    #[test]
    fn category_deserialize_buffer_overflow() {
        let cat: Category = serde_json::from_str("\"buffer-overflow\"").unwrap();
        assert_eq!(cat, Category::BufferOverflow);
    }

    #[test]
    fn category_deserialize_null_deref() {
        let cat: Category = serde_json::from_str("\"null-deref\"").unwrap();
        assert_eq!(cat, Category::NullDeref);
    }

    #[test]
    fn category_deserialize_error_handling() {
        let cat: Category = serde_json::from_str("\"error-handling\"").unwrap();
        assert_eq!(cat, Category::ErrorHandling);
    }

    #[test]
    fn category_deserialize_documentation() {
        let cat: Category = serde_json::from_str("\"documentation\"").unwrap();
        assert_eq!(cat, Category::Documentation);
    }

    #[test]
    fn category_deserialize_other() {
        let cat: Category = serde_json::from_str("\"other\"").unwrap();
        assert_eq!(cat, Category::Other);
    }

    #[test]
    fn category_unknown_slug_falls_back_to_other() {
        let cat: Category = serde_json::from_str("\"unknown-category\"").unwrap();
        assert_eq!(
            cat,
            Category::Other,
            "Unknown slug should fall back to Other"
        );
    }

    #[test]
    fn category_empty_string_falls_back_to_other() {
        let cat: Category = serde_json::from_str("\"\"").unwrap();
        assert_eq!(
            cat,
            Category::Other,
            "Empty string should fall back to Other"
        );
    }

    #[test]
    fn category_rejects_numeric() {
        let result = serde_json::from_str::<Category>("42");
        assert!(result.is_err(), "Numeric value should fail deserialization");
    }

    #[test]
    fn category_clone() {
        let cat = Category::BufferOverflow;
        let cloned = cat;
        assert_eq!(cat, cloned);
    }

    #[test]
    fn category_debug() {
        let debug = format!("{:?}", Category::NullDeref);
        assert!(
            debug.contains("NullDeref"),
            "Debug should contain variant name"
        );
    }

    #[test]
    fn category_equality() {
        assert_eq!(Category::Style, Category::Style);
    }

    #[test]
    fn category_inequality() {
        assert_ne!(Category::Style, Category::Performance);
    }

    #[test]
    fn category_default_is_other() {
        assert_eq!(Category::default(), Category::Other);
    }

    #[test]
    fn category_roundtrip_all_variants() {
        let variants = [
            Category::BufferOverflow,
            Category::NullDeref,
            Category::ResourceLeak,
            Category::UnvalidatedInput,
            Category::RaceCondition,
            Category::ErrorHandling,
            Category::HardcodedSecret,
            Category::IntegerOverflow,
            Category::Injection,
            Category::LogicError,
            Category::TypeMismatch,
            Category::DeprecatedApi,
            Category::Performance,
            Category::Style,
            Category::Documentation,
            Category::Other,
        ];
        for variant in &variants {
            let json = serde_json::to_string(variant).unwrap();
            let deserialized: Category = serde_json::from_str(&json).unwrap();
            assert_eq!(variant, &deserialized, "Roundtrip failed for {:?}", variant);
        }
    }

    #[test]
    fn category_slug_matches_serde_for_all_variants() {
        let variants = [
            Category::BufferOverflow,
            Category::NullDeref,
            Category::ResourceLeak,
            Category::UnvalidatedInput,
            Category::RaceCondition,
            Category::ErrorHandling,
            Category::HardcodedSecret,
            Category::IntegerOverflow,
            Category::Injection,
            Category::LogicError,
            Category::TypeMismatch,
            Category::DeprecatedApi,
            Category::Performance,
            Category::Style,
            Category::Documentation,
            Category::Other,
        ];
        for variant in &variants {
            let serde_slug = serde_json::to_string(variant).unwrap();
            let serde_slug = serde_slug.trim_matches('"');
            assert_eq!(
                variant.slug(),
                serde_slug,
                "slug() mismatch for {:?}",
                variant
            );
        }
    }

    #[test]
    fn category_slugs_constant_matches_all_variants() {
        let variants = [
            Category::BufferOverflow,
            Category::NullDeref,
            Category::ResourceLeak,
            Category::UnvalidatedInput,
            Category::RaceCondition,
            Category::ErrorHandling,
            Category::HardcodedSecret,
            Category::IntegerOverflow,
            Category::Injection,
            Category::LogicError,
            Category::TypeMismatch,
            Category::DeprecatedApi,
            Category::Performance,
            Category::Style,
            Category::Documentation,
            Category::Other,
        ];
        assert_eq!(
            CATEGORY_SLUGS.len(),
            variants.len(),
            "CATEGORY_SLUGS length must match variant count"
        );
        for (i, variant) in variants.iter().enumerate() {
            assert_eq!(
                CATEGORY_SLUGS[i],
                variant.slug(),
                "CATEGORY_SLUGS[{}] mismatch for {:?}",
                i,
                variant
            );
        }
    }

    #[test]
    fn category_variant_count_is_sixteen() {
        let variants = [
            Category::BufferOverflow,
            Category::NullDeref,
            Category::ResourceLeak,
            Category::UnvalidatedInput,
            Category::RaceCondition,
            Category::ErrorHandling,
            Category::HardcodedSecret,
            Category::IntegerOverflow,
            Category::Injection,
            Category::LogicError,
            Category::TypeMismatch,
            Category::DeprecatedApi,
            Category::Performance,
            Category::Style,
            Category::Documentation,
            Category::Other,
        ];
        assert_eq!(
            variants.len(),
            16,
            "Category should have exactly 16 variants"
        );
    }

    // ── B. Path normalization (15 tests) ─────────────────────────

    #[test]
    fn normalize_converts_backslashes_to_forward_slashes() {
        assert_eq!(normalize_path("src\\main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_converts_nested_backslashes() {
        assert_eq!(normalize_path("src\\backend\\mod.rs"), "src/backend/mod.rs");
    }

    #[test]
    fn normalize_strips_leading_dot_slash() {
        assert_eq!(normalize_path("./src/main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_strips_leading_dot_backslash() {
        assert_eq!(normalize_path(".\\src\\main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_preserves_already_normalized_path() {
        assert_eq!(normalize_path("src/main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_empty_path_returns_empty() {
        assert_eq!(normalize_path(""), "");
    }

    #[test]
    fn normalize_preserves_leading_slash() {
        assert_eq!(normalize_path("/usr/local/bin"), "/usr/local/bin");
    }

    #[test]
    fn normalize_collapses_double_slashes() {
        assert_eq!(normalize_path("src//main.rs"), "src/main.rs");
    }

    #[test]
    fn normalize_collapses_multiple_slashes() {
        assert_eq!(
            normalize_path("src///backend///mod.rs"),
            "src/backend/mod.rs"
        );
    }

    #[test]
    fn normalize_handles_trailing_slash() {
        assert_eq!(normalize_path("src/backend/"), "src/backend/");
    }

    #[test]
    fn normalize_preserves_spaces_in_path() {
        assert_eq!(
            normalize_path("my project/src/main.rs"),
            "my project/src/main.rs"
        );
    }

    #[test]
    fn normalize_handles_unicode_path() {
        assert_eq!(
            normalize_path("src/módulo/archivo.rs"),
            "src/módulo/archivo.rs"
        );
    }

    #[test]
    fn normalize_windows_drive_path() {
        assert_eq!(
            normalize_path("C:\\Users\\dev\\main.rs"),
            "C:/Users/dev/main.rs"
        );
    }

    #[test]
    fn normalize_dot_only_returns_empty() {
        assert_eq!(normalize_path("."), "");
    }

    #[test]
    fn normalize_mixed_separators() {
        assert_eq!(normalize_path("src\\backend/mod.rs"), "src/backend/mod.rs");
    }

    // ── C. Finding ID generation (15 tests) ──────────────────────

    #[test]
    fn finding_id_is_deterministic() {
        let id1 = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id2 = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        assert_eq!(id1, id2, "Same inputs must produce same ID");
    }

    #[test]
    fn finding_id_has_sixteen_hex_chars() {
        let id = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "ID must be {} chars",
            FINDING_ID_HEX_LENGTH
        );
    }

    #[test]
    fn finding_id_contains_only_hex_chars() {
        let id = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "ID '{}' must contain only hex characters",
            id
        );
    }

    #[test]
    fn finding_id_is_lowercase() {
        let id = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        assert_eq!(id, id.to_lowercase(), "ID must be lowercase hex");
    }

    #[test]
    fn finding_id_differs_for_different_files() {
        let id1 = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id2 = generate_finding_id("src/lib.rs", 42, &Category::BufferOverflow);
        assert_ne!(id1, id2, "Different files must produce different IDs");
    }

    #[test]
    fn finding_id_differs_for_different_lines() {
        let id1 = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id2 = generate_finding_id("src/main.rs", 100, &Category::BufferOverflow);
        assert_ne!(id1, id2, "Different lines must produce different IDs");
    }

    #[test]
    fn finding_id_differs_for_different_categories() {
        let id1 = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id2 = generate_finding_id("src/main.rs", 42, &Category::NullDeref);
        assert_ne!(id1, id2, "Different categories must produce different IDs");
    }

    #[test]
    fn finding_id_line_zero_for_no_line_findings() {
        let id = generate_finding_id("src/main.rs", 0, &Category::Style);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "Line 0 should produce valid ID"
        );
    }

    #[test]
    fn finding_id_normalizes_backslash_input() {
        let id_fwd = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id_back = generate_finding_id("src\\main.rs", 42, &Category::BufferOverflow);
        assert_eq!(
            id_fwd, id_back,
            "Backslash path should normalize to same ID"
        );
    }

    #[test]
    fn finding_id_normalizes_dot_prefix() {
        let id_clean = generate_finding_id("src/main.rs", 42, &Category::BufferOverflow);
        let id_dot = generate_finding_id("./src/main.rs", 42, &Category::BufferOverflow);
        assert_eq!(
            id_clean, id_dot,
            "Dot-prefix path should normalize to same ID"
        );
    }

    #[test]
    fn finding_id_empty_file_path() {
        let id = generate_finding_id("", 0, &Category::Other);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "Empty file path should produce valid ID"
        );
    }

    #[test]
    fn finding_id_very_long_file_path() {
        let long_path = "a/".repeat(500) + "main.rs";
        let id = generate_finding_id(&long_path, 1, &Category::Style);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "Long path should produce valid ID"
        );
    }

    #[test]
    fn finding_id_special_chars_in_path() {
        let id = generate_finding_id("src/my file (2).rs", 10, &Category::LogicError);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "Special chars should produce valid ID"
        );
    }

    #[test]
    fn finding_id_max_line_number() {
        let id = generate_finding_id("src/main.rs", u32::MAX, &Category::Style);
        assert_eq!(
            id.len(),
            FINDING_ID_HEX_LENGTH,
            "Max line number should produce valid ID"
        );
    }

    #[test]
    fn finding_id_all_categories_produce_unique_ids_same_file_line() {
        let variants = [
            Category::BufferOverflow,
            Category::NullDeref,
            Category::ResourceLeak,
            Category::UnvalidatedInput,
            Category::RaceCondition,
            Category::ErrorHandling,
            Category::HardcodedSecret,
            Category::IntegerOverflow,
            Category::Injection,
            Category::LogicError,
            Category::TypeMismatch,
            Category::DeprecatedApi,
            Category::Performance,
            Category::Style,
            Category::Other,
        ];
        let ids: Vec<String> = variants
            .iter()
            .map(|cat| generate_finding_id("src/main.rs", 42, cat))
            .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            ids.len(),
            unique.len(),
            "All categories should produce unique IDs for same file:line"
        );
    }

    // ── D. assign_finding_ids (8 tests) ──────────────────────────

    fn make_finding(file: &str, line: u32, category: Category) -> Finding {
        let mut f = crate::backend::mock::make_test_finding(file, line, "Test finding");
        f.category = category;
        f
    }

    #[test]
    fn assign_ids_empty_findings_unchanged() {
        let mut review = CodeReview {
            summary: "No findings".to_string(),
            findings: vec![],
        };
        assign_finding_ids(&mut review);
        assert!(review.findings.is_empty());
    }

    #[test]
    fn assign_ids_single_finding_gets_id() {
        let mut review = CodeReview {
            summary: "One finding".to_string(),
            findings: vec![make_finding("src/main.rs", 42, Category::BufferOverflow)],
        };
        assign_finding_ids(&mut review);
        assert!(
            !review.findings[0].finding_id.is_empty(),
            "Finding should receive an ID"
        );
        assert_eq!(review.findings[0].finding_id.len(), FINDING_ID_HEX_LENGTH);
    }

    #[test]
    fn assign_ids_multiple_findings_get_unique_ids() {
        let mut review = CodeReview {
            summary: "Multiple findings".to_string(),
            findings: vec![
                make_finding("src/main.rs", 42, Category::BufferOverflow),
                make_finding("src/main.rs", 100, Category::NullDeref),
                make_finding("src/lib.rs", 10, Category::Style),
            ],
        };
        assign_finding_ids(&mut review);
        let ids: Vec<&str> = review
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "All finding IDs should be unique");
    }

    #[test]
    fn assign_ids_deterministic_across_calls() {
        let make_review = || CodeReview {
            summary: "Deterministic test".to_string(),
            findings: vec![make_finding("src/main.rs", 42, Category::BufferOverflow)],
        };
        let mut review1 = make_review();
        let mut review2 = make_review();
        assign_finding_ids(&mut review1);
        assign_finding_ids(&mut review2);
        assert_eq!(
            review1.findings[0].finding_id, review2.findings[0].finding_id,
            "Same input must produce same ID across calls"
        );
    }

    #[test]
    fn assign_ids_different_category_same_file_line_different_id() {
        let mut review = CodeReview {
            summary: "Category test".to_string(),
            findings: vec![
                make_finding("src/main.rs", 42, Category::BufferOverflow),
                make_finding("src/main.rs", 42, Category::NullDeref),
            ],
        };
        assign_finding_ids(&mut review);
        assert_ne!(
            review.findings[0].finding_id, review.findings[1].finding_id,
            "Different categories at same file:line should produce different IDs"
        );
    }

    #[test]
    fn assign_ids_preserves_summary() {
        let mut review = CodeReview {
            summary: "Important summary".to_string(),
            findings: vec![make_finding("src/main.rs", 1, Category::Style)],
        };
        assign_finding_ids(&mut review);
        assert_eq!(
            review.summary, "Important summary",
            "Summary must not be modified"
        );
    }

    #[test]
    fn assign_ids_preserves_other_finding_fields() {
        let mut review = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Critical,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Original title".to_string(),
                description: "Original desc".to_string(),
                suggestion: "Original fix".to_string(),
                category: Category::BufferOverflow,
                finding_id: String::new(),
                reasoning: String::new(),
            }],
        };
        assign_finding_ids(&mut review);
        let f = &review.findings[0];
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.file, "src/main.rs");
        assert_eq!(f.line, 42);
        assert_eq!(f.title, "Original title");
        assert_eq!(f.description, "Original desc");
        assert_eq!(f.suggestion, "Original fix");
        assert_eq!(f.category, Category::BufferOverflow);
    }

    #[test]
    fn assign_ids_independent_of_title() {
        let mut review1 = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Buffer overflow in parse_input".to_string(),
                description: "desc".to_string(),
                suggestion: "fix".to_string(),
                category: Category::BufferOverflow,
                finding_id: String::new(),
                reasoning: String::new(),
            }],
        };
        let mut review2 = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Possible buffer overrun detected".to_string(),
                description: "different description".to_string(),
                suggestion: "different suggestion".to_string(),
                category: Category::BufferOverflow,
                finding_id: String::new(),
                reasoning: String::new(),
            }],
        };
        assign_finding_ids(&mut review1);
        assign_finding_ids(&mut review2);
        assert_eq!(
            review1.findings[0].finding_id, review2.findings[0].finding_id,
            "Findings with different titles but same file:line:category must produce the same ID"
        );
    }

    #[test]
    fn assign_ids_overwrites_preexisting_ids() {
        let mut review = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Finding".to_string(),
                description: "Desc".to_string(),
                suggestion: "Fix".to_string(),
                category: Category::BufferOverflow,
                finding_id: "old-id-value".to_string(),
                reasoning: String::new(),
            }],
        };
        assign_finding_ids(&mut review);
        assert_ne!(
            review.findings[0].finding_id, "old-id-value",
            "Pre-existing ID should be overwritten"
        );
        assert_eq!(review.findings[0].finding_id.len(), FINDING_ID_HEX_LENGTH);
    }

    // --- Phase 1 (G2): Chain-of-thought reasoning tests ---

    #[test]
    fn finding_id_excludes_reasoning() {
        let mut review1 = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Same title".to_string(),
                description: "Same desc".to_string(),
                suggestion: "Same fix".to_string(),
                category: Category::LogicError,
                finding_id: String::new(),
                reasoning: "Short reasoning".to_string(),
            }],
        };
        let mut review2 = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "src/main.rs".to_string(),
                line: 42,
                title: "Same title".to_string(),
                description: "Same desc".to_string(),
                suggestion: "Same fix".to_string(),
                category: Category::LogicError,
                finding_id: String::new(),
                reasoning: "Completely different and much longer reasoning text".to_string(),
            }],
        };
        assign_finding_ids(&mut review1);
        assign_finding_ids(&mut review2);
        assert_eq!(
            review1.findings[0].finding_id, review2.findings[0].finding_id,
            "Findings with different reasoning but same file:line:category must produce the same ID"
        );
    }
}
