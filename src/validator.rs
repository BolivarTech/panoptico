// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Hallucination guard — validates findings against actual diff files.
//!
//! Removes [`Finding`](crate::backend::Finding) entries that reference
//! files not present in the original diff, preventing false positives
//! from LLM hallucinations.

use std::collections::HashSet;

use crate::backend::CodeReview;

/// Remove findings that reference files not in the diff.
///
/// When findings are removed, a note is appended to the summary
/// indicating how many were filtered (e.g.
/// `[Validation: 2 finding(s) removed — referenced files not in the diff]`).
///
/// # Arguments
///
/// * `review` - The code review to validate.
/// * `valid_files` - Set of file paths actually present in the diff.
///
/// # Returns
///
/// A cleaned `CodeReview` with hallucinated findings removed and
/// summary reconciled to reflect the filtered count.
pub fn validate_findings(review: CodeReview, valid_files: &HashSet<String>) -> CodeReview {
    let original_count = review.findings.len();
    let findings: Vec<_> = review
        .findings
        .into_iter()
        .filter(|f| valid_files.contains(&f.file))
        .collect();
    let removed = original_count - findings.len();
    let summary = if removed > 0 {
        format!(
            "{}\n\n[Validation: {} finding(s) removed — referenced files not in the diff]",
            review.summary, removed
        )
    } else {
        review.summary
    };
    CodeReview { summary, findings }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::mock::make_test_finding;
    use crate::backend::{CodeReview, Finding, Severity};
    use crate::finding_id::Category;

    fn make_finding(file: &str, title: &str) -> Finding {
        make_test_finding(file, 1, title)
    }

    fn make_valid_files(files: &[&str]) -> HashSet<String> {
        files.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn keeps_valid_findings() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![
                make_finding("main.c", "Buffer overflow"),
                make_finding("utils.h", "Missing include guard"),
            ],
        };
        let valid = make_valid_files(&["main.c", "utils.h"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings.len(),
            2,
            "All valid findings should be kept"
        );
    }

    #[test]
    fn removes_hallucinated_findings() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![
                make_finding("main.c", "Real finding"),
                make_finding("nonexistent.c", "Hallucinated finding"),
            ],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings.len(),
            1,
            "Hallucinated finding should be removed"
        );
        assert_eq!(result.findings[0].file, "main.c");
    }

    #[test]
    fn removes_all_when_all_hallucinated() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![
                make_finding("fake1.c", "Fake 1"),
                make_finding("fake2.c", "Fake 2"),
            ],
        };
        let valid = make_valid_files(&["real.c"]);
        let result = validate_findings(review, &valid);
        assert!(
            result.findings.is_empty(),
            "All hallucinated findings should be removed"
        );
    }

    #[test]
    fn preserves_summary_when_no_findings_removed() {
        let review = CodeReview {
            summary: "Important review summary".to_string(),
            findings: vec![make_finding("main.c", "Valid finding")],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.summary, "Important review summary",
            "Summary should be unchanged when 0 findings removed"
        );
    }

    #[test]
    fn empty_findings_returns_empty() {
        let review = CodeReview {
            summary: "No findings".to_string(),
            findings: vec![],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert!(
            result.findings.is_empty(),
            "Empty findings should remain empty"
        );
    }

    #[test]
    fn empty_valid_files_removes_all() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![make_finding("main.c", "Finding 1")],
        };
        let valid: HashSet<String> = HashSet::new();
        let result = validate_findings(review, &valid);
        assert!(
            result.findings.is_empty(),
            "Empty valid_files set should cause all findings to be removed"
        );
    }

    #[test]
    fn file_matching_is_exact() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![
                make_finding("src/main.c", "With path prefix"),
                make_finding("main.c", "Without path prefix"),
            ],
        };
        let valid = make_valid_files(&["src/main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings.len(),
            1,
            "File matching should be exact path comparison"
        );
        assert_eq!(result.findings[0].file, "src/main.c");
    }

    #[test]
    fn multiple_findings_same_file_all_kept() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![
                make_finding("main.c", "Finding A"),
                make_finding("main.c", "Finding B"),
                make_finding("main.c", "Finding C"),
            ],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings.len(),
            3,
            "Multiple findings for the same valid file should all be kept"
        );
    }

    #[test]
    fn validate_findings_preserves_finding_id() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "main.c".to_string(),
                line: 42,
                title: "Finding".to_string(),
                description: "Desc".to_string(),
                suggestion: "Fix".to_string(),
                category: Category::BufferOverflow,
                finding_id: "a1b2c3d4e5f67890".to_string(),
                reasoning: String::new(),
            }],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings[0].finding_id, "a1b2c3d4e5f67890",
            "Validation should preserve finding_id"
        );
    }

    #[test]
    fn appends_note_when_findings_removed() {
        let review = CodeReview {
            summary: "Found 2 issues".to_string(),
            findings: vec![
                make_finding("main.c", "Real finding"),
                make_finding("nonexistent.c", "Hallucinated finding"),
            ],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert!(
            result.summary.contains("[Validation:"),
            "Summary should contain validation note when findings removed: {}",
            result.summary
        );
        assert!(
            result.summary.starts_with("Found 2 issues"),
            "Original summary should be preserved at the start: {}",
            result.summary
        );
    }

    #[test]
    fn appends_note_with_correct_count() {
        let review = CodeReview {
            summary: "Review complete".to_string(),
            findings: vec![
                make_finding("main.c", "Valid"),
                make_finding("fake1.c", "Hallucinated 1"),
                make_finding("fake2.c", "Hallucinated 2"),
                make_finding("fake3.c", "Hallucinated 3"),
            ],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert!(
            result.summary.contains("3 finding(s) removed"),
            "Note should report exactly 3 removed findings: {}",
            result.summary
        );
    }

    #[test]
    fn no_note_when_all_findings_valid() {
        let review = CodeReview {
            summary: "Clean review".to_string(),
            findings: vec![
                make_finding("main.c", "Finding A"),
                make_finding("utils.h", "Finding B"),
            ],
        };
        let valid = make_valid_files(&["main.c", "utils.h"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.summary, "Clean review",
            "Summary should have no note when all findings are valid"
        );
    }

    #[test]
    fn appends_note_when_all_hallucinated() {
        let review = CodeReview {
            summary: "Found 2 critical issues".to_string(),
            findings: vec![
                make_finding("fake1.c", "Fake 1"),
                make_finding("fake2.c", "Fake 2"),
            ],
        };
        let valid = make_valid_files(&["real.c"]);
        let result = validate_findings(review, &valid);
        assert!(result.findings.is_empty(), "All findings should be removed");
        assert!(
            result.summary.contains("2 finding(s) removed"),
            "Note should report 2 removed: {}",
            result.summary
        );
        assert!(
            result.summary.starts_with("Found 2 critical issues"),
            "Original summary should be preserved: {}",
            result.summary
        );
    }

    #[test]
    fn validate_findings_preserves_category() {
        let review = CodeReview {
            summary: "test".to_string(),
            findings: vec![Finding {
                severity: Severity::Warning,
                file: "main.c".to_string(),
                line: 42,
                title: "Finding".to_string(),
                description: "Desc".to_string(),
                suggestion: "Fix".to_string(),
                category: Category::RaceCondition,
                finding_id: String::new(),
                reasoning: String::new(),
            }],
        };
        let valid = make_valid_files(&["main.c"]);
        let result = validate_findings(review, &valid);
        assert_eq!(
            result.findings[0].category,
            Category::RaceCondition,
            "Validation should preserve category"
        );
    }
}
