// Author: Julian Bolivar
// Version: 1.1.0
// Date: 2026-02-18

//! Section-based extractor for configuration files (TOML, YAML, JSON, Markdown).
//!
//! Recognizes section boundaries in config files and emits
//! [`DeclarationGroup`](super::UnitKind::DeclarationGroup) units
//! for sections containing changed lines.

use std::collections::HashSet;

use super::{LanguageExtractor, SemanticUnit, UnitKind};
use crate::context::FileContext;

/// Section-based configuration file extractor.
///
/// Handles:
/// - **TOML**: `[section]` headers delimit blocks.
/// - **YAML**: top-level keys (indentation 0) delimit blocks.
/// - **JSON**: top-level object keys (heuristic).
/// - **Markdown**: heading lines (`#`, `##`) delimit sections.
///
/// All units are emitted as [`UnitKind::DeclarationGroup`].
#[derive(Debug, Clone, Copy)]
pub struct ConfigExtractor;

impl LanguageExtractor for ConfigExtractor {
    fn language_id(&self) -> &str {
        "config"
    }

    fn extensions(&self) -> &[&str] {
        &["toml", "yaml", "yml", "json", "md", "markdown"]
    }

    fn extract_units(
        &self,
        content: &str,
        file_path: &str,
        changed_lines: &HashSet<u32>,
    ) -> Vec<SemanticUnit> {
        if changed_lines.is_empty() {
            return vec![];
        }

        let ext = file_path.rsplit('.').next().unwrap_or("");
        let lines: Vec<&str> = content.lines().collect();

        let sections = match ext {
            "toml" => detect_toml_sections(&lines),
            "yaml" | "yml" => detect_yaml_sections(&lines),
            "json" => detect_json_sections(&lines),
            "md" | "markdown" => detect_markdown_sections(&lines),
            _ => vec![],
        };

        let mut units = Vec::new();
        for section in &sections {
            let overlaps = changed_lines
                .iter()
                .any(|&l| l >= section.start_line && l <= section.end_line);
            if overlaps {
                let changed_in_unit: Vec<u32> = changed_lines
                    .iter()
                    .copied()
                    .filter(|&l| l >= section.start_line && l <= section.end_line)
                    .collect();
                let start_idx = (section.start_line - 1) as usize;
                let end_idx = section.end_line as usize;
                let unit_content = lines
                    .get(start_idx..end_idx.min(lines.len()))
                    .unwrap_or(&[])
                    .join("\n");

                units.push(SemanticUnit {
                    kind: UnitKind::DeclarationGroup,
                    name: section.name.clone(),
                    file: file_path.to_string(),
                    start_line: section.start_line,
                    end_line: section.end_line,
                    content: unit_content,
                    changed_lines: changed_in_unit,
                    context: FileContext::default(),
                });
            }
        }

        units
    }
}

/// A detected section in a config file.
struct Section {
    name: String,
    start_line: u32,
    end_line: u32,
}

/// Detect TOML sections delimited by `[section]` headers.
fn detect_toml_sections(lines: &[&str]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_start: u32 = 1;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let line_num = u32::try_from(i + 1).unwrap_or(u32::MAX);

        if trimmed.starts_with('[') && !trimmed.starts_with("[[") {
            // Close previous section
            if let Some(name) = current_name.take() {
                sections.push(Section {
                    name,
                    start_line: current_start,
                    end_line: line_num - 1,
                });
            }
            // Start new section
            let name = trimmed
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_string();
            current_name = Some(name);
            current_start = line_num;
        }
    }

    // Close last section
    if let Some(name) = current_name {
        let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
        sections.push(Section {
            name,
            start_line: current_start,
            end_line: total,
        });
    }

    // If no sections found, treat entire file as one section
    if sections.is_empty() && !lines.is_empty() {
        sections.push(Section {
            name: "root".to_string(),
            start_line: 1,
            end_line: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        });
    }

    sections
}

