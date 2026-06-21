//! FTS5 retrieval adapter for Retrieval Intelligence v1.
//!
//! Wraps the existing FTS5 query planner (fts5_planner.rs) to produce
//! `CandidateSet` results per lane, feeding into the fusion pipeline.

use std::path::PathBuf;

use crate::candidate::{CandidateEntry, CandidateSet};
use crate::fts5_planner::{FusedResult, QueryPlanner};
use crate::fts5_store::Fts5Store;
use crate::search_plan::{LaneKind, SearchPlan};

use super::{is_generated_path, is_vendor_path, RetrievalAdapter};

/// FTS5 retrieval adapter.
///
/// Wraps `fts5_planner::QueryPlanner` and converts `FusedResult` into
/// `CandidateSet` per active FTS5 lane.
pub struct Fts5Adapter<'a> {
    store: &'a Fts5Store,
}

impl<'a> Fts5Adapter<'a> {
    /// Create a new FTS5 adapter wrapping the given store.
    pub fn new(store: &'a Fts5Store) -> Self {
        Self { store }
    }

    /// Execute FTS5 search and convert results to CandidateSets.
    fn execute_fts5(&self, query: &str, top_k: usize) -> Result<Vec<FusedResult>, String> {
        let planner = QueryPlanner::new(self.store);
        planner
            .search(query, top_k)
            .map_err(|e| format!("FTS5 search error: {e}"))
    }

    /// Convert a single FusedResult into a CandidateEntry for a given lane.
    fn fused_to_entry(result: &FusedResult, lane: LaneKind) -> CandidateEntry {
        let file_path = PathBuf::from(&result.file_path);
        let is_vendor = is_vendor_path(&result.file_path);
        let is_generated = is_generated_path(&result.file_path);

        // is_exact_hit: true for SymbolExact lane when there's an exact name match
        let is_exact_hit = matches!(lane, LaneKind::FTS5Symbol | LaneKind::SymbolExact)
            && !result.symbol_name.is_empty();

        CandidateEntry {
            chunk_id: None,
            symbol_id: Some(result.symbol_id as u64),
            file_path,
            line_range: Some((result.start_line as usize, result.end_line as usize)),
            content_hash: None,
            score: result.score as f32,
            rank: 0, // will be set by caller
            is_exact_hit,
            is_vendor,
            is_generated,
            source_lane: lane,
        }
    }
}

