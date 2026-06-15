//! Export Detection for Mutation Risk Assessment.
//!
//! Compares pre/post mutation exported symbols to detect removed exports,
//! added exports, and changed public symbol contracts.

use crate::mutation_risk::FileKind;

/// A detected export change.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportChange {
    /// The symbol name.
    pub name: String,
    /// What kind of change occurred.
    pub change_kind: ExportChangeKind,
    /// The file where the change was detected.
    pub file_path: String,
    /// Additional context (e.g., old signature).
    pub context: Option<String>,
}

/// Kind of export change.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum ExportChangeKind {
    /// An export was removed.
    Removed,
    /// A new export was added.
    Added,
    /// An export signature appears to have changed.
    SignatureChanged,
}

/// Result of export detection for a mutation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ExportDetectionResult {
    /// Whether export detection was performed (false if unavailable).
    pub detected: bool,
    /// Removed exports (breaking changes).
    pub removed: Vec<ExportChange>,
    /// Added exports (non-breaking).
    pub added: Vec<ExportChange>,
    /// Signature changes (potentially breaking).
    pub signature_changes: Vec<ExportChange>,
    /// Overall risk adjustment from export changes.
    pub risk_adjustment: f64,
    /// Human-readable summary.
    pub summary: String,
}

impl ExportDetectionResult {
    /// Create an empty result (detection unavailable).
    pub fn unavailable() -> Self {
        Self {
            detected: false,
            removed: Vec::new(),
            added: Vec::new(),
            signature_changes: Vec::new(),
            risk_adjustment: 0.0,
            summary: "Export detection unavailable".to_string(),
        }
    }

    /// Whether any breaking changes were detected.
    pub fn has_breaking_changes(&self) -> bool {
        !self.removed.is_empty() || !self.signature_changes.is_empty()
    }
}

/// Detect export changes between old and new file content.
///
/// Uses simple heuristics to extract exported symbols from source code.
/// This is a best-effort approach — not a full semantic analysis.
pub fn detect_export_changes(
    old_content: &str,
    new_content: &str,
    file_path: &str,
    file_kind: FileKind,
) -> ExportDetectionResult {
    // Export detection is only supported for source files
    if !matches!(file_kind, FileKind::Source) {
        return ExportDetectionResult::unavailable();
    }

    let old_exports = extract_exports(old_content, file_kind);
    let new_exports = extract_exports(new_content, file_kind);

    let mut removed = Vec::new();
    let mut added = Vec::new();
    let mut signature_changes = Vec::new();

    // Detect removed exports
    for old_export in &old_exports {
        if !new_exports.iter().any(|n| n.name == old_export.name) {
            removed.push(ExportChange {
                name: old_export.name.clone(),
                change_kind: ExportChangeKind::Removed,
                file_path: file_path.to_string(),
                context: old_export.signature.clone(),
            });
        }
    }

    // Detect added exports
    for new_export in &new_exports {
        if !old_exports.iter().any(|n| n.name == new_export.name) {
            added.push(ExportChange {
                name: new_export.name.clone(),
                change_kind: ExportChangeKind::Added,
                file_path: file_path.to_string(),
                context: new_export.signature.clone(),
            });
        }
    }

    // Detect signature changes (same name, different signature)
    for old_export in &old_exports {
        if let Some(new_export) = new_exports.iter().find(|n| n.name == old_export.name) {
            if old_export.signature != new_export.signature {
                signature_changes.push(ExportChange {
                    name: old_export.name.clone(),
                    change_kind: ExportChangeKind::SignatureChanged,
                    file_path: file_path.to_string(),
                    context: Some(format!(
                        "old: {} → new: {}",
                        old_export.signature.as_deref().unwrap_or("?"),
                        new_export.signature.as_deref().unwrap_or("?")
                    )),
                });
            }
        }
    }

    // Compute risk adjustment
    let risk_adjustment = (removed.len() as f64 * 0.3)
        + (signature_changes.len() as f64 * 0.2)
        + (added.len() as f64 * 0.05);

    // Build summary
    let mut parts = Vec::new();
    if !removed.is_empty() {
        parts.push(format!("{} export(s) removed", removed.len()));
    }
    if !added.is_empty() {
        parts.push(format!("{} export(s) added", added.len()));
    }
    if !signature_changes.is_empty() {
        parts.push(format!("{} signature(s) changed", signature_changes.len()));
    }
    let summary = if parts.is_empty() {
        "No export changes detected".to_string()
    } else {
        parts.join(", ")
    };

    ExportDetectionResult {
        detected: true,
        removed,
        added,
        signature_changes,
        risk_adjustment,
        summary,
    }
}

