//! Settings kill switches, documentation, and user-facing configuration.
//!
//! Exposes safe configuration for the new intelligence layers: FTS5, hybrid ranking,
//! chunk retrieval, graph, mutation risk, verify, context economy, and symbolic refactor.
//! All new subsystems have config toggles with defaults that preserve exact behavior.

/// Configuration for AFT intelligence subsystems.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct IntelligenceConfig {
    /// FTS5 full-text search configuration.
    pub fts5: Fts5Config,
    /// Hybrid ranking configuration.
    pub hybrid_ranking: HybridRankingConfig,
    /// Chunk retrieval configuration.
    pub chunk_retrieval: ChunkRetrievalConfig,
    /// Graph/callgraph configuration.
    pub graph: GraphConfig,
    /// Mutation risk classifier configuration.
    pub mutation_risk: MutationRiskConfig,
    /// Verify workflow configuration.
    pub verify: VerifyConfig,
    /// Context economy configuration.
    pub context_economy: ContextEconomyConfig,
    /// Symbolic refactor configuration.
    pub symbolic_refactor: SymbolicRefactorConfig,
    /// Retrieval Intelligence v2 feature flag. When true, SearchPlan is built
    /// and search_plan_debug is added to NDJSON responses. Default: false.
    #[serde(default)]
    pub retrieval_intelligence_v2: bool,
}

/// FTS5 full-text search configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Fts5Config {
    /// Enable FTS5 indexing and search.
    pub enabled: bool,
    /// Auto-index on file changes.
    pub auto_index: bool,
    /// Index on start.
    pub index_on_start: bool,
    /// Maximum results per query.
    pub max_results: usize,
    /// Maximum body characters in results.
    pub max_body_chars: usize,
    /// Maximum body lines in results.
    pub max_body_lines: usize,
    /// Enable raw FTS debug output.
    pub raw_fts_debug: bool,
}

impl Default for Fts5Config {
    fn default() -> Self {
        Self {
            enabled: false,
            auto_index: false,
            index_on_start: false,
            max_results: 20,
            max_body_chars: 2000,
            max_body_lines: 60,
            raw_fts_debug: false,
        }
    }
}

/// Hybrid ranking configuration (lexical + semantic).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HybridRankingConfig {
    /// Enable hybrid ranking (lexical + semantic fusion).
    pub enabled: bool,
    /// Weight for lexical results (0.0–1.0).
    pub lexical_weight: f64,
    /// Weight for semantic results (0.0–1.0).
    pub semantic_weight: f64,
    /// Enable reranking of results.
    pub rerank_enabled: bool,
    /// Maximum candidates for reranking.
    pub rerank_max_candidates: usize,
}

impl Default for HybridRankingConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            lexical_weight: 0.5,
            semantic_weight: 0.5,
            rerank_enabled: false,
            rerank_max_candidates: 20,
        }
    }
}

/// Chunk retrieval configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChunkRetrievalConfig {
    /// Enable chunked retrieval for large files.
    pub enabled: bool,
    /// Maximum characters per chunk.
    pub max_chunk_chars: usize,
    /// Overlap between chunks.
    pub chunk_overlap_chars: usize,
    /// Maximum chunks per file.
    pub max_chunks_per_file: usize,
}

impl Default for ChunkRetrievalConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chunk_chars: 4000,
            chunk_overlap_chars: 200,
            max_chunks_per_file: 10,
        }
    }
}

/// Graph/callgraph configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GraphConfig {
    /// Enable callgraph computation and navigation.
    pub enabled: bool,
    /// Enable lazy indexing on first query.
    pub lazy_index: bool,
    /// Enable SQLite persistence.
    pub persist: bool,
    /// Maximum files to index.
    pub max_files: usize,
}

impl Default for GraphConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            lazy_index: true,
            persist: true,
            max_files: 10000,
        }
    }
}

/// Mutation risk classifier configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MutationRiskConfig {
    /// Enable mutation risk assessment.
    pub enabled: bool,
    /// Threshold for high-risk warnings (0.0–1.0).
    pub high_risk_threshold: f64,
    /// Threshold for critical-risk blocks (0.0–1.0).
    pub critical_risk_threshold: f64,
}

impl Default for MutationRiskConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            high_risk_threshold: 0.7,
            critical_risk_threshold: 0.9,
        }
    }
}

