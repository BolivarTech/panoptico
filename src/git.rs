// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Git diff extraction — runs git commands to get PR diffs.

use std::collections::HashMap;
use std::process::Command;

use crate::error::ReviewError;

/// Extracted git diff data mapping file paths to their diff content.
#[derive(Debug)]
pub struct GitDiff {
    /// Map of file path → unified diff content.
    pub files: HashMap<String, String>,
}

impl GitDiff {
    /// Extract diff between `base_ref` and `target_ref`.
    ///
    /// # Arguments
    ///
    /// * `base_ref` - Git reference to diff against (e.g., `origin/main`).
    /// * `target_ref` - Git reference to diff towards (e.g., `HEAD`, a SHA, or a tag).
    /// * `extensions` - Glob patterns for file filtering (e.g., `["*.c", "*.h"]`).
    ///   Empty slice or `["*"]` includes all files.
    ///
    /// # Errors
    ///
    /// Returns `ReviewError::GitDiff` if git commands fail.
    pub fn extract(
        base_ref: &str,
        target_ref: &str,
        extensions: &[String],
    ) -> Result<Self, ReviewError> {
        validate_ref("base_ref", base_ref)?;
        validate_ref("target_ref", target_ref)?;

        let range = format!("{}...{}", base_ref, target_ref);
        let output = Command::new("git")
            .args(["diff", "--name-only", &range])
            .output()
            .map_err(|e| ReviewError::GitDiff(e.to_string()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ReviewError::GitDiff(format!(
                "git diff --name-only failed: {}",
                stderr
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let all_files: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

        let filtered = filter_by_extensions(&all_files, extensions);

        let mut files = HashMap::new();
        for file in &filtered {
            let diff = get_file_diff(base_ref, target_ref, file)?;
            if !diff.is_empty() {
                files.insert(file.clone(), diff);
            }
        }

        Ok(Self { files })
    }
}

/// Filter file paths by glob extension patterns.
///
/// Returns all files if `extensions` is empty or contains `"*"`.
///
/// # Arguments
///
/// * `files` - Slice of file paths to filter.
/// * `extensions` - Glob patterns (e.g., `["*.c", "*.h"]`).
///
/// # Returns
///
/// Filtered file paths matching at least one extension pattern.
pub fn filter_by_extensions(files: &[&str], extensions: &[String]) -> Vec<String> {
    if extensions.is_empty() || extensions.iter().any(|e| e == "*") {
        return files.iter().map(|f| f.to_string()).collect();
    }
    files
        .iter()
        .filter(|f| {
            extensions.iter().any(|ext| {
                let suffix = ext.trim_start_matches('*');
                f.ends_with(suffix)
            })
        })
        .map(|f| f.to_string())
        .collect()
}

/// Validate that a git reference does not contain argument-injection patterns.
///
/// Rejects references starting with `-` (could be interpreted as git flags)
/// and references containing shell metacharacters.
///
/// # Arguments
///
/// * `label` - Descriptive name for error messages (e.g., `"base_ref"`, `"target_ref"`).
/// * `git_ref` - The git reference string to validate.
///
/// # Errors
///
/// Returns [`ReviewError::Config`] if the reference is invalid.
fn validate_ref(label: &str, git_ref: &str) -> Result<(), ReviewError> {
    if git_ref.is_empty() {
        return Err(ReviewError::Config(format!("{} must not be empty", label)));
    }
    if git_ref.starts_with('-') {
        return Err(ReviewError::Config(format!(
            "{} '{}' must not start with '-'",
            label, git_ref
        )));
    }
    if git_ref.contains([';', '|', '&', '$', '`', '\n', '\r']) {
        return Err(ReviewError::Config(format!(
            "{} '{}' contains invalid characters",
            label, git_ref
        )));
    }
    Ok(())
}

/// Get the unified diff for a single file between `base_ref` and `target_ref`.
///
/// # Errors
///
/// Returns [`ReviewError::GitDiff`] if the git command fails.
fn get_file_diff(base_ref: &str, target_ref: &str, file: &str) -> Result<String, ReviewError> {
    let range = format!("{}...{}", base_ref, target_ref);
    let output = Command::new("git")
        .args(["diff", &range, "--", file])
        .output()
        .map_err(|e| ReviewError::GitDiff(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ReviewError::GitDiff(format!(
            "git diff failed for {}: {}",
            file, stderr
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- filter_by_extensions ---

    #[test]
    fn filter_empty_extensions_includes_all() {
        let files = vec!["main.c", "utils.h", "README.md", "Makefile"];
        let result = filter_by_extensions(&files, &[]);
        assert_eq!(result.len(), 4, "Empty extensions should include all files");
    }

    #[test]
    fn filter_wildcard_includes_all() {
        let files = vec!["main.c", "utils.h", "README.md"];
        let extensions = vec!["*".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert_eq!(result.len(), 3, "Wildcard '*' should include all files");
    }

    #[test]
    fn filter_single_extension() {
        let files = vec!["main.c", "utils.h", "test.c", "README.md"];
        let extensions = vec!["*.c".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert_eq!(result.len(), 2, "*.c should match 2 files");
        assert!(result.contains(&"main.c".to_string()));
        assert!(result.contains(&"test.c".to_string()));
    }

    #[test]
    fn filter_multiple_extensions() {
        let files = vec!["main.c", "utils.h", "test.py", "README.md"];
        let extensions = vec!["*.c".to_string(), "*.h".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert_eq!(result.len(), 2, "*.c and *.h should match 2 files");
        assert!(result.contains(&"main.c".to_string()));
        assert!(result.contains(&"utils.h".to_string()));
    }

    #[test]
    fn filter_no_matches_returns_empty() {
        let files = vec!["main.c", "utils.h"];
        let extensions = vec!["*.rs".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert!(result.is_empty(), "No matches should return empty");
    }

    #[test]
    fn filter_empty_file_list_returns_empty() {
        let files: Vec<&str> = vec![];
        let extensions = vec!["*.c".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert!(result.is_empty(), "Empty file list should return empty");
    }

    #[test]
    fn filter_nested_paths_match_extension() {
        let files = vec![
            "src/main.c",
            "src/lib/utils.h",
            "docs/README.md",
            "tests/test_main.c",
        ];
        let extensions = vec!["*.c".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert_eq!(result.len(), 2, "*.c should match files in nested paths");
        assert!(result.contains(&"src/main.c".to_string()));
        assert!(result.contains(&"tests/test_main.c".to_string()));
    }

    #[test]
    fn filter_case_sensitive() {
        let files = vec!["main.C", "main.c", "main.CPP"];
        let extensions = vec!["*.c".to_string()];
        let result = filter_by_extensions(&files, &extensions);
        assert_eq!(
            result.len(),
            1,
            "Extension matching should be case-sensitive"
        );
        assert!(result.contains(&"main.c".to_string()));
    }

    // --- validate_ref ---

    #[test]
    fn validate_ref_accepts_normal_ref() {
        assert!(validate_ref("base_ref", "origin/main").is_ok());
    }

    #[test]
    fn validate_ref_accepts_commit_hash() {
        assert!(validate_ref("base_ref", "abc123def").is_ok());
    }

    #[test]
    fn validate_ref_rejects_empty() {
        assert!(validate_ref("base_ref", "").is_err());
    }

    #[test]
    fn validate_ref_rejects_leading_dash() {
        let result = validate_ref("base_ref", "--help");
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("must not start with '-'"));
    }

    #[test]
    fn validate_ref_rejects_shell_metacharacters() {
        assert!(validate_ref("base_ref", "origin/main; rm -rf /").is_err());
        assert!(validate_ref("base_ref", "origin/main|cat").is_err());
        assert!(validate_ref("base_ref", "ref&background").is_err());
        assert!(validate_ref("base_ref", "$VAR").is_err());
        assert!(validate_ref("base_ref", "`cmd`").is_err());
    }

    #[test]
    fn validate_ref_label_in_empty_error() {
        let result = validate_ref("target_ref", "");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("target_ref"),
            "Error should include the label 'target_ref': {}",
            msg
        );
    }

    #[test]
    fn validate_ref_label_in_dash_error() {
        let result = validate_ref("target_ref", "--exec");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("target_ref"),
            "Error should include the label 'target_ref': {}",
            msg
        );
    }

    #[test]
    fn validate_ref_label_in_metachar_error() {
        let result = validate_ref("target_ref", "ref;evil");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(
            msg.contains("target_ref"),
            "Error should include the label 'target_ref': {}",
            msg
        );
    }

    // --- GitDiff struct ---

    #[test]
    fn git_diff_files_is_hashmap() {
        let diff = GitDiff {
            files: HashMap::new(),
        };
        assert!(
            diff.files.is_empty(),
            "New GitDiff should have empty files map"
        );
    }
}
