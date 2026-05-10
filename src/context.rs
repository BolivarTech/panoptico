// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-18

//! File context enrichment for review prompts.
//!
//! Provides structured metadata about source files (language, test
//! coverage, framework hints) that the LLM can use to calibrate
//! severity and reduce false positives.

use serde::Serialize;

/// Structured metadata about a source file for prompt enrichment.
///
/// Provides context the model can use to calibrate severity and
/// relevance of findings. Built from diff metadata and filesystem
/// probes before the map phase.
///
/// # Examples
///
/// ```
/// use panoptico::context::{FileContext, build_file_context};
///
/// let ctx = build_file_context("src/main.rs", "+fn main() {}");
/// assert_eq!(ctx.language, "rust");
/// assert!(ctx.has_test_file); // Rust files always true (inline #[cfg(test)])
/// ```
#[derive(Debug, Clone, Default, Serialize)]
pub struct FileContext {
    /// Programming language (e.g., "rust", "c", "python", "unknown").
    pub language: String,
    /// Whether the file is entirely new in this PR.
    pub is_new_file: bool,
    /// Whether a corresponding test file likely exists.
    pub has_test_file: bool,
    /// Number of added/modified lines in this file.
    pub lines_changed: u32,
    /// Detected framework/library hints from imports in the diff.
    pub framework_hints: Vec<String>,
}

/// Detect programming language from file extension.
///
/// # Arguments
///
/// * `file_path` - Path to the source file (e.g., "src/main.rs").
///
/// # Returns
///
/// Language identifier string. Returns "unknown" for unrecognized extensions.
pub fn detect_language(file_path: &str) -> String {
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "c" | "h" => "c",
        "cpp" | "cxx" | "cc" | "hpp" | "hxx" => "cpp",
        "py" | "pyi" => "python",
        "js" | "jsx" | "mjs" => "javascript",
        "ts" | "tsx" | "mts" => "typescript",
        "go" => "go",
        "java" => "java",
        "cs" => "csharp",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "sh" | "bash" => "shell",
        "ps1" => "powershell",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" | "markdown" => "markdown",
        "xml" => "xml",
        "html" | "htm" => "html",
        "css" | "scss" | "sass" => "css",
        "sql" => "sql",
        _ => "unknown",
    }
    .to_string()
}

/// Check if a file is new by scanning its diff content for the
/// "new file mode" header that git produces for added files.
///
/// # Arguments
///
/// * `diff_content` - Raw unified diff content for one file.
pub fn is_new_file(diff_content: &str) -> bool {
    diff_content
        .lines()
        .any(|line| line.starts_with("new file mode"))
}

/// Heuristic check for a corresponding test file on the filesystem.
///
/// Returns `true` for Rust `.rs` files (inline `#[cfg(test)]` is standard),
/// and probes common naming conventions for other languages.
///
/// # Known Limitations
///
/// - For Rust, always returns `true` — assumes inline `#[cfg(test)]`
///   modules. Files without test modules will be incorrectly reported
///   as having tests.
/// - Relies on `Path::exists()`, which requires the file to be present
///   in the working tree. In CI environments with shallow clones or
///   sparse checkouts, test files may not be on disk, causing false
///   negatives. This is acceptable: `has_test_file: false` simply
///   means the model gets slightly less context.
pub fn probe_test_file(file_path: &str) -> bool {
    let path = std::path::Path::new(file_path);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let parent = path.parent().unwrap_or(std::path::Path::new(""));

    // Rust: inline #[cfg(test)] is idiomatic — always true
    if ext == "rs" {
        return true;
    }

    let candidates: Vec<std::path::PathBuf> = match ext {
        "py" => vec![
            parent.join(format!("test_{stem}.py")),
            parent.join(format!("{stem}_test.py")),
            parent.join("tests").join(format!("test_{stem}.py")),
        ],
        "js" | "jsx" | "mjs" => vec![
            parent.join(format!("{stem}.test.js")),
            parent.join(format!("{stem}.spec.js")),
            parent.join("__tests__").join(format!("{stem}.js")),
        ],
        "ts" | "tsx" | "mts" => vec![
            parent.join(format!("{stem}.test.ts")),
            parent.join(format!("{stem}.spec.ts")),
            parent.join("__tests__").join(format!("{stem}.ts")),
        ],
        "c" | "h" => vec![
            parent.join(format!("test_{stem}.c")),
            std::path::PathBuf::from("tests").join(format!("{stem}_test.c")),
        ],
        "cpp" | "cxx" | "cc" => vec![
            parent.join(format!("{stem}_test.cpp")),
            std::path::PathBuf::from("tests").join(format!("{stem}_test.cpp")),
        ],
        "go" => vec![parent.join(format!("{stem}_test.go"))],
        "java" => vec![
            parent.join(format!("{stem}Test.java")),
            parent.join("test").join(format!("{stem}Test.java")),
        ],
        _ => vec![],
    };

    candidates.iter().any(|c| c.exists())
}