/// Verify workflow configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyConfig {
    /// Enable verify suggest mode.
    pub enabled: bool,
    /// Include diagnostics in suggestions.
    pub include_diagnostics: bool,
    /// Include likely test suggestions.
    pub include_likely_tests: bool,
    /// Include lint/typecheck suggestions.
    pub include_lint_suggestions: bool,
}

impl Default for VerifyConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            include_diagnostics: true,
            include_likely_tests: true,
            include_lint_suggestions: true,
        }
    }
}

/// Context economy configuration.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContextEconomyConfig {
    /// Enable context savings tracking.
    pub enabled: bool,
    /// Enable sidecar generation (outline, imports, tests).
    pub sidecar_enabled: bool,
    /// Enable unchanged reread summaries.
    pub reread_summary_enabled: bool,
    /// Enable failed edit recovery context.
    pub edit_recovery_enabled: bool,
}

/// Symbolic refactor configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SymbolicRefactorConfig {
    /// Enable symbolic rename/delete operations.
    pub enabled: bool,
    /// Require dry-run before apply.
    pub require_dry_run: bool,
    /// Block deletion when references exist.
    pub block_on_references: bool,
    /// Minimum confidence for auto-apply.
    pub min_confidence_for_apply: String,
}

impl Default for SymbolicRefactorConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            require_dry_run: true,
            block_on_references: true,
            min_confidence_for_apply: "high".to_string(),
        }
    }
}

/// Validate an intelligence configuration.
///
/// Returns a list of validation errors (empty if valid).
pub fn validate_config(config: &IntelligenceConfig) -> Vec<String> {
    let mut errors = Vec::new();

    // Validate FTS5
    if config.fts5.max_results == 0 {
        errors.push("fts5.max_results must be > 0".to_string());
    }
    if config.fts5.max_body_chars == 0 {
        errors.push("fts5.max_body_chars must be > 0".to_string());
    }

    // Validate hybrid ranking
    if config.hybrid_ranking.lexical_weight + config.hybrid_ranking.semantic_weight > 1.01 {
        errors.push("hybrid_ranking: lexical_weight + semantic_weight must be <= 1.0".to_string());
    }
    if config.hybrid_ranking.lexical_weight < 0.0 || config.hybrid_ranking.lexical_weight > 1.0 {
        errors.push("hybrid_ranking.lexical_weight must be 0.0–1.0".to_string());
    }

    // Validate chunk retrieval
    if config.chunk_retrieval.max_chunk_chars == 0 {
        errors.push("chunk_retrieval.max_chunk_chars must be > 0".to_string());
    }

    // Validate graph
    if config.graph.max_files == 0 {
        errors.push("graph.max_files must be > 0".to_string());
    }

    // Validate mutation risk
    if config.mutation_risk.high_risk_threshold > config.mutation_risk.critical_risk_threshold {
        errors.push(
            "mutation_risk: high_risk_threshold must be <= critical_risk_threshold".to_string(),
        );
    }

    // Validate symbolic refactor
    let valid_confidence = ["exact", "high", "medium", "low", "none"];
    if !valid_confidence.contains(&config.symbolic_refactor.min_confidence_for_apply.as_str()) {
        errors.push(format!(
            "symbolic_refactor.min_confidence_for_apply must be one of: {}",
            valid_confidence.join(", ")
        ));
    }

    errors
}

