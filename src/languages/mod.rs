// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-18

//! Language-aware semantic extraction for source code.
//!
//! This module provides extractors that recognize syntactic boundaries
//! (functions, classes, structs, declaration groups) in source code
//! and return the units that contain changed lines. Used by the
//! `--semantic` review mode to send complete code units to the LLM.
//!
//! # Architecture
//!
//! - [`LanguageExtractor`] trait defines the extraction interface.
//! - [`ExtractorKind`] enum provides zero-cost dispatch to concrete extractors.
//! - [`LanguageRegistry`] maps file extensions to extractors.
//! - [`group_semantic_batches`] packs units into token-limited batches.

pub mod brace_family;
pub mod config_files;
pub mod python;
pub mod scanner;

use std::collections::{HashMap, HashSet};

use crate::context::FileContext;

/// Approximate characters per token for size estimation.
const CHARS_PER_TOKEN: u32 = 4;

/// Default maximum tokens per semantic batch.
///
/// Approximately 16,000 characters of source code (at 4 chars/token).
/// Controls how much context the LLM receives per review request.
pub const DEFAULT_MAX_SEMANTIC_TOKENS: u32 = 4000;

/// Kind of semantic code unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitKind {
    /// A standalone function (Rust `fn`, C function, Python `def`, JS function).
    Function,
    /// A method inside a class or impl block.
    Method,
    /// A class definition (Python `class`, Java/C#/JS class).
    Class,
    /// A struct definition (Rust, C, Go).
    Struct,
    /// An enum definition.
    Enum,
    /// A trait or interface definition.
    Trait,
    /// An impl block (Rust-specific).
    Impl,
    /// A group of related top-level declarations: constants, statics,
    /// type aliases, imports, macros, global variables.
    DeclarationGroup,
    /// Fallback: changed lines not inside any recognized construct.
    /// Context is lines around the change.
    TopLevel,
}

/// A semantic unit of code extracted from a source file.
#[derive(Debug, Clone)]
pub struct SemanticUnit {
    /// Kind of code block.
    pub kind: UnitKind,
    /// Name of the construct (function name, struct name, or
    /// descriptive label like "imports" or "constants").
    pub name: String,
    /// Source file path.
    pub file: String,
    /// Start line (1-indexed, inclusive).
    pub start_line: u32,
    /// End line (1-indexed, inclusive).
    pub end_line: u32,
    /// Complete source code of the unit (including doc comments
    /// and attributes above the declaration).
    pub content: String,
    /// Lines within this unit that were modified in the PR.
    pub changed_lines: Vec<u32>,
    /// File context metadata for this unit's file.
    pub context: FileContext,
}

/// A batch of semantic units grouped by estimated token count.
#[derive(Debug)]
pub struct SemanticBatch {
    /// Units in this batch.
    pub units: Vec<SemanticUnit>,
    /// Estimated token count for this batch.
    pub estimated_tokens: u32,
}

/// Language-specific code extractor.
///
/// Implementors recognize syntactic boundaries (functions, classes,
/// declaration groups) in source code and return the units that
/// contain changed lines.
///
/// Concrete types: [`brace_family::BraceFamilyExtractor`],
/// [`python::PythonExtractor`], [`config_files::ConfigExtractor`].
/// Dispatched via [`ExtractorKind`] enum.
pub trait LanguageExtractor {
    /// Language identifier (e.g., "rust", "c", "python").
    fn language_id(&self) -> &str;
    /// File extensions this extractor handles (without dot).
    fn extensions(&self) -> &[&str];
    /// Extract semantic units from full file content, filtering
    /// to only those that overlap with `changed_lines`.
    fn extract_units(
        &self,
        content: &str,
        file_path: &str,
        changed_lines: &HashSet<u32>,
    ) -> Vec<SemanticUnit>;
}

/// Enum dispatch for language extractors.
///
/// Each variant wraps a concrete extractor. The enum delegates
/// [`LanguageExtractor`] methods to the inner type via match arms.
/// This avoids `Box<dyn>` and is trivially [`Clone`]-able.
#[derive(Clone)]
pub enum ExtractorKind {
    /// Brace-delimited languages (Rust, C, C++, Go, Java, JS, TS).
    BraceFamily(brace_family::BraceFamilyExtractor),
    /// Indentation-delimited (Python).
    Python(python::PythonExtractor),
    /// Section-based config files (TOML, YAML, JSON, Markdown).
    Config(config_files::ConfigExtractor),
}