/// Detect framework/library hints from import patterns in diff content.
///
/// # Arguments
///
/// * `language` - Language identifier from `detect_language()`.
/// * `diff_content` - Raw unified diff content.
///
/// # Returns
///
/// A list of detected framework/library names (may be empty).
pub fn detect_frameworks(language: &str, diff_content: &str) -> Vec<String> {
    let mut hints = Vec::new();
    let patterns: &[(&str, &str)] = match language {
        "rust" => &[
            ("tokio::", "tokio"),
            ("async_trait", "async-trait"),
            ("serde::", "serde"),
            ("#![no_std]", "no_std"),
            ("actix_web", "actix"),
            ("axum::", "axum"),
            ("clap::", "clap"),
            ("tracing::", "tracing"),
            ("reqwest::", "reqwest"),
            ("sqlx::", "sqlx"),
        ],
        "python" => &[
            ("import flask", "flask"),
            ("import django", "django"),
            ("import fastapi", "fastapi"),
            ("import sqlalchemy", "sqlalchemy"),
            ("import pytest", "pytest"),
            ("import numpy", "numpy"),
            ("import pandas", "pandas"),
            ("import torch", "pytorch"),
        ],
        "javascript" | "typescript" => &[
            ("from 'react", "react"),
            ("from \"react", "react"),
            ("from 'next", "nextjs"),
            ("from 'express", "express"),
            ("from 'vue", "vue"),
            ("from '@angular", "angular"),
        ],
        "go" => &[
            ("\"net/http\"", "net/http"),
            ("\"github.com/gin", "gin"),
            ("\"github.com/gorilla", "gorilla"),
        ],
        _ => &[],
    };
    for (pattern, hint) in patterns {
        if diff_content.contains(pattern) {
            hints.push(hint.to_string());
        }
    }
    hints
}

/// Build context metadata for a single file from its diff content.
///
/// # Arguments
///
/// * `file_path` - Path to the source file.
/// * `diff_content` - Raw unified diff content for this file.
pub fn build_file_context(file_path: &str, diff_content: &str) -> FileContext {
    let language = detect_language(file_path);
    let lines_changed = u32::try_from(
        diff_content
            .lines()
            .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
            .count(),
    )
    .unwrap_or(u32::MAX);
    let framework_hints = detect_frameworks(&language, diff_content);

    FileContext {
        language,
        is_new_file: is_new_file(diff_content),
        has_test_file: probe_test_file(file_path),
        lines_changed,
        framework_hints,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_rust() {
        assert_eq!(detect_language("src/main.rs"), "rust");
    }

    #[test]
    fn detect_language_python() {
        assert_eq!(detect_language("app.py"), "python");
    }

    #[test]
    fn detect_language_unknown() {
        assert_eq!(detect_language("Makefile"), "unknown");
    }

    #[test]
    fn is_new_file_detects_new() {
        let diff = "diff --git a/src/new.rs b/src/new.rs\n\
                     new file mode 100644\n\
                     --- /dev/null\n\
                     +++ b/src/new.rs\n\
                     @@ -0,0 +1,5 @@\n\
                     +fn hello() {}";
        assert!(is_new_file(diff), "Should detect 'new file mode' in diff");
    }

    #[test]
    fn is_new_file_false_for_existing() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
                     --- a/src/main.rs\n\
                     +++ b/src/main.rs\n\
                     @@ -1,3 +1,4 @@\n\
                     +use std::io;";
        assert!(
            !is_new_file(diff),
            "Normal diff should not be detected as new file"
        );
    }

    #[test]
    fn probe_test_file_true_for_rust() {
        assert!(
            probe_test_file("src/main.rs"),
            "Rust files should always return true (inline #[cfg(test)])"
        );
    }

    #[test]
    fn detect_frameworks_rust_tokio() {
        let diff = "+use tokio::runtime::Runtime;";
        let hints = detect_frameworks("rust", diff);
        assert!(
            hints.contains(&"tokio".to_string()),
            "Should detect tokio framework: {:?}",
            hints
        );
    }

    #[test]
    fn detect_frameworks_empty_for_plain() {
        let diff = "+let x = 42;";
        let hints = detect_frameworks("rust", diff);
        assert!(
            hints.is_empty(),
            "Plain code with no imports should have no framework hints"
        );
    }

    #[test]
    fn build_file_context_assembles_all_fields() {
        let diff = "diff --git a/src/lib.rs b/src/lib.rs\n\
                     new file mode 100644\n\
                     --- /dev/null\n\
                     +++ b/src/lib.rs\n\
                     @@ -0,0 +1,3 @@\n\
                     +use tokio::runtime::Runtime;\n\
                     +use serde::Serialize;\n\
                     +fn main() {}";
        let ctx = build_file_context("src/lib.rs", diff);
        assert_eq!(ctx.language, "rust", "Language should be rust");
        assert!(ctx.is_new_file, "Should detect new file");
        assert!(ctx.has_test_file, "Rust files should have test file = true");
        assert_eq!(ctx.lines_changed, 3, "Should count 3 added lines");
        assert!(
            ctx.framework_hints.contains(&"tokio".to_string()),
            "Should detect tokio: {:?}",
            ctx.framework_hints
        );
        assert!(
            ctx.framework_hints.contains(&"serde".to_string()),
            "Should detect serde: {:?}",
            ctx.framework_hints
        );
    }
}