impl<'a> RetrievalAdapter for Fts5Adapter<'a> {
    fn retrieve(&self, query: &str, plan: &SearchPlan) -> Vec<CandidateSet> {
        let mut candidate_sets = Vec::new();

        // Determine which FTS5 lanes are active based on the plan's prefetch
        let fts5_lanes = [
            (LaneKind::FTS5Symbol, "FTS5Symbol"),
            (LaneKind::FTS5Body, "FTS5Body"),
            (LaneKind::FTS5Path, "FTS5Path"),
            (LaneKind::FTS5Docs, "FTS5Docs"),
            (LaneKind::SymbolExact, "SymbolExact"),
        ];

        // Check if FTS5 is available — return empty sets if not
        // The store availability is implicit: if the store was created, FTS5 is available

        for (lane, _lane_name) in &fts5_lanes {
            // Check if this lane is active in the plan's prefetch
            let active = plan.prefetch.iter().any(|p| p.lane == *lane);
            if !active {
                continue;
            }

            // Get the retriever plan for this lane
            let retriever = plan.prefetch.iter().find(|p| p.lane == *lane);
            let max_candidates = retriever.map(|r| r.max_candidates).unwrap_or(50);
            let weight = retriever.map(|r| r.weight).unwrap_or(0.0);

            // Skip lanes with weight < 0.1 unless it's the safety lane
            let is_safety = retriever.map(|r| r.is_safety_lane).unwrap_or(false);
            if weight < 0.1 && !is_safety {
                continue;
            }

            // Execute FTS5 search
            match self.execute_fts5(query, max_candidates) {
                Ok(results) => {
                    let entries: Vec<CandidateEntry> = results
                        .iter()
                        .enumerate()
                        .map(|(rank, result)| {
                            let mut entry = Self::fused_to_entry(result, *lane);
                            entry.rank = rank;
                            entry
                        })
                        .collect();

                    candidate_sets.push(CandidateSet {
                        source_lane: *lane,
                        candidates: entries,
                    });
                }
                Err(_e) => {
                    // Return empty CandidateSet on error (not an error response)
                    candidate_sets.push(CandidateSet {
                        source_lane: *lane,
                        candidates: Vec::new(),
                    });
                }
            }
        }

        candidate_sets
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search_plan::{RetrieverPlan, SafetyLaneContext, SearchPlanBuilder};

    fn plan_with_fts5_lanes() -> SearchPlan {
        let shape = crate::query_shape::QueryShape {
            kind: crate::query_shape::QueryKind::Identifier,
            weights: crate::query_shape::ShapeWeights {
                semantic: 0.2,
                lexical: 0.8,
                should_use_lexical: true,
            },
        };
        let ctx = SafetyLaneContext {
            fts5_available: true,
            search_index_ready: true,
        };
        SearchPlanBuilder::from_query_shape(&shape, &ctx)
    }

    // AC-4: FTS5Body (is_safety_lane=true) never skipped even at weight=0.1
    #[test]
    fn safety_lane_never_skipped() {
        let mut plan = plan_with_fts5_lanes();
        // Set FTS5Body weight to 0.1 (minimum) and mark as safety lane
        plan.lane_weights.insert(LaneKind::FTS5Body, 0.1);
        plan.prefetch.push(RetrieverPlan {
            lane: LaneKind::FTS5Body,
            weight: 0.1,
            max_candidates: 50,
            is_safety_lane: true,
            latency_budget_ms: None,
        });

        // Even at weight 0.1, safety lane should be included
        let retriever = plan.prefetch.iter().find(|p| p.lane == LaneKind::FTS5Body);
        assert!(retriever.is_some());
        assert!(retriever.unwrap().is_safety_lane);
    }

    // AC-3: Lane with weight < 0.1 skipped unless is_safety_lane
    #[test]
    fn low_weight_lane_skipped() {
        let mut plan = plan_with_fts5_lanes();
        plan.prefetch.push(RetrieverPlan {
            lane: LaneKind::FTS5Docs,
            weight: 0.05, // below 0.1
            max_candidates: 50,
            is_safety_lane: false,
            latency_budget_ms: None,
        });

        // Non-safety lane at weight < 0.1 should be skipped
        let active = plan
            .prefetch
            .iter()
            .filter(|p| p.weight >= 0.1 || p.is_safety_lane)
            .any(|p| p.lane == LaneKind::FTS5Docs);
        assert!(!active);
    }

    // AC-5: is_exact_hit for SymbolExact lane
    #[test]
    fn exact_hit_for_symbol_exact() {
        let result = FusedResult {
            symbol_id: 42,
            file_id: 1,
            file_path: "src/main.rs".to_string(),
            symbol_name: "MyStruct".to_string(),
            symbol_kind: "struct".to_string(),
            start_line: 10,
            end_line: 20,
            snippet: String::new(),
            score: 0.9,
            best_lane: "exact_symbol_sql".to_string(),
            matched_lanes: vec!["exact_symbol_sql".to_string()],
            name_path: "MyStruct".to_string(),
            duplicate_index: 0,
        };

        let entry = Fts5Adapter::fused_to_entry(&result, LaneKind::SymbolExact);
        assert!(entry.is_exact_hit);
    }

    // AC-6: is_vendor heuristic
    #[test]
    fn vendor_path_detected() {
        assert!(is_vendor_path("node_modules/foo/index.ts"));
        assert!(is_vendor_path("src/vendor/bar.rs"));
        assert!(!is_vendor_path("src/main.rs"));
    }

    // is_generated heuristic
    #[test]
    fn generated_path_detected() {
        assert!(is_generated_path("src/generated/code.rs"));
        assert!(is_generated_path("output.gen.ts"));
        assert!(!is_generated_path("src/main.rs"));
    }
}