/// Internal export representation for comparison.
#[derive(Debug, Clone)]
struct ExportedSymbol {
    name: String,
    signature: Option<String>,
}

/// Extract exported symbols from source content.
fn extract_exports(content: &str, file_kind: FileKind) -> Vec<ExportedSymbol> {
    match file_kind {
        FileKind::Source => extract_source_exports(content),
        _ => Vec::new(),
    }
}

/// Extract exports from source code files.
fn extract_source_exports(content: &str) -> Vec<ExportedSymbol> {
    let mut exports = Vec::new();
    let mut in_block_comment = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // Track block comments
        if trimmed.contains("/*") && !trimmed.contains("*/") {
            in_block_comment = true;
            continue;
        }
        if in_block_comment {
            if trimmed.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }

        // Skip single-line comments
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("///") {
            continue;
        }

        // TypeScript/JavaScript exports
        if let Some(sym) = extract_ts_export(trimmed) {
            exports.push(sym);
            continue;
        }

        // Rust public items
        if let Some(sym) = extract_rust_export(trimmed) {
            exports.push(sym);
            continue;
        }

        // Python exports
        if let Some(sym) = extract_python_export(trimmed) {
            exports.push(sym);
            continue;
        }

        // Go exports
        if let Some(sym) = extract_go_export(trimmed) {
            exports.push(sym);
            continue;
        }
    }

    exports
}

/// Extract TypeScript/JavaScript exported symbol.
fn extract_ts_export(line: &str) -> Option<ExportedSymbol> {
    // Named exports: export function foo, export class Foo, export const foo
    if line.starts_with("export ") {
        let rest = &line[7..]; // skip "export "
        let words: Vec<&str> = rest.split_whitespace().collect();
        if words.len() >= 2 {
            let name = clean_export_name(words[1]);
            // Extract full signature up to opening brace
            let sig_end = rest.find('{').unwrap_or(rest.len());
            let sig_text = rest[..sig_end].trim().to_string();
            return Some(ExportedSymbol {
                name: name.to_string(),
                signature: Some(sig_text),
            });
        }
    }

    // Default export: export default function/class
    if line.starts_with("export default ") {
        let rest = &line[15..]; // skip "export default "
        let words: Vec<&str> = rest.split_whitespace().collect();
        if !words.is_empty() {
            let kind = words[0];
            let name = if words.len() >= 2 {
                clean_export_name(words[1])
            } else {
                "default"
            };
            return Some(ExportedSymbol {
                name: name.to_string(),
                signature: Some(format!("export default {kind}")),
            });
        }
    }

    // export { ... } or export { ... } from '...'
    if line.starts_with("export {") {
        // Simple extraction of names inside braces
        if let Some(start) = line.find('{') {
            if let Some(end) = line.find('}') {
                let names_str = &line[start + 1..end];
                // Return the first exported name
                let first_name = names_str.split(',').next()?.split_whitespace().next()?;
                if first_name != "type" && !first_name.is_empty() {
                    return Some(ExportedSymbol {
                        name: first_name.to_string(),
                        signature: Some(format!("export {{ {first_name} }}")),
                    });
                }
            }
        }
    }

    None
}

/// Clean an export name by stripping trailing parentheses, braces, and colons.
fn clean_export_name(raw: &str) -> &str {
    let mut name = raw;
    // Strip trailing junk chars
    while let Some(last) = name.chars().last() {
        if last.is_alphanumeric() || last == '_' {
            break;
        }
        name = &name[..name.len() - last.len_utf8()];
    }
    name
}

/// Extract Rust public symbol.
fn extract_rust_export(line: &str) -> Option<ExportedSymbol> {
    if !line.starts_with("pub ") {
        return None;
    }

    let rest = &line[4..]; // skip "pub "
    let words: Vec<&str> = rest.split_whitespace().collect();
    if words.len() >= 2 {
        let kind = words[0];
        let name = words[1].trim_end_matches('(').trim_end_matches('{');
        let sig = format!("pub {kind} {name}");
        return Some(ExportedSymbol {
            name: name.to_string(),
            signature: Some(sig),
        });
    }

    None
}

