//! Symbol-scoped diagnostics: group and prioritize diagnostics by containing symbol.
//!
//! Maps diagnostic ranges to containing symbols, prioritizes by edit context,
//! and distinguishes "no errors" from "diagnostics unavailable".

use std::collections::BTreeMap;

/// Diagnostic severity level.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Hint,
}

impl DiagnosticSeverity {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Hint => "hint",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "error" | "err" | "e" => Self::Error,
            "warning" | "warn" | "w" => Self::Warning,
            "info" | "i" => Self::Info,
            "hint" | "h" => Self::Hint,
            _ => Self::Info,
        }
    }
}

/// LSP backend availability status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum LspAvailability {
    /// LSP is available and returning diagnostics.
    Available,
    /// LSP is available but returned an error.
    Error,
    /// No LSP server is configured for this language.
    Unavailable,
    /// LSP is configured but not yet initialized.
    NotInitialized,
}

impl LspAvailability {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Error => "error",
            Self::Unavailable => "unavailable",
            Self::NotInitialized => "not_initialized",
        }
    }
}

/// A single diagnostic with position and severity.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    /// File path.
    pub file: String,
    /// 1-based line number.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// Severity.
    pub severity: DiagnosticSeverity,
    /// Error/warning message.
    pub message: String,
    /// Source (e.g., "rustc", "eslint", "typescript").
    pub source: String,
    /// Code (e.g., "E0308", "no-unused-vars").
    pub code: Option<String>,
}

/// A group of diagnostics for a containing symbol.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolDiagnosticGroup {
    /// Symbol name (if determinable).
    pub symbol_name: Option<String>,
    /// Symbol kind (function, struct, etc.).
    pub symbol_kind: Option<String>,
    /// Symbol start line (1-based).
    pub symbol_line: u32,
    /// Diagnostics in this symbol, sorted by severity.
    pub diagnostics: Vec<Diagnostic>,
    /// Priority rank (1 = edited symbol, 2 = referencing symbol, 3 = same file, 4 = broader).
    pub priority: u32,
}

/// Result of symbol-scoped diagnostic analysis.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SymbolDiagnosticsResult {
    /// Diagnostic groups sorted by priority.
    pub groups: Vec<SymbolDiagnosticGroup>,
    /// Total diagnostics across all groups.
    pub total_diagnostics: u32,
    /// Diagnostics by severity.
    pub severity_counts: BTreeMap<String, u32>,
    /// LSP backend status.
    pub lsp_status: LspAvailability,
    /// Whether diagnostics are complete (vs. partial due to unavailable backend).
    pub complete: bool,
    /// Message when not complete.
    pub message: Option<String>,
}

impl SymbolDiagnosticsResult {
    /// Create a result indicating no LSP is available.
    pub fn unavailable(reason: &str) -> Self {
        Self {
            groups: Vec::new(),
            total_diagnostics: 0,
            severity_counts: BTreeMap::new(),
            lsp_status: LspAvailability::Unavailable,
            complete: false,
            message: Some(format!("diagnostics unavailable: {reason}")),
        }
    }

    /// Create a result with no errors.
    pub fn no_errors(lsp_status: LspAvailability) -> Self {
        Self {
            groups: Vec::new(),
            total_diagnostics: 0,
            severity_counts: BTreeMap::new(),
            lsp_status,
            complete: true,
            message: Some("no errors".to_string()),
        }
    }
}

/// Group diagnostics by containing symbol and prioritize by edit context.
///
/// `diagnostics` — raw diagnostics from LSP or parser.
/// `edited_line` — the line that was just edited (if any), used for priority boosting.
/// `edited_symbol` — the symbol name that was just edited (if determinable).
pub fn group_diagnostics(
    diagnostics: Vec<Diagnostic>,
    edited_line: Option<u32>,
    _edited_symbol: Option<&str>,
) -> SymbolDiagnosticsResult {
    if diagnostics.is_empty() {
        return SymbolDiagnosticsResult::no_errors(LspAvailability::Available);
    }

    // Build severity counts
    let mut severity_counts = BTreeMap::new();
    for d in &diagnostics {
        *severity_counts
            .entry(d.severity.label().to_string())
            .or_insert(0) += 1;
    }

    // Group diagnostics by approximate "containing symbol" (line proximity heuristic)
    // In a full implementation, this would use LSP document symbols or callgraph data.
    let mut groups: BTreeMap<u32, Vec<Diagnostic>> = BTreeMap::new();

    for d in &diagnostics {
        // Simple heuristic: group by 50-line windows
        let bucket = (d.line / 50) * 50;
        groups.entry(bucket).or_default().push(d.clone());
    }

    // Convert to SymbolDiagnosticGroup with priority
    let mut symbol_groups: Vec<SymbolDiagnosticGroup> = groups
        .into_iter()
        .map(|(start_line, mut diags)| {
            // Sort by severity (errors first)
            diags.sort_by(|a, b| a.severity.cmp(&b.severity));

            // Determine priority based on edit context
            let priority = if let Some(edited) = edited_line {
                let edited_bucket = (edited / 50) * 50;
                if start_line == edited_bucket {
                    1 // Edited symbol
                } else if (start_line as i32 - edited as i32).unsigned_abs() < 100 {
                    2 // Nearby symbol (likely referencing)
                } else {
                    3 // Same file but distant
                }
            } else {
                4 // No edit context
            };

            SymbolDiagnosticGroup {
                symbol_name: None,
                symbol_kind: None,
                symbol_line: start_line,
                diagnostics: diags,
                priority,
            }
        })
        .collect();

    // Sort by priority
    symbol_groups.sort_by_key(|g| g.priority);

    SymbolDiagnosticsResult {
        groups: symbol_groups,
        total_diagnostics: severity_counts.values().sum(),
        severity_counts,
        lsp_status: LspAvailability::Available,
        complete: true,
        message: None,
    }
}