impl LanguageExtractor for ExtractorKind {
    fn language_id(&self) -> &str {
        match self {
            Self::BraceFamily(e) => e.language_id(),
            Self::Python(e) => e.language_id(),
            Self::Config(e) => e.language_id(),
        }
    }

    fn extensions(&self) -> &[&str] {
        match self {
            // BraceFamilyExtractor stores extensions as Vec<String> (non-trait method).
            // The registry has already registered by extension, so this is unused at runtime.
            Self::BraceFamily(_) => &[],
            Self::Python(e) => e.extensions(),
            Self::Config(e) => e.extensions(),
        }
    }

    fn extract_units(
        &self,
        content: &str,
        file_path: &str,
        changed_lines: &HashSet<u32>,
    ) -> Vec<SemanticUnit> {
        match self {
            Self::BraceFamily(e) => e.extract_units(content, file_path, changed_lines),
            Self::Python(e) => e.extract_units(content, file_path, changed_lines),
            Self::Config(e) => e.extract_units(content, file_path, changed_lines),
        }
    }
}

/// Registry of language extractors, keyed by file extension.
///
/// # Examples
///
/// ```
/// use panoptico::languages::LanguageRegistry;
///
/// let registry = LanguageRegistry::new();
/// assert!(registry.get("src/main.rs").is_some());
/// assert!(registry.get("unknown.xyz").is_none());
/// ```
pub struct LanguageRegistry {
    extractors: HashMap<String, ExtractorKind>,
}

impl LanguageRegistry {
    /// Create a registry with all built-in extractors.
    pub fn new() -> Self {
        let mut registry = Self {
            extractors: HashMap::new(),
        };

        // Register brace-family extractors
        for extractor in brace_family::all_extractors() {
            for ext in extractor.extensions() {
                registry.extractors.insert(
                    ext.to_string(),
                    ExtractorKind::BraceFamily(extractor.clone()),
                );
            }
        }

        // Register Python
        let py = python::PythonExtractor;
        for ext in py.extensions() {
            registry
                .extractors
                .insert(ext.to_string(), ExtractorKind::Python(py));
        }

        // Register config files
        let cfg = config_files::ConfigExtractor;
        for ext in cfg.extensions() {
            registry
                .extractors
                .insert(ext.to_string(), ExtractorKind::Config(cfg));
        }

        registry
    }

    /// Get the extractor for a file path, or `None` for unknown extensions.
    pub fn get(&self, file_path: &str) -> Option<&ExtractorKind> {
        let ext = file_path.rsplit('.').next()?;
        self.extractors.get(ext)
    }
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Group semantic units into batches respecting a token limit.
///
/// Uses a greedy algorithm: add units to the current batch until
/// the limit is reached, then start a new batch. Oversized units
/// get their own batch with a warning.
///
/// # Arguments
///
/// * `units` - Semantic units to batch.
/// * `max_tokens` - Maximum estimated tokens per batch.
///
/// # Returns
///
/// A vector of [`SemanticBatch`] values ready for review requests.
pub fn group_semantic_batches(units: Vec<SemanticUnit>, max_tokens: u32) -> Vec<SemanticBatch> {
    let mut batches = Vec::new();
    let mut current_units = Vec::new();
    let mut current_tokens: u32 = 0;

    for unit in units {
        let unit_tokens = u32::try_from(unit.content.len()).unwrap_or(u32::MAX) / CHARS_PER_TOKEN;

        if unit_tokens > max_tokens {
            // Flush current batch if any
            if !current_units.is_empty() {
                batches.push(SemanticBatch {
                    units: std::mem::take(&mut current_units),
                    estimated_tokens: current_tokens,
                });
                current_tokens = 0;
            }
            // Oversized unit gets its own batch
            eprintln!(
                "Warning: oversized semantic unit '{}' ({} tokens > {} limit)",
                unit.name, unit_tokens, max_tokens
            );
            batches.push(SemanticBatch {
                estimated_tokens: unit_tokens,
                units: vec![unit],
            });
            continue;
        }

        if current_tokens + unit_tokens > max_tokens && !current_units.is_empty() {
            batches.push(SemanticBatch {
                units: std::mem::take(&mut current_units),
                estimated_tokens: current_tokens,
            });
            current_tokens = 0;
        }

        current_tokens += unit_tokens;
        current_units.push(unit);
    }

    if !current_units.is_empty() {
        batches.push(SemanticBatch {
            units: current_units,
            estimated_tokens: current_tokens,
        });
    }

    batches
}

/// Create a fallback semantic unit for changed lines in unknown file types.
///
/// Captures ±20 lines around each changed line as a `TopLevel` unit.
///
/// # Arguments
///
/// * `content` - Full file content.
/// * `file_path` - Path to the file.
/// * `changed_lines` - Set of changed line numbers.
/// * `context` - File context metadata.
pub fn fallback_extract(
    content: &str,
    file_path: &str,
    changed_lines: &HashSet<u32>,
    context: &FileContext,
) -> Vec<SemanticUnit> {
    if changed_lines.is_empty() {
        return vec![];
    }
    let lines: Vec<&str> = content.lines().collect();
    let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);

