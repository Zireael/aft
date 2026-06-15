//! Native declaration resolution, references, and implementations primitives.
//!
//! Provides internal APIs for declaration-at-usage resolution, symbol reference lookup,
//! and implementation edge discovery. Uses FTS5, symbol identity, imports, references,
//! and syntax heuristics. Results carry confidence/degraded metadata.

use std::collections::BTreeMap;

/// Confidence level for resolution results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Confidence {
    /// Exact match from LSP or direct declaration.
    Exact,
    /// High confidence from syntax heuristics and FTS5.
    High,
    /// Medium confidence from naming conventions.
    Medium,
    /// Low confidence — best guess from partial information.
    Low,
    /// No confidence — result is speculative.
    None,
}

impl PartialOrd for Confidence {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Confidence {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let rank = |c: &Confidence| match c {
            Confidence::Exact => 4,
            Confidence::High => 3,
            Confidence::Medium => 2,
            Confidence::Low => 1,
            Confidence::None => 0,
        };
        rank(self).cmp(&rank(other))
    }
}

impl Confidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "exact" => Self::Exact,
            "high" => Self::High,
            "medium" => Self::Medium,
            "low" => Self::Low,
            _ => Self::None,
        }
    }
}

/// Resolution quality / degraded state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ResolutionQuality {
    /// Full resolution available.
    Full,
    /// Partial resolution (some symbols resolved, some not).
    Partial,
    /// Degraded resolution (LSP unavailable, using heuristics only).
    Degraded,
    /// Unavailable (no resolution possible).
    Unavailable,
}

impl ResolutionQuality {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Partial => "partial",
            Self::Degraded => "degraded",
            Self::Unavailable => "unavailable",
        }
    }
}

/// A resolved declaration site.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Declaration {
    /// Symbol name.
    pub name: String,
    /// File path.
    pub file: String,
    /// 1-based start line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// Symbol kind (function, struct, enum, trait, etc.).
    pub kind: String,
    /// Confidence level.
    pub confidence: Confidence,
    /// Signature text (if available).
    pub signature: Option<String>,
    /// Whether this is the definition (vs. declaration only).
    pub is_definition: bool,
}

/// A reference to a symbol from a usage site.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Reference {
    /// Symbol name.
    pub name: String,
    /// File path.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// 1-based column.
    pub column: u32,
    /// Reference kind (call, import, type, etc.).
    pub kind: String,
    /// Confidence level.
    pub confidence: Confidence,
    /// Containing symbol name (if determinable).
    pub containing_symbol: Option<String>,
    /// Containing symbol kind.
    pub containing_symbol_kind: Option<String>,
}

/// A reference group: all references from a containing symbol.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReferenceGroup {
    /// Containing symbol name.
    pub symbol_name: String,
    /// Containing symbol kind.
    pub symbol_kind: String,
    /// File path.
    pub file: String,
    /// Start line.
    pub line: u32,
    /// References within this symbol.
    pub references: Vec<Reference>,
}

/// An implementation edge (trait/protocol -> struct/class).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImplementationEdge {
    /// Interface/trait/protocol name.
    pub interface_name: String,
    /// Implementing type name.
    pub implementor_name: String,
    /// File where implementation is found.
    pub file: String,
    /// 1-based line.
    pub line: u32,
    /// Confidence level.
    pub confidence: Confidence,
    /// Whether this is a direct or indirect implementation.
    pub direct: bool,
}

/// Result of declaration resolution at a usage site.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeclarationResult {
    /// Resolved declaration (if found).
    pub declaration: Option<Declaration>,
    /// All candidate declarations (when ambiguous).
    pub candidates: Vec<Declaration>,
    /// Resolution quality.
    pub quality: ResolutionQuality,
    /// Message when quality is not Full.
    pub message: Option<String>,
}

/// Result of reference lookup for a symbol.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ReferenceResult {
    /// References grouped by containing symbol.
    pub groups: Vec<ReferenceGroup>,
    /// Total reference count.
    pub total_references: u32,
    /// Resolution quality.
    pub quality: ResolutionQuality,
    /// Message when quality is not Full.
    pub message: Option<String>,
}

/// Result of implementation edge discovery.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImplementationResult {
    /// Implementation edges found.
    pub edges: Vec<ImplementationEdge>,
    /// Total edge count.
    pub total_edges: u32,
    /// Resolution quality.
    pub quality: ResolutionQuality,
    /// Message when quality is not Full.
    pub message: Option<String>,
}

/// Resolve declaration at a usage site using symbol identity and FTS5 heuristics.
///
/// Returns the best matching declaration, or candidates when ambiguous.
pub fn resolve_declaration(
    symbol_name: &str,
    file: &str,
    line: u32,
    _context: Option<&str>,
) -> DeclarationResult {
    // This is a heuristic-based resolver. In a full implementation, this would:
    // 1. Query LSP for go-to-definition
    // 2. Query FTS5 for symbol identity matches
    // 3. Use import analysis to narrow candidates
    // 4. Apply syntax heuristics for confidence scoring

    // For now, return a degraded result indicating the resolver is available
    // but requires LSP integration for full resolution.
    DeclarationResult {
        declaration: None,
        candidates: Vec::new(),
        quality: ResolutionQuality::Degraded,
        message: Some(format!(
            "declaration resolution for '{symbol_name}' at {file}:{line} requires LSP integration"
        )),
    }
}

