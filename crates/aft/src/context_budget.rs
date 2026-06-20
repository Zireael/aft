//! Context budget model for Retrieval Intelligence v1.
//!
//! Defines ContextBudget, ContextMode, EnrichPool, and ContextBudgetResult per §A.2.
//! Implements exhaustion simulation with PathOnly fallback and reranker-skip logic.
//!
//! Key invariants (WARNING 8):
//! - PathOnly candidates are EXCLUDED from content reranker input.
//! - Reranker is skipped when enriched_count/rerank_pool_size < rerank_min_enriched_ratio.
//! - Zero enriched candidates: skip reranker unconditionally.

use std::path::PathBuf;

// ---------------------------------------------------------------------------
// ContextMode — how candidate content is retrieved
// ---------------------------------------------------------------------------

/// How candidate content is retrieved for enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ContextMode {
    /// Only file path and line range — no content read.
    PathOnly,
    /// Symbol signature (first line / header).
    Signature,
    /// Full symbol body.
    SymbolBody,
    /// Symbol body plus doc comments.
    SymbolBodyWithDocs,
    /// Specific line window from file.
    LineWindow,
    /// File outline (struct/function signatures).
    FileOutline,
    /// Automatic mode based on intent and budget.
    Auto,
}

// ---------------------------------------------------------------------------
// EnrichPool — which pool to enrich before reranking
// ---------------------------------------------------------------------------

/// Which pool of candidates to enrich with content before reranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EnrichPool {
    /// Enrich only the final top-k results.
    FinalTopK,
    /// Enrich the fusion pool (pre-rerank).
    FusionPool,
    /// Enrich the full rerank pool (recommended when rerank enabled).
    RerankPool,
}

// ---------------------------------------------------------------------------
// ContextBudget — configuration
// ---------------------------------------------------------------------------

/// Context budget configuration per §A.2.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextBudget {
    /// Total token budget for all candidates (default 4000).
    pub total_tokens: usize,
    /// Tokens per candidate (default 300).
    pub per_candidate_tokens: usize,
    /// Minimum candidate characters to include.
    pub min_candidate_chars: usize,
    /// How to retrieve candidate content.
    pub mode: ContextMode,
    /// Which pool to enrich before reranking.
    pub enrich_pool: EnrichPool,
    /// Minimum enriched ratio (enriched/pool) to run reranker.
    /// Below this: skip reranker, emit reranker_skipped_reason.
    pub rerank_min_enriched_ratio: f32,
}

// ---------------------------------------------------------------------------
// ContextBudgetResult — outcome of budget allocation
// ---------------------------------------------------------------------------

/// Result of applying context budget to a candidate pool.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ContextBudgetResult {
    /// Whether the budget was exhausted before all candidates could be enriched.
    pub context_exhausted: bool,
    /// Number of candidates that fell back to PathOnly.
    pub unenriched_candidate_count: usize,
    /// Reason the reranker was skipped (if applicable).
    pub reranker_skipped_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Profile presets
// ---------------------------------------------------------------------------

impl ContextBudget {
    /// Agent fast profile: balanced speed and quality.
    pub fn agent_fast() -> Self {
        Self {
            total_tokens: 4000,
            per_candidate_tokens: 300,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::FusionPool,
            rerank_min_enriched_ratio: 0.5,
        }
    }

    /// Symbol exact profile: precise symbol lookup with full signature.
    pub fn symbol_exact() -> Self {
        Self {
            total_tokens: 2000,
            per_candidate_tokens: 500,
            min_candidate_chars: 80,
            mode: ContextMode::Signature,
            enrich_pool: EnrichPool::FinalTopK,
            rerank_min_enriched_ratio: 0.5,
        }
    }

    /// Agent deep profile: thorough analysis with large budget.
    pub fn agent_deep() -> Self {
        Self {
            total_tokens: 12000,
            per_candidate_tokens: 500,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::RerankPool,
            rerank_min_enriched_ratio: 0.5,
        }
    }
}

// ---------------------------------------------------------------------------
// Budget simulation
// ---------------------------------------------------------------------------

/// Simulate budget allocation over a pool of candidates.
///
/// Candidates are assumed to be pre-sorted by rank (highest first).
/// Each candidate needs `per_candidate_tokens` tokens to be enriched.
/// Candidates that exceed the remaining budget get PathOnly fallback.
///
/// Returns the ContextBudgetResult with exhaustion status and counts.
pub fn simulate_budget(budget: &ContextBudget, pool_size: usize) -> ContextBudgetResult {
    let max_enriched = budget.total_tokens / budget.per_candidate_tokens;
    let enriched_count = pool_size.min(max_enriched);
    let unenriched_count = pool_size.saturating_sub(enriched_count);
    let context_exhausted = unenriched_count > 0;

    // Check reranker skip condition
    let reranker_skipped_reason = if pool_size == 0 {
        Some("no_candidates".to_string())
    } else if enriched_count == 0 {
        Some("no_enriched_candidates".to_string())
    } else {
        let ratio = enriched_count as f32 / pool_size as f32;
        if ratio < budget.rerank_min_enriched_ratio {
            Some("insufficient_enriched_ratio".to_string())
        } else {
            None
        }
    };

    ContextBudgetResult {
        context_exhausted,
        unenriched_candidate_count: unenriched_count,
        reranker_skipped_reason,
    }
}