/// Extract Python export (from module-level __all__ or public names).
fn extract_python_export(line: &str) -> Option<ExportedSymbol> {
    // __all__ = ['foo', 'bar']
    if line.starts_with("__all__") {
        // Extract names from __all__
        if let Some(start) = line.find('[') {
            if let Some(end) = line.find(']') {
                let names_str = &line[start + 1..end];
                // This is a special case - we return multiple exports
                // For simplicity, we'll handle this by returning the first name
                // and noting it's from __all__
                let first_name = names_str
                    .split(',')
                    .next()?
                    .trim()
                    .trim_matches('\'')
                    .trim_matches('"');
                return Some(ExportedSymbol {
                    name: first_name.to_string(),
                    signature: Some("__all__".to_string()),
                });
            }
        }
    }

    // Regular Python "def" or "class" at module level (not prefixed with _)
    if (line.starts_with("def ") || line.starts_with("class "))
        && !line.starts_with("def _")
        && !line.starts_with("class _")
    {
        let words: Vec<&str> = line.split_whitespace().collect();
        if words.len() >= 2 {
            let kind = words[0];
            let name = words[1].trim_end_matches('(').trim_end_matches(':');
            return Some(ExportedSymbol {
                name: name.to_string(),
                signature: Some(format!("{kind} {name}")),
            });
        }
    }

    None
}

/// Extract Go exported symbol (starts with uppercase).
fn extract_go_export(line: &str) -> Option<ExportedSymbol> {
    let words: Vec<&str> = line.split_whitespace().collect();
    if words.len() < 2 {
        return None;
    }

    let kind = words[0];
    if !matches!(kind, "func" | "type" | "var" | "const") {
        return None;
    }

    let name = words[1].trim_start_matches('(');
    // Go exports start with uppercase
    if name.chars().next()?.is_uppercase() {
        let sig = format!("{kind} {name}");
        return Some(ExportedSymbol {
            name: name.to_string(),
            signature: Some(sig),
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_no_changes() {
        let content = "export function foo() { return 1; }";
        let result = detect_export_changes(content, content, "test.ts", FileKind::Source);
        assert!(result.detected);
        assert!(result.removed.is_empty());
        assert!(result.added.is_empty());
        assert!(result.signature_changes.is_empty());
        assert_eq!(result.risk_adjustment, 0.0);
    }

    #[test]
    fn detect_removed_export() {
        let old = "export function foo() { return 1; }\nexport function bar() { return 2; }";
        let new = "export function foo() { return 1; }";
        let result = detect_export_changes(old, new, "test.ts", FileKind::Source);
        assert!(result.detected);
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].name, "bar");
        assert!(result.has_breaking_changes());
    }

    #[test]
    fn detect_added_export() {
        let old = "export function foo() { return 1; }";
        let new = "export function foo() { return 1; }\nexport function baz() { return 3; }";
        let result = detect_export_changes(old, new, "test.ts", FileKind::Source);
        assert!(result.detected);
        assert_eq!(result.added.len(), 1);
        assert_eq!(result.added[0].name, "baz");
        assert!(!result.has_breaking_changes());
    }

    #[test]
    fn detect_signature_change() {
        let old = "export function foo(x: number) { return x; }";
        let new = "export function foo(x: number, y: string) { return x; }";
        let result = detect_export_changes(old, new, "test.ts", FileKind::Source);
        assert!(result.detected);
        assert_eq!(result.signature_changes.len(), 1);
        assert!(result.has_breaking_changes());
    }

    #[test]
    fn detection_unavailable_for_non_source() {
        let result = detect_export_changes("", "", "test.md", FileKind::Documentation);
        assert!(!result.detected);
    }

    #[test]
    fn rust_public_exports() {
        let content = "pub fn foo() {}\npub struct Bar {}\npub enum Baz {}";
        let result = detect_export_changes(content, content, "lib.rs", FileKind::Source);
        assert!(result.detected);
        assert_eq!(result.removed.len(), 0);
    }

    #[test]
    fn rust_removed_public_export() {
        let old = "pub fn foo() {}\npub struct Bar {}";
        let new = "pub fn foo() {}";
        let result = detect_export_changes(old, new, "lib.rs", FileKind::Source);
        assert!(result.detected);
        assert_eq!(result.removed.len(), 1);
        assert_eq!(result.removed[0].name, "Bar");
    }

    #[test]
    fn summary_formatting() {
        let old = "export function a() {}\nexport function b() {}";
        let new = "export function a() {}";
        let result = detect_export_changes(old, new, "test.ts", FileKind::Source);
        assert!(result.summary.contains("1 export(s) removed"));
    }

    #[test]
    fn risk_adjustment_calculation() {
        let old = "export function a() {}\nexport function b() {}";
        let new = "export function a() {}\nexport function c() {}";
        let result = detect_export_changes(old, new, "test.ts", FileKind::Source);
        // 1 removed (0.3) + 1 added (0.05) = 0.35
        assert!((result.risk_adjustment - 0.35).abs() < 0.01);
    }
}