    let min_line = *changed_lines.iter().min().unwrap_or(&1);
    let max_line = *changed_lines.iter().max().unwrap_or(&total);

    let start = min_line.saturating_sub(20).max(1);
    let end = (max_line + 20).min(total);

    let start_idx = (start - 1) as usize;
    let end_idx = end as usize;
    let unit_content = lines
        .get(start_idx..end_idx.min(lines.len()))
        .unwrap_or(&[])
        .join("\n");

    let mut sorted_changed: Vec<u32> = changed_lines
        .iter()
        .copied()
        .filter(|&l| l >= start && l <= end)
        .collect();
    sorted_changed.sort_unstable();

    vec![SemanticUnit {
        kind: UnitKind::TopLevel,
        name: format!("lines {}-{}", start, end),
        file: file_path.to_string(),
        start_line: start,
        end_line: end,
        content: unit_content,
        changed_lines: sorted_changed,
        context: context.clone(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_unit(name: &str, content_len: usize) -> SemanticUnit {
        SemanticUnit {
            kind: UnitKind::Function,
            name: name.to_string(),
            file: "test.rs".to_string(),
            start_line: 1,
            end_line: 10,
            content: "x".repeat(content_len),
            changed_lines: vec![5],
            context: FileContext::default(),
        }
    }

    // --- Test 35: group_semantic_batches_respects_limit ---
    #[test]
    fn group_semantic_batches_respects_limit() {
        // Each unit is 400 chars = 100 tokens. Limit 150 tokens.
        let units = vec![make_unit("a", 400), make_unit("b", 400)];
        let batches = group_semantic_batches(units, 150);
        assert_eq!(batches.len(), 2, "Should split into 2 batches");
        assert_eq!(batches[0].units.len(), 1);
        assert_eq!(batches[1].units.len(), 1);
    }

    // --- Test 36: group_semantic_batches_oversized_unit_solo ---
    #[test]
    fn group_semantic_batches_oversized_unit_solo() {
        // One small (100 tokens), one oversized (2000 tokens), one small (100 tokens)
        let units = vec![
            make_unit("small1", 400),
            make_unit("big", 8000),
            make_unit("small2", 400),
        ];
        let batches = group_semantic_batches(units, 500);
        assert!(
            batches.len() >= 2,
            "Oversized unit should be in its own batch"
        );
        // Find the batch with the big unit
        let big_batch = batches
            .iter()
            .find(|b| b.units.iter().any(|u| u.name == "big"))
            .expect("Should find batch with big unit");
        assert_eq!(
            big_batch.units.len(),
            1,
            "Oversized unit should be alone in its batch"
        );
    }

    // --- Test 37: registry_dispatches_by_extension ---
    #[test]
    fn registry_dispatches_by_extension() {
        let registry = LanguageRegistry::new();
        let ext = registry.get("src/main.rs");
        assert!(ext.is_some(), ".rs should be registered");
        assert!(
            matches!(ext.unwrap(), ExtractorKind::BraceFamily(_)),
            ".rs should use BraceFamily extractor"
        );
    }

    // --- Test 38: registry_returns_none_for_unknown ---
    #[test]
    fn registry_returns_none_for_unknown() {
        let registry = LanguageRegistry::new();
        assert!(
            registry.get("data.xyz").is_none(),
            "Unknown extension should return None"
        );
    }
}