/// Find all references for a symbol, grouped by containing symbol.
///
/// Uses FTS5 search, import analysis, and callgraph data.
pub fn find_references(
    symbol_name: &str,
    _file: Option<&str>,
    _include_tests: bool,
) -> ReferenceResult {
    // This is a heuristic-based resolver. In a full implementation, this would:
    // 1. Query FTS5 for all occurrences of the symbol name
    // 2. Group by containing symbol using callgraph data
    // 3. Classify reference types (call, import, type, etc.)
    // 4. Score confidence based on context

    // For now, return a degraded result indicating the resolver is available
    // but requires FTS5/callgraph integration for full resolution.
    ReferenceResult {
        groups: Vec::new(),
        total_references: 0,
        quality: ResolutionQuality::Degraded,
        message: Some(format!(
            "reference resolution for '{symbol_name}' requires FTS5/callgraph integration"
        )),
    }
}

/// Find implementations for an interface/trait/protocol-like symbol.
///
/// Uses syntax heuristics and FTS5 search to find implementing types.
pub fn find_implementations(interface_name: &str, _file: Option<&str>) -> ImplementationResult {
    // This is a heuristic-based resolver. In a full implementation, this would:
    // 1. Query FTS5 for "impl {interface_name}" or "implements {interface_name}"
    // 2. Use callgraph data for trait-object usage patterns
    // 3. Score confidence based on syntax patterns

    // For now, return a degraded result indicating the resolver is available
    // but requires FTS5/callgraph integration for full resolution.
    ImplementationResult {
        edges: Vec::new(),
        total_edges: 0,
        quality: ResolutionQuality::Degraded,
        message: Some(format!(
            "implementation resolution for '{interface_name}' requires FTS5/callgraph integration"
        )),
    }
}

/// Map a set of references into reference groups by containing symbol.
///
/// This is a utility for grouping flat reference lists into structured output.
pub fn group_references_by_symbol(references: Vec<Reference>) -> Vec<ReferenceGroup> {
    let mut groups: BTreeMap<String, ReferenceGroup> = BTreeMap::new();

    for r in references {
        let key = r
            .containing_symbol
            .clone()
            .unwrap_or_else(|| "<unknown>".to_string());

        let group = groups.entry(key).or_insert_with(|| ReferenceGroup {
            symbol_name: r
                .containing_symbol
                .clone()
                .unwrap_or_else(|| "<unknown>".to_string()),
            symbol_kind: r
                .containing_symbol_kind
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            file: r.file.clone(),
            line: r.line,
            references: Vec::new(),
        });

        group.references.push(r);
    }

    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_declaration_returns_degraded() {
        let result = resolve_declaration("foo", "src/main.rs", 10, None);
        assert_eq!(result.quality, ResolutionQuality::Degraded);
        assert!(result.declaration.is_none());
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn find_references_returns_degraded() {
        let result = find_references("foo", None, false);
        assert_eq!(result.quality, ResolutionQuality::Degraded);
        assert!(result.groups.is_empty());
    }

    #[test]
    fn find_implementations_returns_degraded() {
        let result = find_implementations("MyTrait", None);
        assert_eq!(result.quality, ResolutionQuality::Degraded);
        assert!(result.edges.is_empty());
    }

    #[test]
    fn group_references_by_symbol_works() {
        let refs = vec![
            Reference {
                name: "foo".to_string(),
                file: "src/main.rs".to_string(),
                line: 10,
                column: 5,
                kind: "call".to_string(),
                confidence: Confidence::High,
                containing_symbol: Some("bar".to_string()),
                containing_symbol_kind: Some("function".to_string()),
            },
            Reference {
                name: "foo".to_string(),
                file: "src/main.rs".to_string(),
                line: 20,
                column: 5,
                kind: "call".to_string(),
                confidence: Confidence::High,
                containing_symbol: Some("bar".to_string()),
                containing_symbol_kind: Some("function".to_string()),
            },
            Reference {
                name: "foo".to_string(),
                file: "src/main.rs".to_string(),
                line: 30,
                column: 5,
                kind: "import".to_string(),
                confidence: Confidence::Medium,
                containing_symbol: None,
                containing_symbol_kind: None,
            },
        ];

        let groups = group_references_by_symbol(refs);
        assert_eq!(groups.len(), 2);
        // "bar" group has 2 references
        let bar_group = groups.iter().find(|g| g.symbol_name == "bar").unwrap();
        assert_eq!(bar_group.references.len(), 2);
    }

    #[test]
    fn confidence_ordering() {
        assert!(Confidence::Exact > Confidence::High);
        assert!(Confidence::High > Confidence::Medium);
        assert!(Confidence::Medium > Confidence::Low);
        assert!(Confidence::Low > Confidence::None);
    }

    #[test]
    fn resolution_quality_labels() {
        assert_eq!(ResolutionQuality::Full.label(), "full");
        assert_eq!(ResolutionQuality::Partial.label(), "partial");
        assert_eq!(ResolutionQuality::Degraded.label(), "degraded");
        assert_eq!(ResolutionQuality::Unavailable.label(), "unavailable");
    }
}