/// Map a diagnostic range to a containing symbol using LSP document symbols.
///
/// Returns the symbol name, kind, and priority boost.
pub fn map_diagnostic_to_symbol(
    diagnostic_line: u32,
    document_symbols: &[(String, String, u32, u32)], // (name, kind, start_line, end_line)
    edited_line: Option<u32>,
) -> (Option<String>, Option<String>, u32) {
    // Find the symbol that contains this line
    for (name, kind, start, end) in document_symbols {
        if diagnostic_line >= *start && diagnostic_line <= *end {
            let priority = if Some(diagnostic_line) == edited_line {
                1 // Edited symbol
            } else {
                2 // Same symbol but not edited
            };
            return (Some(name.clone()), Some(kind.clone()), priority);
        }
    }

    // No containing symbol found
    let priority = if let Some(edited) = edited_line {
        if (diagnostic_line as i32 - edited as i32).unsigned_abs() < 20 {
            2 // Near edited line
        } else {
            4 // Distant
        }
    } else {
        4
    };

    (None, None, priority)
}

/// Create a mock diagnostics result for testing unavailable state.
pub fn mock_unavailable_result() -> SymbolDiagnosticsResult {
    SymbolDiagnosticsResult::unavailable("no LSP server configured")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_diagnostics_returns_no_errors() {
        let result = group_diagnostics(vec![], None, None);
        assert_eq!(result.total_diagnostics, 0);
        assert!(result.complete);
        assert!(result.message.unwrap().contains("no errors"));
    }

    #[test]
    fn single_error_groups_correctly() {
        let diag = Diagnostic {
            file: "src/main.rs".to_string(),
            line: 10,
            column: 5,
            severity: DiagnosticSeverity::Error,
            message: "type mismatch".to_string(),
            source: "rustc".to_string(),
            code: Some("E0308".to_string()),
        };
        let result = group_diagnostics(vec![diag], None, None);
        assert_eq!(result.total_diagnostics, 1);
        assert_eq!(result.groups.len(), 1);
        assert_eq!(result.severity_counts.get("error"), Some(&1));
    }

    #[test]
    fn edited_line_gets_priority_1() {
        let diag = Diagnostic {
            file: "src/main.rs".to_string(),
            line: 10,
            column: 5,
            severity: DiagnosticSeverity::Error,
            message: "error".to_string(),
            source: "rustc".to_string(),
            code: None,
        };
        let result = group_diagnostics(vec![diag], Some(10), None);
        assert_eq!(result.groups[0].priority, 1);
    }

    #[test]
    fn nearby_line_gets_priority_2() {
        let diag = Diagnostic {
            file: "src/main.rs".to_string(),
            line: 10,
            column: 5,
            severity: DiagnosticSeverity::Error,
            message: "error".to_string(),
            source: "rustc".to_string(),
            code: None,
        };
        let result = group_diagnostics(vec![diag], Some(50), None);
        assert_eq!(result.groups[0].priority, 2);
    }

    #[test]
    fn distant_line_gets_priority_3() {
        let diag = Diagnostic {
            file: "src/main.rs".to_string(),
            line: 10,
            column: 5,
            severity: DiagnosticSeverity::Error,
            message: "error".to_string(),
            source: "rustc".to_string(),
            code: None,
        };
        let result = group_diagnostics(vec![diag], Some(500), None);
        assert_eq!(result.groups[0].priority, 3);
    }

    #[test]
    fn severity_counts_are_correct() {
        let diags = vec![
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 10,
                column: 5,
                severity: DiagnosticSeverity::Error,
                message: "err1".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 20,
                column: 5,
                severity: DiagnosticSeverity::Warning,
                message: "warn1".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 30,
                column: 5,
                severity: DiagnosticSeverity::Error,
                message: "err2".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
        ];
        let result = group_diagnostics(diags, None, None);
        assert_eq!(result.severity_counts.get("error"), Some(&2));
        assert_eq!(result.severity_counts.get("warning"), Some(&1));
    }

    #[test]
    fn unavailable_result_has_correct_state() {
        let result = mock_unavailable_result();
        assert!(!result.complete);
        assert_eq!(result.lsp_status, LspAvailability::Unavailable);
        assert!(result.groups.is_empty());
    }

    #[test]
    fn diagnostics_sorted_by_severity_within_group() {
        let diags = vec![
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 10,
                column: 5,
                severity: DiagnosticSeverity::Hint,
                message: "hint".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 12,
                column: 5,
                severity: DiagnosticSeverity::Error,
                message: "error".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
            Diagnostic {
                file: "src/main.rs".to_string(),
                line: 11,
                column: 5,
                severity: DiagnosticSeverity::Warning,
                message: "warning".to_string(),
                source: "rustc".to_string(),
                code: None,
            },
        ];
        let result = group_diagnostics(diags, None, None);
        // Within the same bucket, error should come first
        let group = &result.groups[0];
        assert_eq!(group.diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(group.diagnostics[1].severity, DiagnosticSeverity::Warning);
        assert_eq!(group.diagnostics[2].severity, DiagnosticSeverity::Hint);
    }
}
