//! Trigram retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing trigram search index (search_index.rs) to produce
//! `CandidateSet` results for the trigram lane.

use std::path::PathBuf;

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::search_plan::{LaneKind, SearchPlan};

use super::{is_generated_path, is_vendor_path, RetrievalAdapter};

/// Trigram retrieval adapter.
///
/// Wraps the trigram search index to produce `CandidateSet` results.
///
/// Currently a thin wrapper — the actual trigram search logic will be
/// wired when the SearchPlan is integrated into the search pipeline.
pub struct TrigramAdapter;

impl TrigramAdapter {
    /// Create a new trigram adapter.
    pub fn new() -> Self {
        Self
    }
}

impl Default for TrigramAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalAdapter for TrigramAdapter {
    fn retrieve(&self, _query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        // Check if trigram lane is active
        let trigram_plan = plan.prefetch.iter().find(|p| p.lane == LaneKind::Trigram);

        let max_candidates = trigram_plan.map(|p| p.max_candidates).unwrap_or(50);

        // For now, return an empty CandidateSet.
        // The actual trigram search will be wired when the SearchPlan
        // is integrated into the search pipeline.
        let _ = max_candidates;

        vec![CandidateSet {
            source_lane: LaneKind::Trigram,
            candidates: Vec::new(),
        }]
    }
}

/// Build a CandidateEntry from a trigram search result.
///
/// is_exact_hit is true when the query exactly matches a symbol or path.
#[cfg(test)]
pub fn trigram_result_to_entry(
    file_path: &str,
    start_line: u32,
    end_line: u32,
    score: f32,
    rank: usize,
    is_exact: bool,
) -> CandidateEntry {
    let path_str = file_path.to_string();
    CandidateEntry {
        chunk_id: None,
        symbol_id: None,
        file_path: PathBuf::from(file_path),
        line_range: Some((start_line as usize, end_line as usize)),
        content_hash: None,
        score,
        rank,
        is_exact_hit: is_exact,
        is_vendor: is_vendor_path(&path_str),
        is_generated: is_generated_path(&path_str),
        source_lane: LaneKind::Trigram,
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

    fn plan_with_trigram() -> SearchPlan {
        let shape = QueryShape {
            kind: QueryKind::Identifier,
            weights: ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: false,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    // AC-1: Returns CandidateSet with source_lane=Trigram
    #[test]
    fn returns_trigram_lane() {
        let adapter = TrigramAdapter::new();
        let plan = plan_with_trigram();
        let result = adapter.retrieve("test_fn", &plan);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_lane, LaneKind::Trigram);
    }

    // Empty on failure (no panic)
    #[test]
    fn empty_on_unavailable() {
        let adapter = TrigramAdapter::new();
        let plan = plan_with_trigram();
        let result = adapter.retrieve("query", &plan);
        assert_eq!(result[0].candidates.len(), 0);
    }

    // is_vendor and is_generated propagated
    #[test]
    fn vendor_generated_propagated() {
        let entry = trigram_result_to_entry("node_modules/foo/index.ts", 1, 10, 0.9, 0, false);
        assert!(entry.is_vendor);
        assert!(!entry.is_generated);
    }

    // Exact hit for literal exact match
    #[test]
    fn exact_hit_for_literal_match() {
        let entry = trigram_result_to_entry("src/main.rs", 1, 10, 0.95, 0, true);
        assert!(entry.is_exact_hit);
    }
}
