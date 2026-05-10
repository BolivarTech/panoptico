// Author: Julian Bolivar
// Version: 0.0.1
// Date: 2026-02-12

//! Exhaustive tests for core types, registry, and utilities — TDD Red Phase.
//!
//! All tests describe correct expected behavior that the current
//! implementation fails to provide.

use std::collections::HashSet;

use super::*;

// ============================================================================
// LanguageRegistry
// ============================================================================

/// `.h` files are ambiguous between C and C++.
/// The registry should resolve `.h` to C (the more conservative default),
/// but the current implementation maps to C++ (last-registered wins).
#[test]
fn registry_h_extension_resolves_to_c() {
    let registry = LanguageRegistry::new();
    let extractor = registry.get_extractor("driver.h");
    assert_eq!(
        extractor.language_id(),
        "c",
        ".h should resolve to C, not C++"
    );
}

// ============================================================================
// extract_uncovered — clustering
// ============================================================================

/// When uncovered changed lines are far apart, the default `extract_uncovered`
/// should produce separate `SemanticUnit`s per cluster instead of one giant
/// blob spanning hundreds of lines.
#[test]
fn extract_uncovered_distant_lines_produce_separate_units() {
    let content = (1..=200)
        .map(|i| format!("line {}", i))
        .collect::<Vec<_>>()
        .join("\n");

    let mut changed = HashSet::new();
    changed.insert(5);
    changed.insert(195);

    // No covered ranges — everything is uncovered.
    let fallback = FallbackExtractor::new();
    let units = fallback.extract_units(&content, "big.txt", &changed);

    assert!(
        units.len() >= 2,
        "Distant changed lines (5 and 195) should produce at least 2 separate \
         context units, got {} unit(s) spanning lines {}-{}",
        units.len(),
        units.first().map(|u| u.start_line).unwrap_or(0),
        units.last().map(|u| u.end_line).unwrap_or(0),
    );
}