/// Generate a PathOnly fallback string for a candidate.
///
/// Format: "file_path:line_range [budget_exhausted]" + optional signature.
pub fn path_only_fallback(file_path: &PathBuf, line_range: Option<(usize, usize)>) -> String {
    match line_range {
        Some((start, end)) => format!(
            "{}:{}-{} [budget_exhausted]",
            file_path.display(),
            start,
            end
        ),
        None => format!("{} [budget_exhausted]", file_path.display()),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // AC-2: agent_fast profile
    #[test]
    fn agent_fast_profile() {
        let budget = ContextBudget::agent_fast();
        assert_eq!(budget.total_tokens, 4000);
        assert_eq!(budget.per_candidate_tokens, 300);
        assert_eq!(budget.enrich_pool, EnrichPool::FusionPool);
    }

    // AC-3: symbol_exact profile
    #[test]
    fn symbol_exact_profile() {
        let budget = ContextBudget::symbol_exact();
        assert_eq!(budget.mode, ContextMode::Signature);
        assert_eq!(budget.enrich_pool, EnrichPool::FinalTopK);
    }

    // AC-4: agent_deep profile
    #[test]
    fn agent_deep_profile() {
        let budget = ContextBudget::agent_deep();
        assert_eq!(budget.enrich_pool, EnrichPool::RerankPool);
        assert_eq!(budget.total_tokens, 12000);
    }

    // AC-5: Exhaustion simulation
    #[test]
    fn exhaustion_sim_budget_exceeded() {
        let budget = ContextBudget {
            total_tokens: 400,
            per_candidate_tokens: 200,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::RerankPool,
            rerank_min_enriched_ratio: 0.5,
        };
        let result = simulate_budget(&budget, 5);
        // 400 / 200 = 2 enriched, 3 unenriched
        assert!(result.context_exhausted, "budget should be exhausted");
        assert_eq!(result.unenriched_candidate_count, 3);
        // 2/5 = 0.4 < 0.5 → reranker skipped
        assert_eq!(
            result.reranker_skipped_reason.as_deref(),
            Some("insufficient_enriched_ratio")
        );
    }

    // AC-5: PathOnly fallback is non-empty
    #[test]
    fn path_only_fallback_non_empty() {
        let fallback = path_only_fallback(&PathBuf::from("src/main.rs"), Some((10, 20)));
        assert!(!fallback.is_empty());
        assert!(fallback.contains("src/main.rs"));
        assert!(fallback.contains("10-20"));
        assert!(fallback.contains("[budget_exhausted]"));
    }

    // AC-6: reranker_skipped_reason when enriched/pool < 0.5
    #[test]
    fn reranker_skipped_insufficient_ratio() {
        let budget = ContextBudget {
            total_tokens: 500,
            per_candidate_tokens: 100,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::RerankPool,
            rerank_min_enriched_ratio: 0.5,
        };
        // 500/100 = 5 enriched, pool=11 → 5/11 ≈ 0.45 < 0.5
        let result = simulate_budget(&budget, 11);
        assert_eq!(
            result.reranker_skipped_reason.as_deref(),
            Some("insufficient_enriched_ratio")
        );
    }

    // No enriched candidates → skip reranker unconditionally
    #[test]
    fn reranker_skipped_zero_enriched() {
        let budget = ContextBudget {
            total_tokens: 0,
            per_candidate_tokens: 100,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::RerankPool,
            rerank_min_enriched_ratio: 0.5,
        };
        let result = simulate_budget(&budget, 5);
        assert_eq!(
            result.reranker_skipped_reason.as_deref(),
            Some("no_enriched_candidates")
        );
    }

    // Empty pool
    #[test]
    fn empty_pool() {
        let budget = ContextBudget::agent_fast();
        let result = simulate_budget(&budget, 0);
        assert!(!result.context_exhausted);
        assert_eq!(result.unenriched_candidate_count, 0);
        assert_eq!(
            result.reranker_skipped_reason.as_deref(),
            Some("no_candidates")
        );
    }

    // Sufficient ratio → no skip
    #[test]
    fn sufficient_ratio_no_skip() {
        let budget = ContextBudget {
            total_tokens: 1000,
            per_candidate_tokens: 100,
            min_candidate_chars: 80,
            mode: ContextMode::Auto,
            enrich_pool: EnrichPool::RerankPool,
            rerank_min_enriched_ratio: 0.5,
        };
        // 1000/100 = 10 enriched, pool=10 → 10/10 = 1.0 >= 0.5
        let result = simulate_budget(&budget, 10);
        assert!(!result.context_exhausted);
        assert_eq!(result.reranker_skipped_reason, None);
    }

    // PathOnly fallback without line range
    #[test]
    fn path_only_fallback_no_line_range() {
        let fallback = path_only_fallback(&PathBuf::from("src/lib.rs"), None);
        assert!(fallback.contains("src/lib.rs"));
        assert!(fallback.contains("[budget_exhausted]"));
        assert!(!fallback.contains(":"));
    }

    // Serde round-trip
    #[test]
    fn context_budget_serde_roundtrip() {
        let budget = ContextBudget::agent_fast();
        let json = serde_json::to_string(&budget).unwrap();
        let deserialized: ContextBudget = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total_tokens, budget.total_tokens);
        assert_eq!(deserialized.enrich_pool, budget.enrich_pool);
    }

    #[test]
    fn context_budget_result_serde_roundtrip() {
        let result = ContextBudgetResult {
            context_exhausted: true,
            unenriched_candidate_count: 3,
            reranker_skipped_reason: Some("insufficient_enriched_ratio".to_string()),
        };
        let json = serde_json::to_string(&result).unwrap();
        let deserialized: ContextBudgetResult = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.context_exhausted, true);
        assert_eq!(deserialized.unenriched_candidate_count, 3);
    }
}