/// Generate a human-readable documentation string for the configuration.
#[allow(clippy::vec_init_then_push)]
pub fn config_docs() -> String {
    let mut docs = Vec::new();

    docs.push("# AFT Intelligence Configuration".to_string());
    docs.push("".to_string());
    docs.push("All new subsystems have config toggles with safe defaults.".to_string());
    docs.push(
        "Defaults preserve exact behavior and avoid mandatory graph/semantic dependencies."
            .to_string(),
    );
    docs.push("".to_string());

    docs.push("## FTS5 Full-Text Search".to_string());
    docs.push("- `fts5.enabled`: Enable FTS5 indexing and search (default: false)".to_string());
    docs.push("- `fts5.auto_index`: Auto-index on file changes (default: false)".to_string());
    docs.push("- `fts5.max_results`: Maximum results per query (default: 20)".to_string());
    docs.push("".to_string());

    docs.push("## Hybrid Ranking".to_string());
    docs.push(
        "- `hybrid_ranking.enabled`: Enable hybrid lexical+semantic ranking (default: false)"
            .to_string(),
    );
    docs.push(
        "- `hybrid_ranking.lexical_weight`: Weight for lexical results (default: 0.5)".to_string(),
    );
    docs.push(
        "- `hybrid_ranking.semantic_weight`: Weight for semantic results (default: 0.5)"
            .to_string(),
    );
    docs.push("- `hybrid_ranking.rerank_enabled`: Enable reranking (default: false)".to_string());
    docs.push("".to_string());

    docs.push("## Graph/Callgraph".to_string());
    docs.push("- `graph.enabled`: Enable callgraph navigation (default: true)".to_string());
    docs.push("- `graph.lazy_index`: Index on first query (default: true)".to_string());
    docs.push("- `graph.persist`: SQLite persistence (default: true)".to_string());
    docs.push("".to_string());

    docs.push("## Mutation Risk".to_string());
    docs.push("- `mutation_risk.enabled`: Enable risk assessment (default: true)".to_string());
    docs.push(
        "- `mutation_risk.high_risk_threshold`: High-risk warning threshold (default: 0.7)"
            .to_string(),
    );
    docs.push(
        "- `mutation_risk.critical_risk_threshold`: Critical-risk block threshold (default: 0.9)"
            .to_string(),
    );
    docs.push("".to_string());

    docs.push("## Verify".to_string());
    docs.push("- `verify.enabled`: Enable verify suggest mode (default: true)".to_string());
    docs.push("- `verify.include_diagnostics`: Include diagnostics (default: true)".to_string());
    docs.push(
        "- `verify.include_likely_tests`: Include test suggestions (default: true)".to_string(),
    );
    docs.push("".to_string());

    docs.push("## Context Economy".to_string());
    docs.push(
        "- `context_economy.enabled`: Enable context savings tracking (default: false)".to_string(),
    );
    docs.push(
        "- `context_economy.sidecar_enabled`: Generate sidecars (default: false)".to_string(),
    );
    docs.push(
        "- `context_economy.reread_summary_enabled`: Unchanged reread summaries (default: false)"
            .to_string(),
    );
    docs.push("".to_string());

    docs.push("## Symbolic Refactor".to_string());
    docs.push(
        "- `symbolic_refactor.enabled`: Enable symbolic rename/delete (default: false)".to_string(),
    );
    docs.push(
        "- `symbolic_refactor.require_dry_run`: Require dry-run before apply (default: true)"
            .to_string(),
    );
    docs.push("- `symbolic_refactor.block_on_references`: Block deletion when references exist (default: true)".to_string());

    docs.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_valid() {
        let config = IntelligenceConfig::default();
        let errors = validate_config(&config);
        assert!(
            errors.is_empty(),
            "default config should be valid: {:?}",
            errors
        );
    }

    #[test]
    fn fts5_disabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(!config.fts5.enabled);
    }

    #[test]
    fn hybrid_ranking_disabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(!config.hybrid_ranking.enabled);
    }

    #[test]
    fn context_economy_disabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(!config.context_economy.enabled);
    }

    #[test]
    fn symbolic_refactor_disabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(!config.symbolic_refactor.enabled);
    }

    #[test]
    fn graph_enabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(config.graph.enabled);
    }

    #[test]
    fn mutation_risk_enabled_by_default() {
        let config = IntelligenceConfig::default();
        assert!(config.mutation_risk.enabled);
    }

    #[test]
    fn validate_catches_invalid_weights() {
        let mut config = IntelligenceConfig::default();
        config.hybrid_ranking.lexical_weight = 0.8;
        config.hybrid_ranking.semantic_weight = 0.8;
        let errors = validate_config(&config);
        assert!(!errors.is_empty());
        assert!(errors.iter().any(|e| e.contains("weight")));
    }

    #[test]
    fn validate_catches_zero_max_results() {
        let mut config = IntelligenceConfig::default();
        config.fts5.max_results = 0;
        let errors = validate_config(&config);
        assert!(!errors.is_empty());
    }

    #[test]
    fn validate_catches_invalid_confidence() {
        let mut config = IntelligenceConfig::default();
        config.symbolic_refactor.min_confidence_for_apply = "invalid".to_string();
        let errors = validate_config(&config);
        assert!(!errors.is_empty());
    }

    #[test]
    fn config_docs_not_empty() {
        let docs = config_docs();
        assert!(!docs.is_empty());
        assert!(docs.contains("FTS5"));
        assert!(docs.contains("Hybrid Ranking"));
    }
}