/// Detect YAML sections delimited by top-level keys (indent 0).
fn detect_yaml_sections(lines: &[&str]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_start: u32 = 1;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let line_num = u32::try_from(i + 1).unwrap_or(u32::MAX);

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let indent = line.len() - line.trim_start().len();
        if indent == 0 && trimmed.contains(':') {
            // Close previous section
            if let Some(name) = current_name.take() {
                sections.push(Section {
                    name,
                    start_line: current_start,
                    end_line: line_num - 1,
                });
            }
            let name = trimmed
                .split(':')
                .next()
                .unwrap_or("unknown")
                .trim()
                .to_string();
            current_name = Some(name);
            current_start = line_num;
        }
    }

    if let Some(name) = current_name {
        let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
        sections.push(Section {
            name,
            start_line: current_start,
            end_line: total,
        });
    }

    if sections.is_empty() && !lines.is_empty() {
        sections.push(Section {
            name: "root".to_string(),
            start_line: 1,
            end_line: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        });
    }

    sections
}

/// Detect JSON top-level keys (heuristic).
fn detect_json_sections(lines: &[&str]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_start: u32 = 1;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let line_num = u32::try_from(i + 1).unwrap_or(u32::MAX);

        // Simple heuristic: top-level keys are lines with " at indent 2 (inside root {})
        let indent = line.len() - line.trim_start().len();
        if indent <= 2 && trimmed.starts_with('"') && trimmed.contains(':') {
            if let Some(name) = current_name.take() {
                sections.push(Section {
                    name,
                    start_line: current_start,
                    end_line: line_num - 1,
                });
            }
            let name = trimmed
                .trim_start_matches('"')
                .split('"')
                .next()
                .unwrap_or("unknown")
                .to_string();
            current_name = Some(name);
            current_start = line_num;
        }
    }

    if let Some(name) = current_name {
        let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
        sections.push(Section {
            name,
            start_line: current_start,
            end_line: total,
        });
    }

    if sections.is_empty() && !lines.is_empty() {
        sections.push(Section {
            name: "root".to_string(),
            start_line: 1,
            end_line: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        });
    }

    sections
}

/// Detect Markdown sections delimited by headings.
fn detect_markdown_sections(lines: &[&str]) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_start: u32 = 1;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let line_num = u32::try_from(i + 1).unwrap_or(u32::MAX);

        if trimmed.starts_with('#') {
            if let Some(name) = current_name.take() {
                sections.push(Section {
                    name,
                    start_line: current_start,
                    end_line: line_num - 1,
                });
            }
            let name = trimmed.trim_start_matches('#').trim().to_string();
            current_name = Some(name);
            current_start = line_num;
        }
    }

    if let Some(name) = current_name {
        let total = u32::try_from(lines.len()).unwrap_or(u32::MAX);
        sections.push(Section {
            name,
            start_line: current_start,
            end_line: total,
        });
    }

    if sections.is_empty() && !lines.is_empty() {
        sections.push(Section {
            name: "document".to_string(),
            start_line: 1,
            end_line: u32::try_from(lines.len()).unwrap_or(u32::MAX),
        });
    }

    sections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(code: &str, file: &str, changed: &[u32]) -> Vec<SemanticUnit> {
        let ext = ConfigExtractor;
        let changed_set: HashSet<u32> = changed.iter().copied().collect();
        ext.extract_units(code, file, &changed_set)
    }

    // --- Test 33: toml_extracts_section ---
    #[test]
    fn toml_extracts_section() {
        let code = "[review]\nmodel = \"claude\"\nmax_lines = 500\n\n[azure]\nendpoint = \"https://example.com\"";
        let units = extract(code, "config.toml", &[2]);
        assert_eq!(units.len(), 1, "Should extract one section");
        assert_eq!(units[0].kind, UnitKind::DeclarationGroup);
        assert_eq!(units[0].name, "review");
        assert!(units[0].content.contains("model"));
    }

    // --- Test 34: yaml_extracts_top_level_key ---
    #[test]
    fn yaml_extracts_top_level_key() {
        let code = "deploy:\n  image: nginx\n  replicas: 3\n\nservice:\n  port: 80";
        let units = extract(code, "app.yaml", &[2]);
        assert_eq!(units.len(), 1, "Should extract one section");
        assert_eq!(units[0].kind, UnitKind::DeclarationGroup);
        assert_eq!(units[0].name, "deploy");
    }
}
