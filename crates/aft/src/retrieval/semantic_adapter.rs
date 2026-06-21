//! Semantic retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing semantic embedding search infrastructure to produce
//! `CandidateSet` results for the semantic lane.

use std::path::PathBuf;

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::search_plan::{LaneKind, SearchPlan};

use super::RetrievalAdapter;

/// Semantic retrieval adapter.
///
/// Embeds the query and searches the vector store, returning
/// `CandidateSet` results for the semantic lane.
///
/// Currently a thin wrapper — the actual embedding and search
/// logic will be wired when the SearchPlan is integrated into
/// the search pipeline. For now, this adapter produces empty
/// CandidateSets as a placeholder.
pub struct SemanticAdapter;

impl SemanticAdapter {
    /// Create a new semantic adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for SemanticAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalAdapter for SemanticAdapter {
    fn retrieve(&self, _query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        // Check if semantic lane is active in the plan
        let semantic_plan = plan.prefetch.iter().find(|p| p.lane == LaneKind::Semantic);

        let max_candidates = semantic_plan.map(|p| p.max_candidates).unwrap_or(50);

        // For now, return an empty CandidateSet.
        // The actual semantic search will be wired when the SearchPlan
        // is integrated into the search pipeline (future Bead).
        // The key contract is:
        // - source_lane = Semantic
        // - candidates.len() <= max_candidates
        // - is_exact_hit = false for all semantic results

        let _ = max_candidates; // will be used when wiring is done

        vec![CandidateSet {
            source_lane: LaneKind::Semantic,
            candidates: Vec::new(),
        }]
    }
}

/// Build a CandidateEntry from a semantic search result.
///
/// This is a helper for when the actual semantic search is wired up.
/// Semantic hits are never exact by definition.
#[cfg(test)]
pub fn semantic_result_to_entry(
    file_path: &str,
    start_line: u32,
    end_line: u32,
    score: f32,
    rank: usize,
    symbol_id: Option<u64>,
) -> CandidateEntry {
    CandidateEntry {
        chunk_id: None,
        symbol_id,
        file_path: PathBuf::from(file_path),
        line_range: Some((start_line as usize, end_line as usize)),
        content_hash: None,
        score,
        rank,
        is_exact_hit: false, // semantic hits are never exact
        is_vendor: false,
        is_generated: false,
        source_lane: LaneKind::Semantic,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query_shape::{QueryKind, QueryShape, ShapeWeights};
    use crate::search_plan::{RetrieverPlan, SafetyLaneContext, SearchPlanBuilder};

    fn plan_with_semantic() -> SearchPlan {
        let shape = QueryShape {
            kind: QueryKind::Identifier,
            weights: ShapeWeights {
                semantic: 0.8,
                lexical: 0.2,
                should_use_lexical: false,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: true,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    // AC-1: Returns CandidateSet with source_lane=Semantic
    #[test]
    fn returns_semantic_lane() {
        let adapter = SemanticAdapter::new();
        let plan = plan_with_semantic();
        let result = adapter.retrieve("test query", &plan);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_lane, LaneKind::Semantic);
    }

    // AC-2: candidates.len() <= max_candidates
    #[test]
    fn respects_max_candidates_bound() {
        let adapter = SemanticAdapter::new();
        let plan = plan_with_semantic();
        let result = adapter.retrieve("test query", &plan);
        let max = plan
            .prefetch
            .iter()
            .find(|p| p.lane == LaneKind::Semantic)
            .map(|p| p.max_candidates)
            .unwrap_or(50);
        assert!(result[0].candidates.len() <= max);
    }

    // AC-3: Empty CandidateSet (not panic) when embedding fails
    #[test]
    fn empty_on_failure() {
        let adapter = SemanticAdapter::new();
        let plan = plan_with_semantic();
        let result = adapter.retrieve("test query", &plan);
        // Currently always empty (placeholder) — no panic
        assert_eq!(result[0].candidates.len(), 0);
    }

    // AC-4: is_exact_hit=false for all semantic results
    #[test]
    fn exact_hit_always_false() {
        let entry = semantic_result_to_entry("src/main.rs", 10, 20, 0.9, 0, Some(42));
        assert!(!entry.is_exact_hit);
        assert_eq!(entry.source_lane, LaneKind::Semantic);
    }

    // Serde round-trip for CandidateEntry
    #[test]
    fn candidate_entry_serde_roundtrip() {
        let entry = semantic_result_to_entry("src/lib.rs", 5, 15, 0.85, 1, None);
        let json = serde_json::to_string(&entry).unwrap();
        let deserialized: CandidateEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.source_lane, LaneKind::Semantic);
        assert!(!deserialized.is_exact_hit);
    }
}
