// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-09

//! Batch grouping — groups hunks into batches respecting a line limit.
//!
//! Groups [`Hunk`] values into [`Batch`] collections using a greedy
//! algorithm that respects a configurable line limit per batch.

use crate::hunk::Hunk;

/// Default maximum lines per batch.
pub const DEFAULT_MAX_LINES: usize = 500;

/// A batch of hunks grouped for a single review request.
#[derive(Debug, Clone)]
pub struct Batch {
    /// Hunks included in this batch.
    pub hunks: Vec<Hunk>,
    /// Total line count across all hunks.
    pub total_lines: usize,
}

impl Batch {
    /// Concatenate all hunk contents separated by blank lines.
    pub fn content(&self) -> String {
        self.hunks
            .iter()
            .map(|h| h.content.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Return deduplicated list of file paths in this batch.
    pub fn file_list(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        self.hunks
            .iter()
            .filter_map(|h| {
                if seen.insert(h.file.as_str()) {
                    Some(h.file.as_str())
                } else {
                    None
                }
            })
            .collect()
    }
}

/// Group hunks into batches using a greedy algorithm.
///
/// # Rules
///
/// 1. A single hunk exceeding `max_lines` goes into its own batch (with warning).
/// 2. Hunks are added greedily until the next would exceed the limit.
/// 3. No code is ever dropped.
///
/// # Arguments
///
/// * `hunks` - All parsed hunks from the diff.
/// * `max_lines` - Maximum total lines per batch.
///
/// # Returns
///
/// A vector of `Batch` values covering all input hunks.
pub fn group_into_batches(hunks: Vec<Hunk>, max_lines: usize) -> Vec<Batch> {
    let mut batches = Vec::new();
    let mut current_hunks = Vec::new();
    let mut current_lines = 0;

    for hunk in hunks {
        if hunk.lines > max_lines {
            if !current_hunks.is_empty() {
                batches.push(Batch {
                    hunks: current_hunks,
                    total_lines: current_lines,
                });
                current_hunks = Vec::new();
                current_lines = 0;
            }
            batches.push(Batch {
                total_lines: hunk.lines,
                hunks: vec![hunk],
            });
            continue;
        }
        if current_lines + hunk.lines > max_lines && !current_hunks.is_empty() {
            batches.push(Batch {
                hunks: current_hunks,
                total_lines: current_lines,
            });
            current_hunks = Vec::new();
            current_lines = 0;
        }
        current_lines += hunk.lines;
        current_hunks.push(hunk);
    }
    if !current_hunks.is_empty() {
        batches.push(Batch {
            hunks: current_hunks,
            total_lines: current_lines,
        });
    }
    batches
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::hunk::Hunk;

    /// Helper to create a hunk with a given line count.
    fn make_hunk(file: &str, lines: usize) -> Hunk {
        let content = (0..lines)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        Hunk {
            file: file.to_string(),
            content,
            lines,
        }
    }

    // --- group_into_batches ---

    #[test]
    fn empty_input_returns_empty_output() {
        let batches = group_into_batches(vec![], 500);
        assert!(batches.is_empty(), "No hunks should produce no batches");
    }

    #[test]
    fn single_small_hunk_returns_one_batch() {
        let hunks = vec![make_hunk("a.c", 10)];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(batches.len(), 1, "One small hunk should produce 1 batch");
        assert_eq!(batches[0].hunks.len(), 1);
        assert_eq!(batches[0].total_lines, 10);
    }

    #[test]
    fn multiple_small_hunks_fit_one_batch() {
        let hunks = vec![
            make_hunk("a.c", 100),
            make_hunk("b.c", 100),
            make_hunk("c.c", 100),
        ];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(
            batches.len(),
            1,
            "Three hunks totaling 300 lines should fit in 1 batch (limit 500)"
        );
        assert_eq!(batches[0].hunks.len(), 3);
        assert_eq!(batches[0].total_lines, 300);
    }

    #[test]
    fn hunks_exceeding_limit_split_into_multiple_batches() {
        let hunks = vec![make_hunk("a.c", 300), make_hunk("b.c", 300)];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(
            batches.len(),
            2,
            "Two hunks of 300 lines should split into 2 batches (limit 500)"
        );
        assert_eq!(batches[0].total_lines, 300);
        assert_eq!(batches[1].total_lines, 300);
    }

    #[test]
    fn oversized_hunk_goes_solo() {
        let hunks = vec![
            make_hunk("small.c", 50),
            make_hunk("huge.c", 600),
            make_hunk("tiny.c", 30),
        ];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(
            batches.len(),
            3,
            "Oversized hunk should be isolated in its own batch"
        );
        assert_eq!(batches[0].total_lines, 50, "First batch: small hunk");
        assert_eq!(
            batches[1].total_lines, 600,
            "Second batch: oversized hunk alone"
        );
        assert_eq!(batches[2].total_lines, 30, "Third batch: tiny hunk");
    }

    #[test]
    fn exact_limit_fits_in_one_batch() {
        let hunks = vec![make_hunk("a.c", 250), make_hunk("b.c", 250)];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(
            batches.len(),
            1,
            "Two hunks totaling exactly 500 should fit in 1 batch"
        );
        assert_eq!(batches[0].total_lines, 500);
    }

    #[test]
    fn one_line_over_limit_forces_new_batch() {
        let hunks = vec![make_hunk("a.c", 250), make_hunk("b.c", 251)];
        let batches = group_into_batches(hunks, 500);
        assert_eq!(
            batches.len(),
            2,
            "Two hunks totaling 501 should split into 2 batches"
        );
    }

    #[test]
    fn greedy_packing_fills_batches() {
        // 5 hunks of 200 lines each = 1000 total, limit 500
        let hunks: Vec<_> = (0..5)
            .map(|i| make_hunk(&format!("file{}.c", i), 200))
            .collect();
        let batches = group_into_batches(hunks, 500);
        assert_eq!(batches.len(), 3, "5x200 with limit 500 → 3 batches (2+2+1)");
        assert_eq!(batches[0].total_lines, 400);
        assert_eq!(batches[1].total_lines, 400);
        assert_eq!(batches[2].total_lines, 200);
    }

    // --- Batch::content ---

    #[test]
    fn batch_content_joins_hunks_with_blank_lines() {
        let batch = Batch {
            hunks: vec![make_hunk("a.c", 2), make_hunk("b.c", 2)],
            total_lines: 4,
        };
        let content = batch.content();
        assert!(
            content.contains("\n\n"),
            "Batch content should separate hunks with blank lines"
        );
    }

    // --- Batch::file_list ---

    #[test]
    fn file_list_returns_unique_files() {
        let batch = Batch {
            hunks: vec![
                make_hunk("a.c", 10),
                make_hunk("a.c", 20),
                make_hunk("b.c", 15),
            ],
            total_lines: 45,
        };
        let files = batch.file_list();
        let unique: HashSet<&str> = files.iter().copied().collect();
        assert_eq!(
            unique.len(),
            2,
            "file_list should return deduplicated file paths"
        );
        assert!(unique.contains("a.c"));
        assert!(unique.contains("b.c"));
    }

    #[test]
    fn file_list_empty_batch_returns_empty() {
        let batch = Batch {
            hunks: vec![],
            total_lines: 0,
        };
        let files = batch.file_list();
        assert!(files.is_empty(), "Empty batch should have empty file list");
    }
}
