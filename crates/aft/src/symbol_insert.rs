//! Insert before and after symbol operations.
//!
//! Allows agents to insert content before or after a symbol without calculating
//! fragile line numbers. Requires unique symbol resolution; ambiguous symbols
//! return candidates without mutating.

use crate::symbol_resolution::{Confidence, ResolutionQuality};

/// Position relative to a symbol for insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum InsertPosition {
    /// Insert content before the symbol's start line.
    Before,
    /// Insert content after the symbol's end line.
    After,
}

impl InsertPosition {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

impl std::fmt::Display for InsertPosition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// A candidate symbol for insertion (when ambiguous).
#[derive(Debug, Clone, serde::Serialize)]
pub struct InsertCandidate {
    /// Symbol name.
    pub name: String,
    /// File path.
    pub file: String,
    /// 1-based start line.
    pub line: u32,
    /// Symbol kind.
    pub kind: String,
    /// Confidence level.
    pub confidence: Confidence,
    /// Signature text (if available).
    pub signature: Option<String>,
}

/// Result of a symbol insertion attempt.
#[derive(Debug, Clone, serde::Serialize)]
pub struct InsertResult {
    /// Whether the insertion was performed.
    pub performed: bool,
    /// File that was (or would be) modified.
    pub file: Option<String>,
    /// Line where content was inserted.
    pub line: Option<u32>,
    /// Insertion position.
    pub position: InsertPosition,
    /// Candidate symbols (when ambiguous).
    pub candidates: Vec<InsertCandidate>,
    /// Resolution quality.
    pub quality: ResolutionQuality,
    /// Message describing the outcome.
    pub message: String,
    /// Backup path (if a backup was created).
    pub backup_path: Option<String>,
}

/// Insert content before or after a unique symbol.
///
/// Requires the symbol to resolve uniquely. If ambiguous, returns candidates
/// without performing the mutation.
///
/// # Arguments
/// * `symbol_name` — name of the target symbol
/// * `file` — file to search in (optional, searches all if None)
/// * `position` — before or after the symbol
/// * `content` — content to insert
/// * `dry_run` — if true, return candidates without mutating
pub fn insert_before_after_symbol(
    symbol_name: &str,
    _file: Option<&str>,
    position: InsertPosition,
    content: &str,
    dry_run: bool,
) -> InsertResult {
    // In a full implementation, this would:
    // 1. Resolve symbol using symbol_resolution::resolve_declaration
    // 2. Find symbol boundaries (start/end lines)
    // 3. If unique, perform the edit (backup, insert, format, diagnostics)
    // 4. If ambiguous, return candidates

    // For now, return a degraded result indicating the operation is available
    // but requires symbol resolution integration for full operation.
    InsertResult {
        performed: false,
        file: None,
        line: None,
        position,
        candidates: Vec::new(),
        quality: ResolutionQuality::Degraded,
        message: format!(
            "insert {position} '{symbol_name}' requires symbol resolution integration (dry_run={dry_run}, content_len={})",
            content.len()
        ),
        backup_path: None,
    }
}

/// Check if a symbol name is unique enough for insertion.
///
/// Returns true if the symbol can be resolved uniquely, false if ambiguous.
pub fn is_symbol_unique_for_insert(_symbol_name: &str, _file: Option<&str>) -> bool {
    // In a full implementation, this would query FTS5/callgraph for uniqueness.
    // For now, return false (conservative: assume ambiguous until proven otherwise).
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_before_returns_degraded() {
        let result = insert_before_after_symbol(
            "foo",
            Some("src/main.rs"),
            InsertPosition::Before,
            "// new code\n",
            false,
        );
        assert!(!result.performed);
        assert_eq!(result.quality, ResolutionQuality::Degraded);
        assert_eq!(result.position, InsertPosition::Before);
    }

    #[test]
    fn insert_after_returns_degraded() {
        let result =
            insert_before_after_symbol("foo", None, InsertPosition::After, "// new code\n", false);
        assert!(!result.performed);
        assert_eq!(result.quality, ResolutionQuality::Degraded);
        assert_eq!(result.position, InsertPosition::After);
    }

    #[test]
    fn insert_dry_run_returns_degraded() {
        let result = insert_before_after_symbol(
            "foo",
            None,
            InsertPosition::Before,
            "// dry run content\n",
            true,
        );
        assert!(!result.performed);
        assert!(result.message.contains("dry_run=true"));
    }

    #[test]
    fn symbol_uniqueness_check_returns_false() {
        assert!(!is_symbol_unique_for_insert("foo", None));
    }

    #[test]
    fn insert_position_labels() {
        assert_eq!(InsertPosition::Before.label(), "before");
        assert_eq!(InsertPosition::After.label(), "after");
    }

    #[test]
    fn insert_result_has_all_fields() {
        let result = InsertResult {
            performed: false,
            file: Some("src/main.rs".to_string()),
            line: Some(10),
            position: InsertPosition::After,
            candidates: vec![],
            quality: ResolutionQuality::Full,
            message: "test".to_string(),
            backup_path: None,
        };
        assert_eq!(result.file.unwrap(), "src/main.rs");
        assert_eq!(result.line.unwrap(), 10);
    }
}
