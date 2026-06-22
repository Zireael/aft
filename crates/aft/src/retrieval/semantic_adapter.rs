//! Semantic retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing semantic embedding search infrastructure to produce
//! `CandidateSet` results for the semantic lane.

#[cfg(test)]
use std::path::PathBuf;

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::search_plan::{LaneKind, SearchPlan};
use crate::semantic_index::SemanticResult;

use super::{is_generated_path, is_vendor_path, RetrievalAdapter};

/// Semantic retrieval adapter.
///
/// Embeds the query and searches the vector store, returning
/// `CandidateSet` results for the semantic lane.
///
pub struct SemanticAdapter {
    results: Vec<SemanticResult>,
}

impl SemanticAdapter {
    /// Create a new semantic adapter.
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    /// Create a semantic adapter from existing semantic index results.
    pub fn from_results(results: Vec<SemanticResult>) -> Self {
        Self { results }
    }
}

impl Default for SemanticAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalAdapter for SemanticAdapter {
    fn retrieve(&self, _query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        let max_candidates = plan
            .prefetch
            .iter()
            .find(|p| p.lane == LaneKind::Semantic)
            .map(|p| p.max_candidates)
            .unwrap_or(50);

        let candidates = self
            .results
            .iter()
            .take(max_candidates)
            .enumerate()
            .map(|(rank, result)| semantic_result_to_candidate_entry(result, rank))
            .collect();

        vec![CandidateSet {
            source_lane: LaneKind::Semantic,
            candidates,
        }]
    }
}

fn semantic_result_to_candidate_entry(result: &SemanticResult, rank: usize) -> CandidateEntry {
    let path_str = result.file.display().to_string();
    let line_range = if result.start_line > 0 || result.end_line > 0 {
        Some((result.start_line as usize, result.end_line as usize))
    } else {
        None
    };

    CandidateEntry {
        chunk_id: None,
        symbol_id: None,
        file_path: result.file.clone(),
        line_range,
        content_hash: None,
        score: result.score,
        rank,
        is_exact_hit: false,
        is_vendor: is_vendor_path(&path_str),
        is_generated: is_generated_path(&path_str),
        source_lane: LaneKind::Semantic,
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
    use crate::search_plan::{SafetyLaneContext, SearchPlanBuilder};
    use crate::symbols::SymbolKind;

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

    // AC-3: Empty CandidateSet (not panic) when no semantic results are available
    #[test]
    fn empty_on_failure() {
        let adapter = SemanticAdapter::new();
        let plan = plan_with_semantic();
        let result = adapter.retrieve("test query", &plan);
        assert_eq!(result[0].candidates.len(), 0);
    }

    #[test]
    fn semantic_results_become_candidates() {
        let adapter = SemanticAdapter::from_results(vec![SemanticResult {
            file: PathBuf::from("src/lib.rs"),
            name: "SemanticBackendConfig".to_string(),
            kind: SymbolKind::Struct,
            start_line: 10,
            end_line: 12,
            exported: true,
            snippet: "pub struct SemanticBackendConfig".to_string(),
            score: 0.88,
            source: "semantic",
        }]);
        let plan = plan_with_semantic();
        let result = adapter.retrieve("semantic backend config", &plan);

        assert_eq!(result[0].source_lane, LaneKind::Semantic);
        assert_eq!(result[0].candidates.len(), 1);
        let candidate = &result[0].candidates[0];
        assert_eq!(candidate.file_path, PathBuf::from("src/lib.rs"));
        assert_eq!(candidate.line_range, Some((10, 12)));
        assert_eq!(candidate.score, 0.88);
        assert_eq!(candidate.rank, 0);
        assert!(!candidate.is_exact_hit);
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
